//! Closed filesystem outcome boundary for external-disk accounting.
//! Raw fact records and journal checksums never implement this authority.
use super::NativeExternalFile;
use crate::{
    large_document::{Reservation, ScratchBytes},
    psd::save36::CompletedSave36,
    workspace::source_runtime::{
        CommittedPublication, PublicationIntent, PublicationPlan, RegisteredCreation, SourceSession,
    },
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
};
use std::{
    fs::{self, File},
    io::{Read, Write},
};

const OUTCOME_HEADERS: usize = 512 * 1024;
const TRANSFER: usize = 65536;

fn component(name: &OsStr) -> Result<(), String> {
    if name.as_encoded_bytes().len() > 1024
        || !matches!(
            (
                Path::new(name).components().next(),
                Path::new(name).components().nth(1)
            ),
            (Some(std::path::Component::Normal(_)), None)
        )
    {
        return Err("Native publication name is not a bounded component".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        if name.encode_wide().any(|u| u == 0 || u == b':' as u16) {
            return Err("Native publication refuses stream aliases".into());
        }
    }
    Ok(())
}
fn absent(path: &Path) -> Result<(), String> {
    if matches!(fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
        Ok(())
    } else {
        Err("Native publication name already exists or is uncertain; retained".into())
    }
}
fn named(file: &File, path: &Path, expected: NativeExternalFile) -> Result<(), String> {
    if !super::regular(path) {
        return Err("Native outcome name is not regular".into());
    }
    let current =
        super::open_native_identity(path).map_err(|_| "Native outcome name unavailable")?;
    if NativeExternalFile::observe_held(file)? != expected
        || NativeExternalFile::observe_held(&current)? != expected
    {
        return Err("Native outcome name or held identity changed; debt retained".into());
    }
    Ok(())
}
fn directory(file: &File, path: &Path, expected: (u64, u64)) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "Native destination parent unavailable")?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Native destination parent alias refused".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & (0x400 | 0x4000) != 0 {
            return Err("Native destination reparse or encrypted directory is unsupported".into());
        }
    }
    if super::identity(file)? != expected
        || super::identity(&super::owned_rename::open_directory(path)?)? != expected
    {
        return Err("Native destination parent identity changed; files retained".into());
    }
    Ok(())
}
fn create_temporary(parent: &File, path: &Path, name: &OsStr) -> Result<File, String> {
    component(name)?;
    #[cfg(unix)]
    {
        use std::os::{
            fd::{AsRawFd, FromRawFd},
            unix::ffi::OsStrExt,
        };
        let name =
            std::ffi::CString::new(name.as_bytes()).map_err(|_| "Native temporary NUL refused")?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDWR
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_NOFOLLOW
                    | libc::O_CLOEXEC
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            return Err("Native exclusive temporary creation failed; names retained".into());
        }
        // Fresh exclusive regular descriptor returned by openat, uniquely owned.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        super::owned_rename::staging_options()
            .open(path.join(name))
            .map_err(|_| "Native exclusive temporary creation failed; names retained".into())
    }
}

/// New-destination driver only. Existing-target relocation has a separate
/// journal/recovery contract; this type cannot accidentally select overwrite.
/// Drop never unlinks a file or refunds the admitted intent.
pub(crate) struct NativeNewDestination<'inventory, 'lease, 'session> {
    plan: PublicationPlan,
    inventory: &'inventory super::NativeInventory<'lease>,
    session: &'session SourceSession,
    parent: File,
    temporary: File,
    temporary_id: (u64, u64),
    written: u64,
    sealed: Option<super::owned_rename::Sealed>,
    failed: bool,
    creation: Option<RegisteredCreation>,
    #[cfg(windows)]
    _pins: Vec<File>,
    headers: Reservation,
}
impl<'inventory, 'lease, 'session> NativeNewDestination<'inventory, 'lease, 'session> {
    pub(crate) fn create(
        session: &'session SourceSession,
        candidate: &CompletedSave36<'_>,
        inventory: &'inventory super::NativeInventory<'lease>,
        intent: &PublicationIntent<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        candidate.check_session(session)?;
        let observed = inventory.observe_external(check)?;
        let plan = intent.plan(&observed, check)?;
        plan.check_session(session)?;
        if plan.expected_version().is_some()
            || plan.backup_name().is_some()
            || inventory.writer_lease().expected().is_some()
            || inventory.writer_lease().path() != plan.target()
            || candidate.bytes() != plan.candidate_bytes()
            || candidate.sha256() != plan.candidate_sha256()
            || !plan.uses_budgets(&session.resident(), &session.disk())
            || !inventory.writer_budget().same_pool(&session.resident())
        {
            return Err(
                "Native new-destination admission differs; no external bytes created".into(),
            );
        }
        drop(observed);
        component(plan.temporary_name())?;
        let headers = session
            .resident()
            .reserve_bytes(OUTCOME_HEADERS, OUTCOME_HEADERS)
            .map_err(|e| e.to_string())?;
        let parent_path = plan
            .target()
            .parent()
            .ok_or("Native destination has no parent")?;
        let parent = super::owned_rename::open_directory(parent_path)?;
        directory(&parent, parent_path, plan.parent_identity())?;
        #[cfg(windows)]
        let pins = super::owned_rename::pin_directories(parent_path, plan.parent_identity())?;
        candidate.recheck(check)?;
        plan.check_session(session)?;
        inventory.validate(check)?;
        absent(plan.target())?;
        absent(&parent_path.join(plan.temporary_name()))?;
        let temporary = create_temporary(&parent, parent_path, plan.temporary_name())?;
        super::native_regular(&temporary).map_err(|_| "Native temporary is not a regular owner")?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if temporary
                .metadata()
                .map_err(|_| "Native temporary metadata unavailable")?
                .file_attributes()
                & 0x4000
                != 0
            {
                return Err(
                    "Native encrypted temporary is unsupported; empty file and debt retained"
                        .into(),
                );
            }
        }
        let temporary_id = super::identity(&temporary)?;
        let mut staged = Self {
            plan,
            inventory,
            session,
            parent,
            temporary,
            temporary_id,
            written: 0,
            sealed: None,
            failed: false,
            creation: None,
            #[cfg(windows)]
            _pins: pins,
            headers,
        };
        staged.check_temporary()?;
        // No payload byte precedes the durable identity registration. Failure
        // retains the empty name and the entire admitted intent/debt.
        staged
            .temporary
            .sync_all()
            .map_err(|_| "Native empty creation sync uncertain; retained")?;
        crate::server::sync_parent(
            &staged
                .plan
                .target()
                .parent()
                .ok_or("Native creation parent missing")?
                .join(staged.plan.temporary_name()),
        )?;
        staged.check_temporary()?;
        let fact = NativeExternalFile::observe_held(&staged.temporary)?;
        let creation = intent.register_creation(
            &NativeCreationReceipt {
                writer: &staged,
                fact,
            },
            check,
        )?;
        creation.check_plan(&staged.plan, session, ExternalCreationRole::Temporary, fact)?;
        staged.creation = Some(creation);
        Ok(staged)
    }
    fn check_temporary(&self) -> Result<(), String> {
        let parent = self
            .plan
            .target()
            .parent()
            .ok_or("Native destination parent missing")?;
        directory(&self.parent, parent, self.plan.parent_identity())?;
        let fact = NativeExternalFile::observe_held(&self.temporary)?;
        if fact.identity() != self.temporary_id {
            return Err("Native temporary held identity changed".into());
        }
        named(
            &self.temporary,
            &parent.join(self.plan.temporary_name()),
            fact,
        )
    }
    fn active(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        if self.failed {
            return Err("Native destination staging is failed; files retained".into());
        }
        check()?;
        self.plan.check_session(self.session)?;
        self.check_temporary()?;
        self.creation
            .as_ref()
            .ok_or("Native temporary creation is not durably registered")?
            .check_plan(
                &self.plan,
                self.session,
                ExternalCreationRole::Temporary,
                NativeExternalFile::observe_held(&self.temporary)?,
            )?;
        absent(self.plan.target())
    }
    pub(crate) fn replay(
        &mut self,
        candidate: &CompletedSave36<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let result = (|| {
            if self.creation.is_none() {
                return Err("Native temporary creation is not durably registered".into());
            }
            self.active(check)?;
            candidate.check_session(self.session)?;
            if self.written != 0
                || self.sealed.is_some()
                || candidate.bytes() != self.plan.candidate_bytes()
                || candidate.sha256() != self.plan.candidate_sha256()
            {
                return Err("Native destination replay differs from the admitted candidate".into());
            }
            let mut buffer = ScratchBytes::new(&self.session.resident(), TRANSFER, TRANSFER)
                .map_err(|e| e.to_string())?;
            let mut failure = None;
            let session = self.session;
            let mut active = || {
                candidate
                    .check_session(session)
                    .and_then(|_| check())
                    .map_err(|e| {
                        failure = Some(e);
                        crate::large_document::Error::Cancelled
                    })
            };
            let reading = (|| -> Result<(), String> {
                let mut cursor = candidate.cursor(0, candidate.bytes(), &mut active)?;
                let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
                while self.written < candidate.bytes() {
                    self.active(check)?;
                    if self
                        .temporary
                        .metadata()
                        .map_err(|_| "Native temporary metadata unavailable")?
                        .len()
                        != self.written
                    {
                        return Err("Native temporary append position changed".into());
                    }
                    let count = (candidate.bytes() - self.written).min(TRANSFER as u64) as usize;
                    let got = cursor
                        .read(&mut buffer[..count])
                        .map_err(|e| e.to_string())?;
                    if got != count {
                        return Err("Native candidate replay ended early".into());
                    }
                    self.temporary
                        .write_all(&buffer[..count])
                        .map_err(|_| "Native temporary write failed; prefix retained")?;
                    hash.update(&buffer[..count]);
                    self.written += count as u64;
                }
                if cursor.read(&mut [0; 1]).map_err(|e| e.to_string())? != 0
                    || hash.finish().as_ref() != self.plan.candidate_sha256()
                {
                    return Err("Native candidate replay SHA or EOF differs".into());
                }
                Ok(())
            })();
            drop(active);
            if let Some(error) = failure {
                return Err(error);
            }
            reading?;
            candidate.recheck(check)?;
            self.active(check)?;
            let sealed = super::owned_rename::Sealed::verify(
                &self.temporary,
                self.plan.candidate_bytes(),
                self.plan.candidate_sha256(),
                &self.session.resident(),
                &mut || self.active(check),
            )?;
            self.inventory.validate(check)?;
            self.active(check)?;
            self.sealed = Some(sealed);
            Ok(())
        })();
        if result.is_err() {
            self.failed = true;
        }
        result
    }
    /// All payload work is complete before Root arms its Engine-only fence.
    pub(crate) fn ready_to_arm(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.active(check)?;
        self.sealed
            .as_ref()
            .ok_or("Native destination replay is incomplete")?
            .check(&self.temporary)?;
        self.inventory.validate(check)?;
        self.active(check)
    }
    pub(crate) fn publish(
        self,
        committed: &CommittedPublication<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<NativePublicationReceipt<'inventory, 'lease>, String> {
        committed.check_plan(&self.plan)?;
        check()?;
        if self.failed || self.written != self.plan.candidate_bytes() {
            return Err("Native staged destination is incomplete".into());
        }
        self.check_temporary()?;
        self.inventory.writer_facts(check)?;
        let sealed = self
            .sealed
            .as_ref()
            .ok_or("Native destination is not sealed")?;
        let parent = self
            .plan
            .target()
            .parent()
            .ok_or("Native destination parent missing")?;
        let target = self
            .plan
            .target()
            .file_name()
            .ok_or("Native destination name missing")?;
        super::owned_rename::publish_new(
            parent,
            &self.parent,
            Path::new(self.plan.temporary_name()),
            Path::new(target),
            &self.temporary,
            sealed,
            &mut || {
                committed.check_plan(&self.plan)?;
                check()
            },
        )?;
        let sealed = super::owned_rename::Sealed::verify(
            &self.temporary,
            self.plan.candidate_bytes(),
            self.plan.candidate_sha256(),
            self.inventory.writer_budget(),
            &mut || {
                committed.check_plan(&self.plan)?;
                check()
            },
        )?;
        let fact = NativeExternalFile::observe_held(&self.temporary)?;
        named(&self.temporary, self.plan.target(), fact)?;
        absent(&parent.join(self.plan.temporary_name()))?;
        let version = super::Version {
            identity: fact.identity(),
            bytes: fact.bytes(),
            modified: fact.modified_nanos(),
            fingerprint: self.plan.candidate_sha256(),
        };
        let core = OutcomeCore::new(
            self.plan,
            self.inventory,
            self.parent,
            self.temporary,
            true,
            Some(sealed),
            self.creation
                .ok_or("Native publication lost creation registration")?,
            self.headers,
            #[cfg(windows)]
            self._pins,
            check,
        )?;
        Ok(NativePublicationReceipt { core, version })
    }
    /// Abort keeps the new staging file as observed charged debt. No historical
    /// backup is deleted, and no unverified close/deletion becomes a refund.
    pub(crate) fn abort(
        self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<NativeAbortReceipt<'inventory, 'lease>, String> {
        check()?;
        self.check_temporary()?;
        absent(self.plan.target())?;
        let core = OutcomeCore::new(
            self.plan,
            self.inventory,
            self.parent,
            self.temporary,
            false,
            None,
            self.creation
                .ok_or("Native abort lost creation registration")?,
            self.headers,
            #[cfg(windows)]
            self._pins,
            check,
        )?;
        Ok(NativeAbortReceipt { core })
    }
}

struct OutcomeCore<'a, 'lease> {
    plan: PublicationPlan,
    inventory: &'a super::NativeInventory<'lease>,
    parent: File,
    file: File,
    published: bool,
    seal: Option<super::owned_rename::Sealed>,
    fact: NativeExternalFile,
    facts: Vec<NativeExternalFile>,
    retired: Vec<(u64, u64)>,
    _creation: RegisteredCreation,
    _headers: Reservation,
    #[cfg(windows)]
    _pins: Vec<File>,
}
impl<'a, 'lease> OutcomeCore<'a, 'lease> {
    #[allow(clippy::too_many_arguments)]
    fn new(
        plan: PublicationPlan,
        inventory: &'a super::NativeInventory<'lease>,
        parent: File,
        file: File,
        published: bool,
        seal: Option<super::owned_rename::Sealed>,
        creation: RegisteredCreation,
        headers: Reservation,
        #[cfg(windows)] pins: Vec<File>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let fact = NativeExternalFile::observe_held(&file)?;
        let mut facts = inventory.writer_facts(check)?;
        facts.push(fact);
        facts.sort_by_key(|f| f.identity());
        if facts.windows(2).any(|w| w[0].identity() == w[1].identity()) {
            return Err("Native outcome identity inventory is ambiguous".into());
        }
        let result = Self {
            plan,
            inventory,
            parent,
            file,
            published,
            seal,
            fact,
            facts,
            retired: Vec::new(),
            _creation: creation,
            _headers: headers,
            #[cfg(windows)]
            _pins: pins,
        };
        result.revalidate()?;
        Ok(result)
    }
    fn revalidate(&self) -> Result<(), String> {
        let parent = self
            .plan
            .target()
            .parent()
            .ok_or("Native outcome parent missing")?;
        directory(&self.parent, parent, self.plan.parent_identity())?;
        let mut facts = self.inventory.writer_facts(&|| Ok(()))?;
        let path = if self.published {
            self.plan.target().to_path_buf()
        } else {
            parent.join(self.plan.temporary_name())
        };
        named(&self.file, &path, self.fact)?;
        if self.published {
            self.seal
                .as_ref()
                .ok_or("Native publication seal missing")?
                .check(&self.file)?;
            absent(&parent.join(self.plan.temporary_name()))?;
        } else {
            absent(self.plan.target())?;
        }
        facts.push(self.fact);
        facts.sort_by_key(|f| f.identity());
        if facts != self.facts {
            return Err("Native outcome physical debt changed; reservation retained".into());
        }
        Ok(())
    }
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String> {
        if (intent, target, growth)
            != (
                self.plan.intent_sha256(),
                self.plan.target_binding(),
                self.plan.growth(),
            )
        {
            return Err("Native outcome belongs to another admitted intent".into());
        }
        self.revalidate()
    }
}
pub(crate) struct NativePublicationReceipt<'a, 'lease> {
    core: OutcomeCore<'a, 'lease>,
    version: super::Version,
}
pub(crate) struct NativeAbortReceipt<'a, 'lease> {
    core: OutcomeCore<'a, 'lease>,
}
impl NativePublicationReceipt<'_, '_> {
    pub(crate) fn version(&self) -> super::Version {
        self.version
    }
}
impl sealed::RootOutcome for NativePublicationReceipt<'_, '_> {}
impl sealed::RootOutcome for NativeAbortReceipt<'_, '_> {}
impl ExternalOutcomeProof for NativePublicationReceipt<'_, '_> {
    fn validate(&self, i: [u8; 32], t: [u8; 32], g: u64) -> Result<(), String> {
        self.core.validate(i, t, g)
    }
    fn facts(&self) -> &[NativeExternalFile] {
        &self.core.facts
    }
    fn retired(&self) -> &[(u64, u64)] {
        &self.core.retired
    }
}
impl ExternalOutcomeProof for NativeAbortReceipt<'_, '_> {
    fn validate(&self, i: [u8; 32], t: [u8; 32], g: u64) -> Result<(), String> {
        self.core.validate(i, t, g)
    }
    fn facts(&self) -> &[NativeExternalFile] {
        &self.core.facts
    }
    fn retired(&self) -> &[(u64, u64)] {
        &self.core.retired
    }
}

/// Internal name binding; only the closed writer can mint a production value
/// from the durable publication plan. A raw name is never backup authority.
pub(super) struct BackupPlan {
    target: PathBuf,
    expected: Option<super::Version>,
    name: Option<OsString>,
}
impl BackupPlan {
    pub(super) fn target(&self) -> &Path {
        &self.target
    }
    pub(super) fn expected(&self) -> Option<super::Version> {
        self.expected
    }
    pub(super) fn name(&self) -> Option<&OsStr> {
        self.name.as_deref()
    }
    #[cfg(test)]
    pub(super) fn for_tests(
        target: PathBuf,
        expected: Option<super::Version>,
        name: Option<OsString>,
    ) -> Self {
        Self {
            target,
            expected,
            name,
        }
    }
}

mod sealed {
    pub trait RootOutcome {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ExternalCreationRole {
    Temporary,
    Backup,
}
/// A live zero-byte exclusive owner. A planned name or copied metadata fact
/// cannot implement this sealed authority, even when its digest happens to fit.
pub(crate) trait ExternalCreationProof: sealed::RootOutcome {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String>;
    fn fact(&self) -> NativeExternalFile;
    fn role(&self) -> ExternalCreationRole;
    fn name(&self) -> &std::ffi::OsStr;
    fn parent_identity(&self) -> (u64, u64);
}
struct NativeCreationReceipt<'a, 'inventory, 'lease, 'session> {
    writer: &'a NativeNewDestination<'inventory, 'lease, 'session>,
    fact: NativeExternalFile,
}
impl sealed::RootOutcome for NativeCreationReceipt<'_, '_, '_, '_> {}
impl ExternalCreationProof for NativeCreationReceipt<'_, '_, '_, '_> {
    fn validate(&self, i: [u8; 32], t: [u8; 32], g: u64) -> Result<(), String> {
        let writer = self.writer;
        if (i, t, g)
            != (
                writer.plan.intent_sha256(),
                writer.plan.target_binding(),
                writer.plan.growth(),
            )
            || self.fact.bytes() != 0
            || writer.written != 0
            || writer.sealed.is_some()
            || writer.failed
        {
            return Err("Native creation proof differs from its zero-byte admitted owner".into());
        }
        writer.check_temporary()?;
        if NativeExternalFile::observe_held(&writer.temporary)? != self.fact {
            return Err("Native zero-byte creation changed before durable registration".into());
        }
        writer.inventory.validate(&|| Ok(()))
    }
    fn fact(&self) -> NativeExternalFile {
        self.fact
    }
    fn role(&self) -> ExternalCreationRole {
        ExternalCreationRole::Temporary
    }
    fn name(&self) -> &std::ffi::OsStr {
        self.writer.plan.temporary_name()
    }
    fn parent_identity(&self) -> (u64, u64) {
        self.writer.plan.parent_identity()
    }
}

/// Implemented by the native destination writer's held publication/abort
/// receipts, after all named identities and retired owners are reobserved.
/// Validation may perform I/O and therefore precedes the ledger mutex.
pub(crate) trait ExternalOutcomeProof: sealed::RootOutcome {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String>;
    fn facts(&self) -> &[NativeExternalFile];
    fn retired(&self) -> &[(u64, u64)];
}

// Production implementations above remain internal to this filesystem writer.
// They do not activate native save routing or manufacture Engine authority.

#[cfg(test)]
pub(crate) struct AccountingOutcome {
    intent: [u8; 32],
    target: [u8; 32],
    growth: u64,
    facts: Vec<NativeExternalFile>,
    retired: Vec<(u64, u64)>,
    accepted: bool,
}
#[cfg(test)]
impl AccountingOutcome {
    /// Accounting-only fixture. This cannot qualify filesystem publication,
    /// source integrity or recovery authority; it is absent from production.
    pub(crate) fn new(
        intent: [u8; 32],
        target: [u8; 32],
        growth: u64,
        facts: Vec<NativeExternalFile>,
        retired: Vec<(u64, u64)>,
    ) -> Self {
        Self {
            intent,
            target,
            growth,
            facts,
            retired,
            accepted: true,
        }
    }
    pub(crate) fn refuse(mut self) -> Self {
        self.accepted = false;
        self
    }
}
#[cfg(test)]
impl sealed::RootOutcome for AccountingOutcome {}
#[cfg(test)]
impl ExternalOutcomeProof for AccountingOutcome {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String> {
        if self.accepted && (self.intent, self.target, self.growth) == (intent, target, growth) {
            Ok(())
        } else {
            Err("Accounting fixture outcome refused".into())
        }
    }
    fn facts(&self) -> &[NativeExternalFile] {
        &self.facts
    }
    fn retired(&self) -> &[(u64, u64)] {
        &self.retired
    }
}

#[cfg(test)]
pub(crate) struct AccountingCreation {
    intent: [u8; 32],
    target: [u8; 32],
    growth: u64,
    fact: NativeExternalFile,
    role: ExternalCreationRole,
    name: std::ffi::OsString,
    parent: (u64, u64),
    accepted: bool,
}
#[cfg(test)]
impl AccountingCreation {
    /// Journal admission fixture only; absent from production, no file authority.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        intent: [u8; 32],
        target: [u8; 32],
        growth: u64,
        fact: NativeExternalFile,
        role: ExternalCreationRole,
        name: std::ffi::OsString,
        parent: (u64, u64),
    ) -> Self {
        Self {
            intent,
            target,
            growth,
            fact,
            role,
            name,
            parent,
            accepted: true,
        }
    }
    pub(crate) fn refuse(mut self) -> Self {
        self.accepted = false;
        self
    }
}
#[cfg(test)]
impl sealed::RootOutcome for AccountingCreation {}
#[cfg(test)]
impl ExternalCreationProof for AccountingCreation {
    fn validate(&self, i: [u8; 32], t: [u8; 32], g: u64) -> Result<(), String> {
        if self.accepted
            && self.fact.bytes() == 0
            && (i, t, g) == (self.intent, self.target, self.growth)
        {
            Ok(())
        } else {
            Err("Accounting creation fixture refused".into())
        }
    }
    fn fact(&self) -> NativeExternalFile {
        self.fact
    }
    fn role(&self) -> ExternalCreationRole {
        self.role
    }
    fn name(&self) -> &std::ffi::OsStr {
        &self.name
    }
    fn parent_identity(&self) -> (u64, u64) {
        self.parent
    }
}

#[cfg(test)]
#[path = "native_outcome_tests.rs"]
mod tests;
