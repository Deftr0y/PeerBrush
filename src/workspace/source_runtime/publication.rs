//! Durable pre-create external debt. Transport never resumes a file writer.
use super::{Directory, HeldFile, Inventory, RootOperation, SourceRuntime, SourceSession};
use crate::{
    file_safety::{owned_rename, NativeExternalSnapshot},
    large_document::{ExternalFact, PublicationTicket, ScratchBytes},
    psd::{
        memory36::{Admitted, MetadataCharge},
        save36::CompletedSave36,
    },
};
use ring::digest::{digest, Context, SHA256};
use std::{
    ffi::OsString,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::{atomic::Ordering, Arc},
};
#[path = "publication/chain.rs"]
mod chain;
#[path = "publication/closed_frame.rs"]
mod closed_frame;
#[path = "publication/creation.rs"]
mod creation;
#[path = "publication/creation_frame.rs"]
mod creation_frame;
#[path = "publication/frame.rs"]
mod frame;
#[path = "publication/settlement.rs"]
mod settlement;
pub(crate) use creation::RegisteredCreation;
use frame::{Expected, IntentFrame, NamedFact, Role};

pub(super) struct Registry {
    entries: Vec<Registered>,
    maximum: usize,
}
struct Registered {
    frame: Admitted<IntentFrame>,
    sha256: [u8; 32],
    file: Option<(u64, u64)>,
    closed: Option<ClosedRegistration>,
    closing: Option<(u64, u64)>,
    created: [Option<creation::Created>; 2],
}
struct ClosedRegistration {
    frame: closed_frame::ChargedClosed,
    sha256: [u8; 32],
    file: (u64, u64),
}
impl Registry {
    pub(super) fn new(maximum: usize) -> Result<Self, String> {
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(maximum)
            .map_err(|_| "External journal registry allocation refused")?;
        if entries
            .capacity()
            .checked_mul(std::mem::size_of::<Registered>())
            .is_none_or(|n| n > maximum.saturating_mul(8192))
        {
            return Err("External journal registry exceeds inventory admission".into());
        }
        Ok(Self { entries, maximum })
    }
    fn insert(
        &mut self,
        frame: Admitted<IntentFrame>,
        sha256: [u8; 32],
        file: Option<(u64, u64)>,
    ) -> Result<(), String> {
        if self.entries.len() >= self.maximum
            || self.entries.len() == self.entries.capacity()
            || self
                .entries
                .iter()
                .any(|r| r.frame.id == frame.id || r.sha256 == sha256)
        {
            return Err("External journal registration is full or ambiguous; debt retained".into());
        }
        self.entries.push(Registered {
            frame,
            sha256,
            file,
            closed: None,
            closing: None,
            created: [None, None],
        });
        Ok(())
    }
}
/// A live exact-operation pre-create receipt. No serialized record constructs it.
/// Drop retains the target lock, physical files and the entire reserved debt.
#[must_use]
pub(crate) struct PublicationIntent<'a> {
    session: &'a SourceSession,
    frame: Admitted<IntentFrame>,
    sha256: [u8; 32],
    ticket: PublicationTicket,
    readback: std::sync::Mutex<ScratchBytes>,
}
pub(crate) struct ArmedPublication<'a> {
    intent: PublicationIntent<'a>,
    prepared: crate::saving::PreparedCommit,
    closing: settlement::Closing,
}
pub(crate) struct CommittedPublication<'a> {
    intent: PublicationIntent<'a>,
    permit: crate::server::FinalizationPermit,
    closing: Option<settlement::Closing>,
}
/// Immutable live-minted binding, independently owned across the Engine fence.
/// It grants no I/O without the separately retained lease/candidate/session.
#[derive(Clone)]
pub(crate) struct PublicationPlan {
    frame: Admitted<IntentFrame>,
    sha256: [u8; 32],
    expected: Option<crate::file_safety::Version>,
    runtime: std::sync::Weak<SourceRuntime>,
    resident: crate::large_document::ResidentBudget,
    disk: crate::large_document::DiskBudget,
}
impl<'a> PublicationIntent<'a> {
    /// Quota-only abort completion under ordinary admission. The caller may
    /// ignore a canceled gesture, but never the source session's closing gate.
    pub(crate) fn abort(
        self,
        proof: &dyn crate::file_safety::ExternalOutcomeProof,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let closing = settlement::Closing::prepare(&self, check)?;
        closing.finish(&self, proof, &|| {
            check()?;
            self.check()
        })
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.session.check()?;
        self.check_record()
    }
    fn check_record(&self) -> Result<(), String> {
        if self.frame.operation != self.session.id
            || !self.ticket.uses_budget(&self.session.disk())
            || self.ticket.intent() != self.sha256
            || self.ticket.target() != self.frame.target_key
            || self.ticket.growth() != self.frame.growth
        {
            return Err("External intent belongs to another operation or quota pool".into());
        }
        let mut scratch = self
            .readback
            .try_lock()
            .map_err(|_| "External intent readback is busy")?;
        // Contention is retryable; it must not latch a corruption diagnosis.
        let state = self
            .session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let result = (|| {
            let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
            let entry = inventory
                .publications
                .entries
                .iter()
                .find(|r| r.sha256 == self.sha256)
                .ok_or("External intent is outside the held journal")?;
            let held = inventory
                .files
                .iter()
                .find(|f| Some(f.identity) == entry.file)
                .ok_or("External intent has no completed held record")?;
            if entry.frame.id != self.frame.id || self.frame.owner != inventory.owner {
                return Err("External intent registration changed; debt retained".into());
            }
            held.check()?;
            let mut file = &held.file;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| "Cannot read held external intent")?;
            let mut hash = Context::new(&SHA256);
            let mut remaining = held.bytes;
            while remaining != 0 {
                let count = remaining.min(scratch.len() as u64) as usize;
                file.read_exact(&mut scratch[..count])
                    .map_err(|_| "Held external intent is partial; debt retained")?;
                hash.update(&scratch[..count]);
                remaining -= count as u64;
            }
            if hash.finish().as_ref() != self.sha256 {
                return Err("Held external intent SHA changed; debt retained".into());
            }
            held.check()
        })();
        if result.is_err() {
            self.session.runtime.tainted.store(true, Ordering::Release);
        }
        result
    }
    pub(crate) fn check_observation(
        &self,
        observed: &NativeExternalSnapshot<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        check()?;
        self.check()?;
        let expected = observed.expected().map(|v| Expected {
            identity: v.identity,
            bytes: v.bytes,
            modified: v.modified_nanos(),
            sha256: v.sha256(),
        });
        if !observed.uses_budget(&self.session.resident())
            || observed.target() != self.frame.target
            || observed.destination_parent_identity() != self.frame.parent
            || observed.backup_directory() != self.frame.backups
            || observed.backup_directory_identity() != self.frame.backups_identity
            || expected != self.frame.expected
            || observed.backups().len() + usize::from(observed.source().is_some())
                != self.frame.facts.len()
        {
            return Err("External inventory differs from the durable intent".into());
        }
        for (name, fact) in observed.backups() {
            let at = self
                .frame
                .facts
                .binary_search_by_key(&fact.identity(), |e| e.fact.identity)
                .map_err(|_| "External backup is absent from the durable intent")?;
            let old = &self.frame.facts[at];
            if old.role != Role::Backup || old.name != name || old.fact != convert(fact) {
                return Err("External backup differs from the durable intent".into());
            }
        }
        observed.revalidate(check)?;
        self.check()
    }
    pub(crate) fn plan(
        &self,
        observed: &NativeExternalSnapshot<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<PublicationPlan, String> {
        self.check_observation(observed, check)?;
        Ok(PublicationPlan {
            frame: self.frame.clone(),
            sha256: self.sha256,
            expected: observed.expected(),
            runtime: Arc::downgrade(&self.session.runtime),
            resident: self.session.resident(),
            disk: self.session.disk(),
        })
    }
    /// The save writer is already fully staged. Root still must check its Engine
    /// save identity, source revision/path/version and dirty-state fence before
    /// consuming the armed request. No closing permit exists during this I/O.
    pub(crate) fn prearm(
        self,
        candidate: &CompletedSave36<'_>,
        control: &crate::saving::Control,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<ArmedPublication<'a>, String> {
        check()?;
        self.check()?;
        candidate.check_session(self.session)?;
        candidate.recheck(check)?;
        if candidate.bytes() != self.frame.candidate_bytes
            || candidate.sha256() != self.frame.candidate_sha256
            || candidate.provenance() != self.frame.provenance
            || candidate.core_sha256() != self.frame.core
            || candidate.scene().render_key()? != self.frame.render
        {
            return Err("Publication was prepared for another immutable candidate".into());
        }
        let identity = candidate.scene().identity();
        let prepared = control.prepare_commit(&identity.document_id, identity.revision)?;
        let closing = settlement::Closing::prepare(&self, check)?;
        check()?;
        self.check()?;
        Ok(ArmedPublication {
            intent: self,
            prepared,
            closing,
        })
    }
    pub(crate) fn id(&self) -> [u8; 16] {
        self.frame.id
    }
    pub(crate) fn sha256(&self) -> [u8; 32] {
        self.sha256
    }
    pub(crate) fn target(&self) -> &Path {
        &self.frame.target
    }
    pub(crate) fn temporary_name(&self) -> &std::ffi::OsStr {
        &self.frame.temporary
    }
    pub(crate) fn backup_name(&self) -> Option<&std::ffi::OsStr> {
        self.frame.backup.as_deref()
    }
    pub(crate) fn candidate_bytes(&self) -> u64 {
        self.frame.candidate_bytes
    }
    pub(crate) fn candidate_sha256(&self) -> [u8; 32] {
        self.frame.candidate_sha256
    }
}
impl<'a> ArmedPublication<'a> {
    pub(crate) fn abort(
        self,
        proof: &dyn crate::file_safety::ExternalOutcomeProof,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.closing.finish(&self.intent, proof, &|| {
            check()?;
            self.intent.check()
        })
    }
    /// Success performs no filesystem I/O or allocation. Root invokes this only
    /// inside the final Engine source/save fence; both this value and its saving
    /// preparation are consumed, so it cannot mint a second committed permit.
    pub(crate) fn commit(
        self,
        control: &crate::saving::Control,
    ) -> Result<CommittedPublication<'a>, &'static str> {
        let permit = self.intent.session.operation.commit_finalization(
            self.intent.sha256,
            control,
            self.prepared,
        )?;
        Ok(CommittedPublication {
            intent: self.intent,
            permit,
            closing: Some(self.closing),
        })
    }
}
impl CommittedPublication<'_> {
    pub(crate) fn finish(
        mut self,
        proof: &dyn crate::file_safety::ExternalOutcomeProof,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let closing = self
            .closing
            .take()
            .ok_or("Committed settlement already consumed")?;
        closing.finish(&self.intent, proof, &|| {
            check()?;
            self.check()
        })
    }
    pub(crate) fn check(&self) -> Result<(), String> {
        self.intent
            .session
            .operation
            .check_finalization(&self.permit, self.intent.sha256)?;
        self.intent.session.check_inventory()?;
        self.intent.check_record()
    }
    pub(crate) fn intent_sha256(&self) -> [u8; 32] {
        self.intent.sha256
    }
    pub(crate) fn target(&self) -> &Path {
        &self.intent.frame.target
    }
    pub(crate) fn target_binding(&self) -> [u8; 32] {
        self.intent.frame.target_key
    }
    pub(crate) fn growth(&self) -> u64 {
        self.intent.frame.growth
    }
    pub(crate) fn check_plan(&self, plan: &PublicationPlan) -> Result<(), String> {
        self.check()?;
        if plan.sha256 != self.intent.sha256
            || plan.frame.id != self.intent.frame.id
            || plan.frame.target_key != self.intent.frame.target_key
        {
            return Err("Committed intent does not match the owned destination plan".into());
        }
        plan.matches_session(self.intent.session)
    }
}
impl PublicationPlan {
    fn matches_session(&self, session: &SourceSession) -> Result<(), String> {
        if self.frame.operation != session.id
            || !self
                .runtime
                .upgrade()
                .is_some_and(|runtime| Arc::ptr_eq(&runtime, &session.runtime))
            || !self.resident.same_pool(&session.resident())
            || !self.disk.same_pool(&session.disk())
        {
            return Err("Destination plan belongs to another source session or pool".into());
        }
        Ok(())
    }
    pub(crate) fn check_session(&self, session: &SourceSession) -> Result<(), String> {
        self.matches_session(session)?;
        session.check()
    }
    pub(crate) fn uses_budgets(
        &self,
        resident: &crate::large_document::ResidentBudget,
        disk: &crate::large_document::DiskBudget,
    ) -> bool {
        self.resident.same_pool(resident) && self.disk.same_pool(disk)
    }
    pub(crate) fn id(&self) -> [u8; 16] {
        self.frame.id
    }
    pub(crate) fn intent_sha256(&self) -> [u8; 32] {
        self.sha256
    }
    pub(crate) fn target_binding(&self) -> [u8; 32] {
        self.frame.target_key
    }
    pub(crate) fn target(&self) -> &Path {
        &self.frame.target
    }
    pub(crate) fn parent_identity(&self) -> (u64, u64) {
        self.frame.parent
    }
    pub(crate) fn backup_directory(&self) -> &Path {
        &self.frame.backups
    }
    pub(crate) fn backup_directory_identity(&self) -> (u64, u64) {
        self.frame.backups_identity
    }
    pub(crate) fn expected_version(&self) -> Option<crate::file_safety::Version> {
        self.expected
    }
    pub(crate) fn candidate_bytes(&self) -> u64 {
        self.frame.candidate_bytes
    }
    pub(crate) fn candidate_sha256(&self) -> [u8; 32] {
        self.frame.candidate_sha256
    }
    pub(crate) fn temporary_name(&self) -> &std::ffi::OsStr {
        &self.frame.temporary
    }
    pub(crate) fn backup_name(&self) -> Option<&std::ffi::OsStr> {
        self.frame.backup.as_deref()
    }
    pub(crate) fn growth(&self) -> u64 {
        self.frame.growth
    }
}
impl SourceSession {
    /// Must complete before the caller creates ANY destination temporary or
    /// backup. The borrowed inventory may be released only after this returns.
    pub(crate) fn prepare_publication<'a>(
        &'a self,
        candidate: &CompletedSave36<'_>,
        observed: &NativeExternalSnapshot<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<PublicationIntent<'a>, String> {
        check()?;
        self.check()?;
        candidate.check_session(self)?;
        if !observed.uses_budget(&self.resident()) {
            return Err("External observation uses another resident pool".into());
        }
        candidate.recheck(check)?;
        observed.revalidate(check)?;
        let owner = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            state.as_ref().ok_or("Source inventory unavailable")?.owner
        };
        let frame = capture(self, owner, candidate, observed, check)?;
        self.persist_pending(frame, observed, check)
    }
    // This is journal admission only. The production caller above derives the
    // frame from nominal source/lease proofs; tests may exercise crash windows
    // without constructing editable pixels or a destination-publication proof.
    fn persist_pending<'a>(
        &'a self,
        frame: Admitted<IntentFrame>,
        observed: &NativeExternalSnapshot<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<PublicationIntent<'a>, String> {
        check()?;
        self.check()?;
        observed.revalidate(check)?;
        if !frame.owner().uses_budget(&self.resident()) || !observed.uses_budget(&self.resident()) {
            return Err("External journal uses another resident pool".into());
        }
        let encoded = frame.encode(&self.resident(), check)?;
        let readback = std::sync::Mutex::new(
            ScratchBytes::new(&self.resident(), 64 * 1024, 64 * 1024).map_err(|e| e.to_string())?,
        );
        let sha256 = digest(&SHA256, &encoded).as_ref().try_into().unwrap();
        let facts = facts(&frame, &self.resident(), check)?;
        let ticket = self
            .disk()
            .reserve_publication(
                sha256,
                frame.target_key,
                &facts,
                frame.expected.map(|e| e.identity),
                frame.growth,
            )
            .map_err(|e| e.to_string())?;
        let result = (|| {
            check()?;
            self.check()?;
            observed.revalidate(check)?;
            {
                let mut state = self
                    .runtime
                    .inventory
                    .try_lock()
                    .map_err(|_| "Source inventory is busy")?;
                let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
                if inventory.files.len() >= self.runtime.limits.max_files
                    || inventory.files.len() == inventory.files.capacity()
                {
                    return Err("External journal file capacity exhausted; debt retained".into());
                }
                inventory.publications.insert(frame.clone(), sha256, None)?;
            }
            let directory = Directory::open(&self.directory)?;
            let charge = self
                .disk()
                .reserve(encoded.len() as u64)
                .map_err(|e| e.to_string())?;
            let id = uuid::Uuid::from_bytes(frame.id);
            let from = format!("external-pending-{id}");
            let to = format!("external-intent-{id}.pbi");
            let mut file = owned_rename::staging_options()
                .open(self.directory.join(&from))
                .map_err(|_| "Cannot create exclusive external intent; debt retained")?;
            charge.bind(&file).map_err(|e| e.to_string())?;
            for chunk in encoded.chunks(64 * 1024) {
                check()?;
                self.check()?;
                file.write_all(chunk)
                    .map_err(|_| "External intent write failed; debt retained")?;
            }
            let seal = owned_rename::Sealed::verify(
                &file,
                encoded.len() as u64,
                sha256,
                &self.resident(),
                &mut || {
                    check()?;
                    self.check()?;
                    observed.revalidate(check)
                },
            )?;
            owned_rename::publish_new(
                directory.path(),
                directory.held(),
                Path::new(&from),
                Path::new(&to),
                &file,
                &seal,
                &mut || {
                    check()?;
                    self.check()?;
                    observed.revalidate(check)
                },
            )?;
            let held = HeldFile::new(self.directory.join(to), file)?;
            observed.revalidate(check)?;
            self.check()?;
            let mut state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
            let entry = inventory
                .publications
                .entries
                .iter_mut()
                .find(|r| r.sha256 == sha256)
                .ok_or("External journal registration disappeared")?;
            if entry.file.is_some() || entry.frame.owner != inventory.owner {
                return Err("External journal completion is ambiguous; debt retained".into());
            }
            entry.file = Some(held.identity);
            inventory.files.push(held);
            drop(state);
            let intent = PublicationIntent {
                session: self,
                frame,
                sha256,
                ticket,
                readback,
            };
            intent.check()?;
            Ok(intent)
        })();
        if result.is_err() {
            self.runtime.tainted.store(true, Ordering::Release);
        }
        result
    }
}
fn capture(
    session: &SourceSession,
    owner: [u8; 16],
    candidate: &CompletedSave36<'_>,
    observed: &NativeExternalSnapshot<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Admitted<IntentFrame>, String> {
    let count = observed
        .backups()
        .len()
        .checked_add(usize::from(observed.source().is_some()))
        .filter(|n| *n <= frame::MAX_FACTS)
        .ok_or("External fact count exceeds admission")?;
    let strings = observed.backups().try_fold(0usize, |n, (name, _)| {
        n.checked_add(name.len().saturating_mul(4))
            .ok_or("External name footprint overflow")
    })?;
    let bytes = strings
        .checked_add(
            count
                .checked_mul(2 * std::mem::size_of::<NamedFact>())
                .ok_or("External fact footprint overflow")?,
        )
        .and_then(|n| n.checked_add(1024 * 1024))
        .filter(|n| *n <= frame::MAX_BYTES * 2)
        .ok_or("External frame footprint exceeds admission")?;
    let charge = MetadataCharge::reserve(&session.resident(), bytes, check)?;
    let id = *uuid::Uuid::new_v4().as_bytes();
    let uuid = uuid::Uuid::from_bytes(id);
    let key = crate::file_safety::destination_key(observed.target());
    let mut hash = Context::new(&SHA256);
    hash.update(b"PeerBrushExternalTarget1\0");
    for part in [
        observed.destination_parent_identity().0,
        observed.destination_parent_identity().1,
        key[0],
        key[1],
    ] {
        hash.update(&part.to_be_bytes());
    }
    let expected = observed.expected().map(|v| Expected {
        identity: v.identity,
        bytes: v.bytes,
        modified: v.modified_nanos(),
        sha256: v.sha256(),
    });
    let backup = expected.map(|_| {
        let mut name = OsString::from(format!("{:016x}{:016x}-{uuid}", key[0], key[1]));
        if let Some(extension) = observed.target().extension() {
            name.push(".");
            name.push(extension)
        }
        name
    });
    let mut facts = Vec::new();
    facts
        .try_reserve_exact(count)
        .map_err(|_| "External facts allocation refused")?;
    if let Some(source) = observed.source() {
        facts.push(NamedFact {
            role: Role::Target,
            name: observed
                .target()
                .file_name()
                .ok_or("Missing external target name")?
                .to_owned(),
            fact: convert(source),
        });
    }
    for (name, fact) in observed.backups() {
        check()?;
        facts.push(NamedFact {
            role: Role::Backup,
            name: name.to_owned(),
            fact: convert(fact),
        });
    }
    facts.sort_unstable_by_key(|f| f.fact.identity);
    let target_key = hash.finish().as_ref().try_into().unwrap();
    let previous = {
        let state = session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
        match inventory
            .publications
            .entries
            .iter()
            .rev()
            .find(|r| r.frame.target_key == target_key)
        {
            Some(entry) => Some(
                entry
                    .closed
                    .as_ref()
                    .ok_or("Target has a pending external intent; debt retained")?
                    .sha256,
            ),
            None => None,
        }
    };
    let frame = IntentFrame {
        id,
        owner,
        operation: session.id,
        target: observed.target().to_owned(),
        target_key,
        previous,
        parent: observed.destination_parent_identity(),
        backups: observed.backup_directory().to_owned(),
        backups_identity: observed.backup_directory_identity(),
        expected,
        candidate_bytes: candidate.bytes(),
        candidate_sha256: candidate.sha256(),
        provenance: candidate.provenance(),
        core: candidate.core_sha256(),
        render: candidate.scene().render_key()?,
        temporary: format!(".peerbrush-native-{uuid}.tmp").into(),
        backup,
        growth: candidate
            .bytes()
            .checked_add(expected.map_or(0, |e| e.bytes))
            .ok_or("External growth overflow")?,
        facts,
    };
    frame.validate()?;
    if let Some(previous) = frame.previous {
        let state = session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let entry = state
            .as_ref()
            .ok_or("Source inventory unavailable")?
            .publications
            .entries
            .iter()
            .find(|r| r.closed.as_ref().is_some_and(|c| c.sha256 == previous))
            .ok_or("Previous closed receipt is missing")?;
        let closed = entry
            .closed
            .as_ref()
            .ok_or("Previous closed receipt absent")?;
        chain::continues(&entry.frame, &closed.frame, closed.sha256, &frame)?;
    }
    Ok(Admitted::from_parts(Arc::new(frame), charge))
}
fn convert(f: crate::file_safety::NativeExternalFile) -> ExternalFact {
    ExternalFact {
        identity: f.identity(),
        bytes: f.bytes(),
        modified: f.modified_nanos(),
    }
}
fn facts(
    frame: &IntentFrame,
    pool: &crate::large_document::ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Admitted<Vec<ExternalFact>>, String> {
    let charge = MetadataCharge::reserve(
        pool,
        frame
            .facts
            .len()
            .checked_mul(2 * std::mem::size_of::<ExternalFact>())
            .and_then(|n| n.checked_add(128))
            .ok_or("External facts footprint overflow")?,
        check,
    )?;
    let mut values = Vec::new();
    values
        .try_reserve_exact(frame.facts.len())
        .map_err(|_| "External facts allocation refused")?;
    values.extend(frame.facts.iter().map(|f| f.fact));
    Ok(Admitted::from_parts(Arc::new(values), charge))
}
pub(super) fn recognized_name(name: &str) -> bool {
    name.strip_prefix("external-pending-")
        .or_else(|| {
            name.strip_prefix("external-intent-")
                .and_then(|s| s.strip_suffix(".pbi"))
        })
        .or_else(|| {
            name.strip_prefix("external-closed-")
                .and_then(|s| s.strip_suffix(".pbc"))
        })
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        || creation::parse_name(name).is_some()
}
/// Rebuild quota only. Decoded paths can be observed read-only under the held
/// profile/instance, but NEVER adopted as writer/deletion/source authority.
/// Pending frames retain full growth even if a destination write may have won.
pub(super) fn restore(
    runtime: &SourceRuntime,
    operation: &RootOperation,
    inventory: &mut Inventory,
) -> Result<(), String> {
    for held in &inventory.files {
        let Some(name) = held.path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("external-pending-") {
            return Err("Incomplete external intent retained; fresh writes refused".into());
        }
        let Some(id) = name
            .strip_prefix("external-intent-")
            .and_then(|s| s.strip_suffix(".pbi"))
        else {
            continue;
        };
        operation.check()?;
        held.check()?;
        let length = usize::try_from(held.bytes)
            .ok()
            .filter(|n| *n <= frame::MAX_BYTES)
            .ok_or("External journal length exceeds admission")?;
        let mut bytes = ScratchBytes::new(&runtime.resident, length, frame::MAX_BYTES)
            .map_err(|e| e.to_string())?;
        let mut file = &held.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot seek external intent")?;
        for chunk in bytes.chunks_mut(65536) {
            operation.check()?;
            held.check()?;
            file.read_exact(chunk)
                .map_err(|_| "Partial external intent retained")?;
        }
        let decoded = IntentFrame::decode(&bytes, &runtime.resident, &|| operation.check())?;
        if decoded.owner != inventory.owner
            || uuid::Uuid::parse_str(id).ok().map(|u| *u.as_bytes()) != Some(decoded.id)
        {
            return Err("External intent belongs to another namespace; debt retained".into());
        }
        let sha256 = digest(&SHA256, &bytes).as_ref().try_into().unwrap();
        held.check()?;
        inventory
            .publications
            .insert(decoded, sha256, Some(held.identity))?;
    }
    for held in &inventory.files {
        let Some((id, role)) = held
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(creation::parse_name)
        else {
            continue;
        };
        let entry = inventory
            .publications
            .entries
            .iter_mut()
            .find(|r| r.frame.id == id)
            .ok_or("Creation phase has no valid pending intent")?;
        operation.check()?;
        held.check()?;
        if held.bytes > creation::BYTES as u64 {
            return Err("Creation phase exceeds strict length".into());
        }
        if held.bytes == 0 {
            continue;
        }
        let mut bytes = [0u8; creation::BYTES];
        let length = held.bytes as usize;
        let mut file = &held.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot seek creation phase")?;
        file.read_exact(&mut bytes[..length])
            .map_err(|_| "Cannot read creation phase")?;
        if length < creation::BYTES {
            creation::Record::incomplete(&bytes[..length], &entry.frame, entry.sha256, role)?;
        } else {
            let record = creation::Record::decode(&bytes, &entry.frame, entry.sha256)?;
            if record.role != role {
                return Err("Creation phase name and body role differ".into());
            }
            let slot = match role {
                crate::file_safety::ExternalCreationRole::Temporary => 0,
                crate::file_safety::ExternalCreationRole::Backup => 1,
            };
            if entry.created[slot]
                .replace(creation::Created {
                    record,
                    file: held.identity,
                    sha256: digest(&SHA256, &bytes).as_ref().try_into().unwrap(),
                })
                .is_some()
            {
                return Err("Duplicate physical creation phase retained".into());
            }
        }
        held.check()?;
    }
    for held in &inventory.files {
        let Some(id) = held
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_prefix("external-closed-"))
            .and_then(|n| n.strip_suffix(".pbc"))
        else {
            continue;
        };
        let id = *uuid::Uuid::parse_str(id)
            .map_err(|_| "Invalid closed receipt name")?
            .as_bytes();
        let entry = inventory
            .publications
            .entries
            .iter_mut()
            .find(|r| r.frame.id == id)
            .ok_or("Closed receipt has no valid pending intent; retained")?;
        if entry.closing.replace(held.identity).is_some() || held.bytes > closed_frame::BYTES as u64
        {
            return Err("Closed receipt registration is ambiguous".into());
        }
        operation.check()?;
        held.check()?;
        if held.bytes == 0 {
            continue;
        }
        let mut bytes =
            ScratchBytes::new(&runtime.resident, held.bytes as usize, closed_frame::BYTES)
                .map_err(|e| e.to_string())?;
        let mut file = &held.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot seek closed receipt")?;
        for chunk in bytes.chunks_mut(65536) {
            operation.check()?;
            held.check()?;
            file.read_exact(chunk)
                .map_err(|_| "Cannot read held closed receipt")?;
        }
        if held.bytes < closed_frame::BYTES as u64 {
            closed_frame::ClosedFrame::incomplete(&bytes, &entry.frame, entry.sha256)?;
        } else {
            let frame = closed_frame::ClosedFrame::decode(
                &bytes,
                &entry.frame,
                entry.sha256,
                &runtime.resident,
                &|| operation.check(),
            )?;
            creation::validate_closed(entry, &frame)?;
            entry.closed = Some(ClosedRegistration {
                frame,
                sha256: digest(&SHA256, &bytes).as_ref().try_into().unwrap(),
                file: held.identity,
            });
        }
        held.check()?;
    }
    let ordered = chain_order(&inventory.publications, &runtime.resident, &|| {
        operation.check()
    })?;
    settlement::census(&inventory.publications, &ordered.latest, &|| {
        operation.check()
    })?;
    for at in &ordered.latest {
        let entry = &inventory.publications.entries[*at];
        if let Some(closed) = &entry.closed {
            settlement::reobserve(&entry.frame, &closed.frame, &|| operation.check())?;
            let charge = MetadataCharge::reserve(
                &runtime.resident,
                closed.frame.bindings.len() * 2 * std::mem::size_of::<ExternalFact>() + 128,
                &|| operation.check(),
            )?;
            let mut current = Vec::new();
            current
                .try_reserve_exact(closed.frame.bindings.len())
                .map_err(|_| "Restored external facts allocation refused")?;
            current.extend(closed.frame.bindings.iter().map(|b| b.fact));
            runtime
                .disk
                .restore_external(
                    entry.sha256,
                    entry.frame.target_key,
                    &current,
                    entry.frame.growth,
                )
                .map_err(|e| e.to_string())?;
            drop(current);
            drop(charge);
        } else {
            let facts = facts(&entry.frame, &runtime.resident, &|| operation.check())?;
            runtime
                .disk
                .restore_publication(
                    entry.sha256,
                    entry.frame.target_key,
                    &facts,
                    entry.frame.expected.map(|e| e.identity),
                    entry.frame.growth,
                )
                .map_err(|e| e.to_string())?;
        }
    }
    for entry in &inventory.publications.entries {
        if entry.closed.is_some() {
            runtime
                .disk
                .retain_settled_intent(entry.sha256, entry.frame.growth)
                .map_err(|e| e.to_string())?;
        }
    }
    reorder(
        &mut inventory.publications,
        &ordered.entries,
        &runtime.resident,
        &|| operation.check(),
    )?;
    Ok(())
}
fn reorder(
    registry: &mut Registry,
    order: &[usize],
    pool: &crate::large_document::ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let owner = MetadataCharge::reserve(
        pool,
        registry
            .maximum
            .checked_mul(3 * std::mem::size_of::<Option<Registered>>())
            .and_then(|n| n.checked_add(128))
            .ok_or("External reorder footprint overflow")?,
        check,
    )?;
    let mut slots = Vec::new();
    slots
        .try_reserve_exact(registry.entries.len())
        .map_err(|_| "External reorder allocation refused")?;
    let mut ordered = Vec::new();
    ordered
        .try_reserve_exact(registry.maximum)
        .map_err(|_| "External ordered registry allocation refused")?;
    slots.extend(registry.entries.drain(..).map(Some));
    for at in order {
        ordered.push(
            slots
                .get_mut(*at)
                .and_then(Option::take)
                .ok_or("External reorder index differs")?,
        );
    }
    if slots.iter().any(Option::is_some) {
        return Err("External reorder omitted records".into());
    }
    let old = std::mem::replace(&mut registry.entries, ordered);
    drop(old);
    drop(slots);
    drop(owner);
    Ok(())
}
fn chain_order(
    registry: &Registry,
    pool: &crate::large_document::ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Admitted<chain::Order>, String> {
    let owner = MetadataCharge::reserve(
        pool,
        registry.entries.len() * 2 * std::mem::size_of::<chain::Node<'_>>() + 128,
        check,
    )?;
    let mut nodes = Vec::new();
    nodes
        .try_reserve_exact(registry.entries.len())
        .map_err(|_| "External chain view allocation refused")?;
    nodes.extend(registry.entries.iter().map(|r| chain::Node {
        frame: &r.frame,
        closed: r.closed.as_ref().map(|c| (&*c.frame, c.sha256)),
    }));
    let ordered = chain::order(&nodes, pool, check)?;
    drop(nodes);
    drop(owner);
    Ok(ordered)
}

#[cfg(test)]
#[path = "publication/tests.rs"]
mod tests;
