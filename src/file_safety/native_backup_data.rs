//! Registered copies of the original main data stream and supported access
//! rules. This is not permission to replace a file or discard filesystem metadata.
use super::super::NativeInventory;
use super::*;
use crate::large_document::verified_transfer_buffer;
use std::io::Read;

pub(crate) struct NativeBackupData<'session> {
    plan: PublicationPlan,
    parent: File,
    file: File,
    fact: NativeExternalFile,
    sealed: super::super::owned_rename::Sealed,
    _creation: RegisteredCreation,
    session: &'session SourceSession,
    _headers: Reservation,
    #[cfg(windows)]
    _pins: Vec<File>,
}
impl<'session> NativeBackupData<'session> {
    pub(crate) fn prepare(
        session: &'session SourceSession,
        candidate: &CompletedSave36<'_>,
        inventory: &mut NativeInventory<'_>,
        intent: &PublicationIntent<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        candidate.check_session(session)?;
        let observed = inventory.observe_external(check)?;
        let plan = intent.plan(&observed, check)?;
        plan.check_session(session)?;
        let expected = plan
            .expected_version()
            .ok_or("A backup needs an observed existing destination")?;
        if candidate.bytes() != plan.candidate_bytes()
            || candidate.sha256() != plan.candidate_sha256()
            || !plan.uses_budgets(&session.resident(), &session.disk())
            || !inventory.writer_budget().same_pool(&session.resident())
        {
            return Err("Backup differs from the admitted native candidate".into());
        }
        drop(observed);
        let name = plan
            .backup_name()
            .ok_or("Native backup has no planned name")?;
        component(name)?;
        inventory.admit_created_backup(expected.bytes)?;
        let headers = session
            .resident()
            .reserve_bytes(OUTCOME_HEADERS, OUTCOME_HEADERS)
            .map_err(|e| e.to_string())?;
        let source = super::super::open_native_read(plan.target())
            .map_err(|_| "Native backup source unavailable")?;
        super::super::native_access::supported(&source)?;
        inventory.validate(check)?;
        let parent = inventory.writer_lease().native_versions_handle()?;
        directory(
            &parent,
            plan.backup_directory(),
            plan.backup_directory_identity(),
        )?;
        #[cfg(windows)]
        let pins = super::super::owned_rename::pin_directories(
            plan.backup_directory(),
            plan.backup_directory_identity(),
        )?;
        let path = plan.backup_directory().join(name);
        absent(&path)?;
        let file = create_backup(&parent, plan.backup_directory(), name)?;
        super::super::native_regular(&file)
            .map_err(|_| "Native backup is not a regular exclusive owner")?;
        super::super::native_access::supported(&file)?;
        super::super::owned_rename::hold_write_stamp(&file)?;
        let identity = super::super::identity(&file)?;
        file.sync_all()
            .map_err(|_| "Native empty backup sync uncertain; debt retained")?;
        crate::server::sync_parent(&path)?;
        let fact = NativeExternalFile::observe_held(&file)?;
        let creation = intent.register_creation(
            &Creation {
                plan: &plan,
                inventory,
                file: &file,
                parent: &parent,
                fact,
                session,
            },
            check,
        )?;
        let active = || {
            check()?;
            session.check()?;
            creation.check_plan(
                &plan,
                session,
                ExternalCreationRole::Backup,
                NativeExternalFile::observe_held(&file)?,
            )?;
            inventory.validate_created_backup(&plan, &file, identity, check)?;
            source_matches(&source, &plan, expected)
        };
        active()?;
        super::super::native_access::preserve(
            &source,
            &file,
            super::super::native_access::Destination::Backup,
            &session.resident(),
            &active,
        )?;
        let budget = session.resident();
        let mut buffer =
            verified_transfer_buffer(&budget, expected.bytes).map_err(|e| e.to_string())?;
        let mut written = 0;
        let mut reader = &source;
        let mut writer = &file;
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        while written < expected.bytes {
            active()?;
            if file
                .metadata()
                .map_err(|_| "Native backup metadata unavailable")?
                .len()
                != written
            {
                return Err("Native backup append position changed; prefix retained".into());
            }
            let count = (expected.bytes - written).min(buffer.len() as u64) as usize;
            reader
                .read_exact(&mut buffer[..count])
                .map_err(|_| "Native backup source became incomplete; prefix retained")?;
            hash.update(&buffer[..count]);
            writer
                .write_all(&buffer[..count])
                .map_err(|_| "Native backup write failed; prefix retained")?;
            written += count as u64;
        }
        if reader
            .read(&mut [0; 1])
            .map_err(|_| "Native backup source EOF unavailable")?
            != 0
            || hash.finish().as_ref() != expected.sha256()
        {
            return Err("Native backup source bytes changed; all names and debt retained".into());
        }
        active()?;
        super::super::owned_rename::finish_write_stamp(&file)?;
        let sealed = super::super::owned_rename::Sealed::verify_with_buffer(
            &file,
            expected.bytes,
            expected.sha256(),
            &budget,
            &mut buffer,
            &mut || active(),
        )?;
        inventory.writer_lease().check_before_native_mutation(&active)?;
        let version = {
            let mut current = super::super::open_native_read(plan.target())
                .map_err(|_| "Native backup source unavailable during readback")?;
            super::super::native_source_version(plan.target(), &mut current, &mut buffer, &active)?
        };
        if version != expected {
            return Err("Destination changed while saving. Current work preserved; use a new Save As path.".into());
        }
        drop(buffer);
        active()?;
        let fact = NativeExternalFile::observe_held(&file)?;
        let result = Self {
            plan,
            parent,
            file,
            fact,
            sealed,
            _creation: creation,
            session,
            _headers: headers,
            #[cfg(windows)]
            _pins: pins,
        };
        inventory.retain_created_backup(&result, check)?;
        Ok(result)
    }
    pub(in crate::file_safety) fn inventory_binding(
        &self,
    ) -> (&PublicationPlan, &File, NativeExternalFile) {
        (&self.plan, &self.file, self.fact)
    }
    pub(crate) fn recheck(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        let active = || {
            check()?;
            self.session.cleanup_check()
        };
        active()?;
        directory(
            &self.parent,
            self.plan.backup_directory(),
            self.plan.backup_directory_identity(),
        )?;
        let path = self.plan.backup_directory().join(
            self.plan
                .backup_name()
                .ok_or("Native backup name missing")?,
        );
        named(&self.file, &path, self.fact)?;
        self.sealed.check(&self.file)?;
        let expected = self
            .plan
            .expected_version()
            .ok_or("Native backup source version missing")?;
        let _sealed = super::super::owned_rename::Sealed::verify(
            &self.file,
            expected.bytes,
            expected.sha256(),
            &self.session.resident(),
            &mut || active(),
        )?;
        named(&self.file, &path, self.fact)?;
        active()
    }
}

#[cfg(test)]
#[path = "native_backup_data_tests.rs"]
mod tests;

pub(super) fn source_matches(
    source: &File,
    plan: &PublicationPlan,
    expected: super::super::Version,
) -> Result<(), String> {
    let actual = NativeExternalFile::observe_held(source)?;
    if actual.identity() != expected.identity
        || actual.bytes() != expected.bytes
        || actual.modified_nanos() != expected.modified_nanos()
        || super::super::link_count(source)? != 1
    {
        return Err("Native backup source identity or stamp changed".into());
    }
    named(source, plan.target(), actual)
}

fn create_backup(parent: &File, path: &Path, name: &OsStr) -> Result<File, String> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        let _ = parent;
        super::super::owned_rename::staging_options()
            .access_mode(0xC0070000)
            .open(path.join(name))
            .map_err(|_| "Native exclusive backup creation failed; names retained".into())
    }
    #[cfg(unix)]
    {
        create_temporary(parent, path, name, false)
    }
}
struct Creation<'a, 'lease> {
    plan: &'a PublicationPlan,
    inventory: &'a NativeInventory<'lease>,
    file: &'a File,
    parent: &'a File,
    fact: NativeExternalFile,
    session: &'a SourceSession,
}
impl sealed::RootOutcome for Creation<'_, '_> {}
impl ExternalCreationProof for Creation<'_, '_> {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String> {
        self.plan.check_session(self.session)?;
        if (intent, target, growth)
            != (
                self.plan.intent_sha256(),
                self.plan.target_binding(),
                self.plan.growth(),
            )
            || self.fact.bytes() != 0
            || NativeExternalFile::observe_held(self.file)? != self.fact
        {
            return Err("Native backup creation is not its admitted zero-byte owner".into());
        }
        directory(
            self.parent,
            self.plan.backup_directory(),
            self.plan.backup_directory_identity(),
        )?;
        self.inventory
            .validate_created_backup(self.plan, self.file, self.fact.identity(), &|| Ok(()))
    }
    fn fact(&self) -> NativeExternalFile {
        self.fact
    }
    fn role(&self) -> ExternalCreationRole {
        ExternalCreationRole::Backup
    }
    fn name(&self) -> &OsStr {
        self.plan.backup_name().unwrap()
    }
    fn parent_identity(&self) -> (u64, u64) {
        self.plan.backup_directory_identity()
    }
}
