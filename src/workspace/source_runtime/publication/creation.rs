//! Durable held-ID registration before an external file's first payload byte.
pub(super) use super::creation_frame::{Record, BYTES};
use super::{frame::IntentFrame, HeldFile, PublicationIntent, PublicationPlan, SourceSession};
use crate::file_safety::{ExternalCreationProof, ExternalCreationRole, NativeExternalFile};
use ring::digest::SHA256;
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Weak},
};
#[derive(Clone, Copy)]
pub(super) struct Created {
    pub record: Record,
    pub file: (u64, u64),
    pub sha256: [u8; 32],
}
pub(super) fn validate_closed(
    entry: &super::Registered,
    closed: &super::closed_frame::ClosedFrame,
) -> Result<(), String> {
    use super::closed_frame::Place;
    for binding in &closed.bindings {
        let slot = match binding.place {
            Place::Target
                if entry
                    .frame
                    .expected
                    .is_some_and(|old| old.identity == binding.fact.identity) =>
            {
                continue
            }
            Place::Target | Place::Temporary => 0,
            Place::CreatedBackup => 1,
            Place::RetainedOriginal => {
                if entry
                    .frame
                    .expected
                    .is_none_or(|old| old.identity != binding.fact.identity)
                {
                    return Err("Relocated original has no admitted original identity".into());
                }
                continue;
            }
            Place::BackupOrdinal(_) => continue,
        };
        if entry.created[slot]
            .is_none_or(|created| created.record.fact.identity != binding.fact.identity)
        {
            return Err("Closed outcome lacks its durable physical creation phase".into());
        }
    }
    for id in &closed.retired {
        if entry.frame.expected.is_some_and(|old| old.identity == *id) {
            continue;
        }
        if !entry
            .created
            .iter()
            .flatten()
            .any(|created| created.record.fact.identity == *id)
        {
            return Err("Closed retirement lacks its exact registered created owner".into());
        }
    }
    Ok(())
}
/// Live intent +sealed creation proof minted this token. Decoding a file never
/// constructs one. It is insufficient for source adoption or publication.
#[must_use]
pub(crate) struct RegisteredCreation {
    frame: crate::psd::memory36::Admitted<IntentFrame>,
    record: Record,
    sha256: [u8; 32],
    runtime: Weak<super::SourceRuntime>,
}
fn slot(role: ExternalCreationRole) -> usize {
    match role {
        ExternalCreationRole::Temporary => 0,
        ExternalCreationRole::Backup => 1,
    }
}
fn suffix(role: ExternalCreationRole) -> &'static str {
    match role {
        ExternalCreationRole::Temporary => "temp",
        ExternalCreationRole::Backup => "backup",
    }
}
pub(super) fn name(id: [u8; 16], role: ExternalCreationRole) -> String {
    format!(
        "external-created-{}-{}.pbc",
        uuid::Uuid::from_bytes(id),
        suffix(role)
    )
}
pub(super) fn parse_name(name: &str) -> Option<([u8; 16], ExternalCreationRole)> {
    let tail = name
        .strip_prefix("external-created-")?
        .strip_suffix(".pbc")?;
    let (id, role) = tail.rsplit_once('-')?;
    let role = match role {
        "temp" => ExternalCreationRole::Temporary,
        "backup" => ExternalCreationRole::Backup,
        _ => return None,
    };
    Some((*uuid::Uuid::parse_str(id).ok()?.as_bytes(), role))
}
impl PublicationIntent<'_> {
    pub(crate) fn register_creation(
        &self,
        proof: &dyn ExternalCreationProof,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<RegisteredCreation, String> {
        check()?;
        self.check()?;
        proof.validate(self.sha256, self.frame.target_key, self.frame.growth)?;
        let role = proof.role();
        let expected_name = match role {
            ExternalCreationRole::Temporary => Some(self.frame.temporary.as_os_str()),
            ExternalCreationRole::Backup => self.frame.backup.as_deref(),
        };
        if expected_name != Some(proof.name()) {
            return Err("Created file name differs from the durable plan".into());
        }
        let record = Record {
            id: self.frame.id,
            owner: self.frame.owner,
            intent: self.sha256,
            target: self.frame.target_key,
            growth: self.frame.growth,
            role,
            parent: proof.parent_identity(),
            fact: super::convert(proof.fact()),
        };
        record.validate(&self.frame, self.sha256)?;
        {
            let state = self
                .session
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
                .find(|r| r.sha256 == self.sha256)
                .ok_or("Creation intent registration missing")?;
            if let Some(old) = entry.created[slot(role)] {
                if old.record != record {
                    return Err("Conflicting physical creation registration retained".into());
                }
                drop(state);
                let token = RegisteredCreation {
                    frame: self.frame.clone(),
                    record,
                    sha256: old.sha256,
                    runtime: Arc::downgrade(&self.session.runtime),
                };
                token.check_registered(self.session, role, proof.fact())?;
                return Ok(token);
            }
        }
        let bytes = record.encode();
        let sha256 = ring::digest::digest(&SHA256, &bytes)
            .as_ref()
            .try_into()
            .unwrap();
        let charge = self
            .session
            .disk()
            .reserve(BYTES as u64)
            .map_err(|e| e.to_string())?;
        let path = self.session.directory.join(name(self.frame.id, role));
        let result = (|| {
            check()?;
            self.check()?;
            proof.validate(self.sha256, self.frame.target_key, self.frame.growth)?;
            let mut file = crate::file_safety::owned_rename::staging_options()
                .open(&path)
                .map_err(|_| "Cannot create exclusive physical creation journal")?;
            charge.bind(&file).map_err(|e| e.to_string())?;
            file.write_all(&bytes[..BYTES - 32])
                .map_err(|_| "Cannot append creation body; debt retained")?;
            check()?;
            self.check()?;
            proof.validate(self.sha256, self.frame.target_key, self.frame.growth)?;
            file.write_all(&bytes[BYTES - 32..])
                .map_err(|_| "Cannot append creation seal; debt retained")?;
            file.sync_all()
                .map_err(|_| "Creation seal sync uncertain")?;
            crate::server::sync_parent(&path)?;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| "Cannot read back creation seal")?;
            let mut actual = [0u8; BYTES];
            file.read_exact(&mut actual)
                .map_err(|_| "Creation seal readback failed")?;
            if actual != bytes {
                return Err("Complete creation seal differs; debt retained".into());
            }
            let held = HeldFile::new(path, file)?;
            check()?;
            self.check()?;
            proof.validate(self.sha256, self.frame.target_key, self.frame.growth)?;
            held.check()?;
            let mut state = self
                .session
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
            if inventory.files.len() >= self.session.runtime.limits.max_files
                || inventory.files.len() == inventory.files.capacity()
            {
                return Err("Creation inventory slot unavailable".into());
            }
            let entry = inventory
                .publications
                .entries
                .iter_mut()
                .find(|r| r.sha256 == self.sha256)
                .ok_or("Creation intent registration missing")?;
            if entry.created[slot(role)]
                .replace(Created {
                    record,
                    file: held.identity,
                    sha256,
                })
                .is_some()
            {
                return Err("Creation registration raced; debt retained".into());
            }
            inventory.files.push(held);
            drop(state);
            Ok(RegisteredCreation {
                frame: self.frame.clone(),
                record,
                sha256,
                runtime: Arc::downgrade(&self.session.runtime),
            })
        })();
        // An unselected creation phase grants no external writer authority.
        // Keep pending debt while allowing unrelated native checkpoint reads.
        result
    }
}
impl RegisteredCreation {
    pub(crate) fn check_plan(
        &self,
        plan: &PublicationPlan,
        session: &SourceSession,
        role: ExternalCreationRole,
        fact: NativeExternalFile,
    ) -> Result<(), String> {
        plan.check_session(session)?;
        if self.record.intent != plan.intent_sha256()
            || self.frame.id != plan.id()
            || self.record.target != plan.target_binding()
        {
            return Err("Registered physical creation belongs to another plan".into());
        }
        self.check_registered(session, role, fact)
    }
    fn check_registered(
        &self,
        session: &SourceSession,
        role: ExternalCreationRole,
        fact: NativeExternalFile,
    ) -> Result<(), String> {
        session.check()?;
        if self.frame.operation != session.id
            || self.record.role != role
            || self.record.fact.identity != fact.identity()
            || !self
                .runtime
                .upgrade()
                .is_some_and(|runtime| Arc::ptr_eq(&runtime, &session.runtime))
        {
            return Err("Registered physical creation belongs to another operation or file".into());
        }
        let state = session
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
        let created = inventory
            .publications
            .entries
            .iter()
            .find(|r| r.sha256 == self.record.intent)
            .and_then(|r| r.created[slot(role)])
            .ok_or("Physical creation is not durably registered")?;
        if created.record != self.record || created.sha256 != self.sha256 {
            return Err("Physical creation registration changed".into());
        }
        let held = inventory
            .files
            .iter()
            .find(|f| f.identity == created.file)
            .ok_or("Creation journal lost its held owner")?;
        held.check()?;
        let mut file = &held.file;
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot inspect creation journal")?;
        let mut bytes = [0; BYTES];
        file.read_exact(&mut bytes)
            .map_err(|_| "Creation journal is partial")?;
        if ring::digest::digest(&SHA256, &bytes).as_ref() != self.sha256 {
            return Err("Creation journal complete SHA changed".into());
        }
        held.check()
    }
}
