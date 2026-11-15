//! One prearmed append-only receipt slot. Partial slots remain pending debt.
use super::{
    closed_frame::{Binding, ChargedClosed, ClosedFrame, Place, BYTES},
    frame::Role,
    ClosedRegistration, Directory, HeldFile, PublicationIntent,
};
use crate::{
    file_safety::{owned_rename, ExternalOutcomeProof},
    large_document::{DiskReservation, PreparedSettlement, ScratchBytes},
    psd::memory36::MetadataCharge,
};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

pub(super) struct Closing {
    file: File,
    directory: Directory,
    path: PathBuf,
    identity: (u64, u64),
    frame: ChargedClosed,
    encoded: ScratchBytes,
    readback: ScratchBytes,
    transfer: PreparedSettlement,
    _disk: DiskReservation,
    #[cfg(test)]
    fault: Option<Fault>,
}
#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Fault {
    BeforeFooter,
    AfterFooter,
    AfterSync,
    BeforeSelection,
}
impl Closing {
    #[cfg(test)]
    pub(super) fn inject(mut self, fault: Fault) -> Self {
        self.fault = Some(fault);
        self
    }
    #[cfg(test)]
    fn fail(&self, point: Fault) -> Result<(), String> {
        if self.fault == Some(point) {
            Err("injected closed journal phase".into())
        } else {
            Ok(())
        }
    }
    pub(super) fn prepare(
        intent: &PublicationIntent<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        intent.check()?;
        let maximum = intent
            .frame
            .facts
            .len()
            .checked_add(2)
            .ok_or("Closed inventory count overflow")?;
        let owner = MetadataCharge::reserve(&intent.session.resident(), 2 * 1024 * 1024, check)?;
        let mut bindings = Vec::new();
        bindings
            .try_reserve_exact(maximum)
            .map_err(|_| "Closed binding allocation refused")?;
        let mut retired = Vec::new();
        retired
            .try_reserve_exact(3)
            .map_err(|_| "Closed retirement allocation refused")?;
        let frame = ChargedClosed::prepared(
            ClosedFrame {
                id: intent.frame.id,
                owner: intent.frame.owner,
                intent: intent.sha256,
                target: intent.frame.target_key,
                growth: intent.frame.growth,
                bindings,
                retired,
            },
            owner,
        );
        let encoded = ScratchBytes::new(&intent.session.resident(), BYTES, BYTES)
            .map_err(|e| e.to_string())?;
        let readback = ScratchBytes::new(&intent.session.resident(), 65536, 65536)
            .map_err(|e| e.to_string())?;
        let transfer = intent
            .ticket
            .prepare_settlement(maximum)
            .map_err(|e| e.to_string())?;
        let directory = Directory::open(&intent.session.directory)?;
        let path = directory.path().join(format!(
            "external-closed-{}.pbc",
            uuid::Uuid::from_bytes(intent.frame.id)
        ));
        {
            let state = intent
                .session
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
            let pending = inventory
                .publications
                .entries
                .iter()
                .filter(|entry| entry.closing.is_some() && entry.closed.is_none())
                .count();
            if inventory
                .files
                .len()
                .checked_add(pending)
                .is_none_or(|n| n >= intent.session.runtime.limits.max_files)
                || inventory
                    .publications
                    .entries
                    .iter()
                    .find(|r| r.sha256 == intent.sha256)
                    .is_none_or(|r| r.closing.is_some())
            {
                return Err("Closed record has no unique preadmitted inventory slot".into());
            }
        }
        let disk = intent
            .session
            .disk()
            .reserve(BYTES as u64)
            .map_err(|e| e.to_string())?;
        let result = (|| {
            check()?;
            intent.check()?;
            let mut options = owned_rename::staging_options();
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.share_mode(1);
            }
            let file = options
                .open(&path)
                .map_err(|_| "Cannot create exclusive closed receipt slot; debt retained")?;
            disk.bind(&file).map_err(|e| e.to_string())?;
            file.sync_all()
                .map_err(|_| "Cannot sync closed receipt slot")?;
            crate::server::sync_parent(&path)?;
            let identity = crate::file_safety::identity(&file)?;
            let mut state = intent
                .session
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
            let entry = inventory
                .publications
                .entries
                .iter_mut()
                .find(|r| r.sha256 == intent.sha256)
                .ok_or("Closed intent registration disappeared")?;
            if entry.closing.replace(identity).is_some() {
                return Err("Closed receipt slot already exists".into());
            }
            drop(state);
            let closing = Self {
                file,
                directory,
                path,
                identity,
                frame,
                encoded,
                readback,
                transfer,
                _disk: disk,
                #[cfg(test)]
                fault: None,
            };
            closing.check_file(0)?;
            check()?;
            intent.check()?;
            Ok(closing)
        })();
        // An unselected outcome retains its target/debt. It is not native
        // source authority, so interruption must not disable checkpoint reads.
        result
    }
    fn check_file(&self, bytes: u64) -> Result<(), String> {
        self.directory.check()?;
        let metadata =
            fs::symlink_metadata(&self.path).map_err(|_| "Closed receipt slot disappeared")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Closed receipt slot is not a regular file".into());
        }
        let named = crate::file_safety::open_native_identity(&self.path)
            .map_err(|_| "Closed receipt slot name unavailable")?;
        if crate::file_safety::identity(&named)? != self.identity
            || crate::file_safety::identity(&self.file)? != self.identity
            || crate::file_safety::link_count(&self.file)? != 1
            || crate::file_safety::link_count(&named)? != 1
            || self
                .file
                .metadata()
                .map_err(|_| "Closed receipt metadata unavailable")?
                .len()
                != bytes
        {
            return Err("Closed receipt slot changed; all debt retained".into());
        }
        Ok(())
    }
    pub(super) fn finish(
        mut self,
        intent: &PublicationIntent<'_>,
        proof: &dyn ExternalOutcomeProof,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let result = (|| {
            check()?;
            self.check_file(0)?;
            if !self.frame.uses_budget(&intent.session.resident())
                || !self.transfer.uses_budget(&intent.session.disk())
            {
                return Err("Closed receipt uses another global pool".into());
            }
            proof.validate(intent.sha256, intent.frame.target_key, intent.frame.growth)?;
            capture(self.frame.get_mut()?, &intent.frame, proof, check)?;
            {
                let state = intent
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
                    .find(|r| r.sha256 == intent.sha256)
                    .ok_or("Closed intent registration missing")?;
                super::creation::validate_closed(entry, &self.frame)?;
            }
            let sha256 = self.frame.encode(
                &intent.frame,
                intent.sha256,
                &mut self.encoded,
                &intent.session.resident(),
                check,
            )?;
            let body = BYTES - 32;
            let mut written = 0;
            for chunk in self.encoded[..body].chunks(65536) {
                check()?;
                self.check_file(written)?;
                (&self.file)
                    .write_all(chunk)
                    .map_err(|_| "Closed receipt body write failed; pending retained")?;
                written += chunk.len() as u64;
            }
            // The complete footer is the selection marker. No SourceSession or
            // factory is reopened here; the caller supplies the exact prearmed
            // committed-intent guard (or normal-session abort guard).
            check()?;
            proof.validate(intent.sha256, intent.frame.target_key, intent.frame.growth)?;
            self.check_file(body as u64)?;
            #[cfg(test)]
            self.fail(Fault::BeforeFooter)?;
            (&self.file)
                .write_all(&self.encoded[body..])
                .map_err(|_| "Closed receipt seal write failed; pending retained")?;
            #[cfg(test)]
            self.fail(Fault::AfterFooter)?;
            self.file
                .sync_all()
                .map_err(|_| "Closed receipt sync uncertain; pending retained")?;
            #[cfg(test)]
            self.fail(Fault::AfterSync)?;
            self.check_file(BYTES as u64)?;
            let mut file = &self.file;
            file.seek(SeekFrom::Start(0))
                .map_err(|_| "Cannot read back closed receipt")?;
            let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
            let mut left = BYTES;
            while left != 0 {
                check()?;
                let count = left.min(self.readback.len());
                file.read_exact(&mut self.readback[..count])
                    .map_err(|_| "Closed receipt readback failed")?;
                hash.update(&self.readback[..count]);
                left -= count;
            }
            if hash.finish().as_ref() != sha256 {
                return Err("Closed receipt complete SHA differs; pending retained".into());
            }
            check()?;
            self.check_file(BYTES as u64)?;
            #[cfg(test)]
            self.fail(Fault::BeforeSelection)?;
            let held = HeldFile::new(self.path, self.file)?;
            self.transfer.settle(proof).map_err(|e| e.to_string())?;
            let mut state = intent
                .session
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_mut().ok_or("Source inventory unavailable")?;
            let entry = inventory
                .publications
                .entries
                .iter_mut()
                .find(|r| r.sha256 == intent.sha256)
                .ok_or("Closed intent registration disappeared")?;
            if entry.closed.is_some()
                || entry.closing != Some(self.identity)
                || inventory.files.len() == inventory.files.capacity()
            {
                return Err(
                    "Closed receipt in-memory selection is ambiguous; durable record retained"
                        .into(),
                );
            }
            entry.closed = Some(ClosedRegistration {
                frame: self.frame.clone(),
                sha256,
                file: self.identity,
            });
            inventory.files.push(held);
            drop(state);
            Ok(())
        })();
        // Primary intent/namespace checks latch their own integrity failures.
        // A partial outcome alone stays pending without poisoning native reads.
        result
    }
}
fn capture(
    out: &mut ClosedFrame,
    original: &super::frame::IntentFrame,
    proof: &dyn ExternalOutcomeProof,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let parent = Directory::open(
        original
            .target
            .parent()
            .ok_or("Missing external destination parent")?,
    )?;
    let backups = Directory::open(&original.backups)?;
    if parent.identity() != original.parent || backups.identity() != original.backups_identity {
        return Err("External directories differ from the intent".into());
    }
    out.bindings.clear();
    out.retired.clear();
    let mut push = |place, path: &Path, required: bool| -> Result<(), String> {
        check()?;
        match observe(path)? {
            Some(fact) => {
                if out.bindings.len() == out.bindings.capacity() {
                    return Err("Closed binding capacity changed".into());
                }
                out.bindings.push(Binding { place, fact });
            }
            None if required => {
                return Err("Retained external file disappeared; pending retained".into())
            }
            None => (),
        }
        Ok(())
    };
    push(Place::Target, &original.target, false)?;
    for (ordinal, old) in original
        .facts
        .iter()
        .enumerate()
        .filter(|(_, f)| f.role == Role::Backup)
    {
        push(
            Place::BackupOrdinal(ordinal as u32),
            &original.backups.join(&old.name),
            true,
        )?;
    }
    if let Some(name) = &original.backup {
        push(Place::CreatedBackup, &original.backups.join(name), false)?;
    }
    if let Some(name) = original.retained_original_name() {
        push(Place::RetainedOriginal, &original.backups.join(name), false)?;
    }
    push(
        Place::Temporary,
        &parent.path().join(&original.temporary),
        false,
    )?;
    out.bindings.sort_unstable_by_key(|b| b.fact.identity);
    if out.bindings.len() != proof.facts().len()
        || out
            .bindings
            .iter()
            .zip(proof.facts())
            .any(|(binding, f)| binding.fact != super::convert(*f))
        || proof.retired().len() > out.retired.capacity()
    {
        return Err("Closed named observations differ from the held Root outcome".into());
    }
    out.retired.extend_from_slice(proof.retired());
    out.validate(original, out.intent)?;
    check()?;
    parent.check()?;
    backups.check()?;
    proof.validate(out.intent, out.target, out.growth)
}
pub(super) fn reobserve(
    original: &super::frame::IntentFrame,
    closed: &ClosedFrame,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let parent = Directory::open(
        original
            .target
            .parent()
            .ok_or("Missing external destination parent")?,
    )?;
    let backups = Directory::open(&original.backups)?;
    if parent.identity() != original.parent || backups.identity() != original.backups_identity {
        return Err("Restored external directories changed; debt retained".into());
    }
    for binding in &closed.bindings {
        check()?;
        let path = match binding.place {
            Place::Target => original.target.clone(),
            Place::BackupOrdinal(ordinal) => original.backups.join(
                &original
                    .facts
                    .get(ordinal as usize)
                    .ok_or("Restored backup ordinal absent")?
                    .name,
            ),
            Place::CreatedBackup => original.backups.join(
                original
                    .backup
                    .as_deref()
                    .ok_or("Restored backup name absent")?,
            ),
            Place::Temporary => parent.path().join(&original.temporary),
            Place::RetainedOriginal => original.backups.join(
                original
                    .retained_original_name()
                    .ok_or("Restored original name absent")?,
            ),
        };
        if observe(&path)? != Some(binding.fact) {
            return Err("Restored external named facts changed; debt retained".into());
        }
    }
    check()?;
    parent.check()?;
    backups.check()
}
/// A directory member not covered by a known latest snapshot/outcome cannot be
/// silently omitted from debt. A pending plan's bounded new backup is covered
/// by its FULL growth, but its name alone grants no ownership/deletion rights.
pub(super) fn census(
    registry: &super::Registry,
    latest: &[usize],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    for (at, index) in latest.iter().enumerate() {
        let frame = &registry.entries[*index].frame;
        if latest[..at]
            .iter()
            .any(|i| registry.entries[*i].frame.backups_identity == frame.backups_identity)
        {
            continue;
        }
        let directory = Directory::open(&frame.backups)?;
        if directory.identity() != frame.backups_identity {
            return Err("External backup directory identity changed".into());
        }
        let mut count = 0usize;
        for item in
            fs::read_dir(directory.path()).map_err(|_| "Cannot inspect held backup membership")?
        {
            check()?;
            count += 1;
            if count > super::frame::MAX_FACTS {
                return Err("External backup membership exceeds admission".into());
            }
            let item = item.map_err(|_| "Cannot inspect backup member")?;
            let name = item.file_name();
            if name.len() > 4096 {
                return Err("External backup name exceeds admission".into());
            }
            let fact = observe(&item.path())?.ok_or("External backup member disappeared")?;
            let mut covered = false;
            for index in latest {
                let record = &registry.entries[*index];
                let original = &record.frame;
                if original.backups_identity != directory.identity() {
                    continue;
                }
                if let Some(closed) = &record.closed {
                    covered |= closed
                        .frame
                        .bindings
                        .iter()
                        .any(|binding| match binding.place {
                            Place::BackupOrdinal(ordinal) => original
                                .facts
                                .get(ordinal as usize)
                                .is_some_and(|old| old.name == name && binding.fact == fact),
                            Place::CreatedBackup => {
                                original.backup.as_deref() == Some(name.as_os_str())
                                    && binding.fact == fact
                            }
                            Place::RetainedOriginal => {
                                original.retained_original_name().as_deref()
                                    == Some(name.as_os_str())
                                    && binding.fact == fact
                            }
                            _ => false,
                        });
                } else {
                    covered |= original.facts.iter().any(|old| {
                        old.role == Role::Backup && old.name == name && old.fact == fact
                    });
                    covered |= original.backup.as_deref() == Some(name.as_os_str())
                        && original.expected.is_some_and(|old| fact.bytes <= old.bytes);
                    covered |= original.retained_original_name().as_deref()
                        == Some(name.as_os_str())
                        && original.expected.is_some_and(|old| {
                            old.identity == fact.identity
                                && old.bytes == fact.bytes
                                && old.modified == fact.modified
                        });
                }
            }
            if !covered {
                return Err(
                    "Unknown or changed backup member retains debt and refuses fresh admission"
                        .into(),
                );
            }
        }
        check()?;
        directory.check()?;
    }
    Ok(())
}
pub(super) fn observe(path: &Path) -> Result<Option<crate::large_document::ExternalFact>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("Cannot observe retained external file".into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err("Retained external path is not a regular file".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Retained external reparse path refused".into());
        }
    }
    let file = crate::file_safety::open_native_identity(path)
        .map_err(|_| "Cannot observe retained external identity")?;
    if crate::file_safety::link_count(&file)? != 1 {
        return Err("Retained external alias refused".into());
    }
    let held = file
        .metadata()
        .map_err(|_| "Cannot observe retained external metadata")?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if held.file_attributes() & 0x400 != 0 {
            return Err("Retained external reparse owner refused".into());
        }
    }
    let fact = crate::large_document::ExternalFact {
        identity: crate::file_safety::identity(&file)?,
        bytes: held.len(),
        modified: held
            .modified()
            .map_err(|_| "External timestamp unavailable")?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    };
    if held.len() != metadata.len() || held.modified().ok() != metadata.modified().ok() {
        return Err("External observation changed".into());
    }
    Ok(Some(fact))
}
