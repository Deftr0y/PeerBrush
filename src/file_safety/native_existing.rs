//! Existing-file staging, retained-original publication and abort proofs.
use super::*;

pub(crate) struct NativeExistingDestination<'inventory, 'lease, 'session> {
    staged: NativeNewDestination<'inventory, 'lease, 'session>,
    backup: NativeBackupData<'session>,
    metadata: metadata::Snapshot,
}
impl<'inventory, 'lease, 'session> NativeExistingDestination<'inventory, 'lease, 'session> {
    pub(crate) fn preflight(
        lease: &super::super::NativeLease,
        budget: &crate::large_document::ResidentBudget,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        lease.validate(check)?;
        let original = open_original(lease.path())?;
        metadata::Snapshot::capture(&original, budget, check)?;
        lease.validate(check)
    }
    pub(crate) fn create(
        session: &'session SourceSession,
        candidate: &CompletedSave36<'_>,
        inventory: &'inventory mut super::super::NativeInventory<'lease>,
        intent: &PublicationIntent<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        candidate.check_session(session)?;
        let observed = inventory.observe_external(check)?;
        observed.check_session(session)?;
        let plan = intent.plan(&observed, check)?;
        plan.check_session(session)?;
        if plan.expected_version().is_none() || plan.backup_name().is_none() {
            return Err("Existing native staging requires an observed original".into());
        }
        let expected = plan.expected_version().unwrap();
        inventory.admit_created_backups(
            expected
                .bytes
                .checked_mul(2)
                .ok_or("Native original/backup coexistence overflow")?,
            2,
        )?;
        let original = open_original(plan.target())?;
        check_target(&plan, Some(&original))?;
        let metadata = metadata::Snapshot::capture(&original, &session.resident(), check)?;
        drop(observed);
        let backup = NativeBackupData::prepare(session, candidate, inventory, intent, check)?;
        let staged = NativeNewDestination::create_plan(
            session,
            candidate,
            inventory,
            intent,
            plan,
            Some(original),
            check,
        )?;
        // Both zero-byte creation owners are already durable. Changing supported
        // access rules here cannot precede either physical registration.
        super::super::native_access::preserve(
            staged
                .original
                .as_ref()
                .ok_or("Native original owner missing")?,
            &staged.temporary,
            super::super::native_access::Destination::Current,
            &session.resident(),
            &|| staged.active(check),
        )?;
        metadata.apply(
            staged
                .original
                .as_ref()
                .ok_or("Native original owner missing")?,
            &staged.temporary,
            &session.resident(),
            check,
        )?;
        backup.recheck(check)?;
        staged.active(check)?;
        Ok(Self {
            staged,
            backup,
            metadata,
        })
    }
    pub(crate) fn replay(
        &mut self,
        candidate: &CompletedSave36<'_>,
        check: &dyn Fn() -> Result<(), String>,
        progress: &mut dyn FnMut(u64) -> Result<(), String>,
    ) -> Result<(), String> {
        let result = (|| {
            self.backup.recheck(check)?;
            self.staged.replay(candidate, check, progress)?;
            self.backup.recheck(check)
        })();
        if result.is_err() {
            self.staged.failed = true;
        }
        result
    }
    pub(crate) fn ready_to_arm(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        self.backup.recheck(check)?;
        self.staged.ready_to_arm(check)?;
        self.validate_metadata(check)
    }
    fn validate_metadata(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        let original = self
            .staged
            .original
            .as_ref()
            .ok_or("Native original owner missing")?;
        let budget = self.staged.inventory.writer_budget();
        self.metadata.validate_original(original, budget, check)?;
        self.metadata
            .validate_current(&self.staged.temporary, budget, check)?;
        super::super::native_access::validate_current(
            original,
            &self.staged.temporary,
            budget,
            check,
        )
    }
    pub(crate) fn abort(
        self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<NativeExistingAbortReceipt<'inventory, 'lease, 'session>, String> {
        self.backup.recheck(check)?;
        let staged = self.staged.abort(check)?;
        let receipt = NativeExistingAbortReceipt {
            staged,
            backup: self.backup,
        };
        receipt.backup.recheck(check)?;
        Ok(receipt)
    }
    pub(crate) fn publish(
        self,
        committed: &CommittedPublication<'_>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<NativeExistingPublicationReceipt<'inventory, 'lease, 'session>, String> {
        let staged = &self.staged;
        let active = || {
            committed.check_plan(&staged.plan)?;
            check()
        };
        active()?;
        if staged.failed || staged.written != staged.plan.candidate_bytes() {
            return Err("Native replacement candidate is incomplete; all files retained".into());
        }
        staged.check_temporary()?;
        staged.inventory.writer_facts(&active)?;
        self.backup.recheck(&active)?;
        self.validate_metadata(&active)?;
        let original = staged
            .original
            .as_ref()
            .ok_or("Native original owner missing")?;
        check_target(&staged.plan, Some(original))?;
        let expected = staged
            .plan
            .expected_version()
            .ok_or("Native original version missing")?;
        let original_seal = super::super::owned_rename::Sealed::verify(
            original,
            expected.bytes,
            expected.sha256(),
            staged.inventory.writer_budget(),
            &mut || {
                active()?;
                check_target(&staged.plan, Some(original))
            },
        )?;
        let candidate_seal = staged
            .sealed
            .as_ref()
            .ok_or("Native replacement candidate is not sealed")?;
        candidate_seal.check(&staged.temporary)?;
        let retained_name = staged
            .plan
            .retained_original_name()
            .ok_or("Native retained original name missing")?;
        component(&retained_name)?;
        let versions = staged.inventory.writer_lease().native_versions_handle()?;
        directory(
            &versions,
            staged.plan.backup_directory(),
            staged.plan.backup_directory_identity(),
        )?;
        #[cfg(windows)]
        let pins = super::super::owned_rename::pin_directories(
            staged.plan.backup_directory(),
            staged.plan.backup_directory_identity(),
        )?;
        absent(&staged.plan.backup_directory().join(&retained_name))?;
        let parent = staged
            .plan
            .target()
            .parent()
            .ok_or("Native target parent missing")?;
        let target = staged
            .plan
            .target()
            .file_name()
            .ok_or("Native target name missing")?;
        self.validate_metadata(&active)?;
        super::super::owned_rename::relocate_new(
            parent,
            &staged.parent,
            staged.plan.backup_directory(),
            &versions,
            Path::new(target),
            Path::new(&retained_name),
            original,
            &original_seal,
            &mut || {
                active()?;
                staged.check_temporary()?;
                staged.inventory.writer_facts(&active)?;
                check_target(&staged.plan, Some(original))
            },
        )?;
        let relocated = || -> Result<(), String> {
            active()?;
            original_seal.check(original)?;
            staged
                .inventory
                .writer_facts_with_original(&staged.plan, original, &active)?;
            Ok(())
        };
        relocated()?;
        self.validate_metadata(&active)?;
        // No rollback overwrites a late human destination. Any uncertain step
        // keeps the original, backup, temporary and full admitted debt in place.
        super::super::owned_rename::publish_new(
            parent,
            &staged.parent,
            Path::new(staged.plan.temporary_name()),
            Path::new(target),
            &staged.temporary,
            candidate_seal,
            &mut || relocated(),
        )?;
        let candidate_seal = super::super::owned_rename::Sealed::verify(
            &staged.temporary,
            staged.plan.candidate_bytes(),
            staged.plan.candidate_sha256(),
            staged.inventory.writer_budget(),
            &mut || relocated(),
        )?;
        let fact = NativeExternalFile::observe_held(&staged.temporary)?;
        self.validate_metadata(&active)?;
        named(&staged.temporary, staged.plan.target(), fact)?;
        absent(&parent.join(staged.plan.temporary_name()))?;
        let mut facts =
            staged
                .inventory
                .writer_facts_with_original(&staged.plan, original, &active)?;
        facts.push(fact);
        facts.sort_by_key(|f| f.identity());
        if facts.windows(2).any(|w| w[0].identity() == w[1].identity()) {
            return Err("Native replacement outcome has ambiguous physical owners".into());
        }
        let version = super::super::Version {
            identity: fact.identity(),
            bytes: fact.bytes(),
            modified: fact.modified_nanos(),
            fingerprint: staged.plan.candidate_sha256(),
        };
        let receipt = NativeExistingPublicationReceipt {
            writer: self,
            versions,
            original_seal,
            candidate_seal,
            fact,
            facts,
            version,
            #[cfg(windows)]
            _pins: pins,
        };
        receipt.revalidate(&|| receipt.writer.staged.session.cleanup_check())?;
        Ok(receipt)
    }
}

pub(crate) struct NativeExistingPublicationReceipt<'inventory, 'lease, 'session> {
    writer: NativeExistingDestination<'inventory, 'lease, 'session>,
    versions: File,
    original_seal: super::super::owned_rename::Sealed,
    candidate_seal: super::super::owned_rename::Sealed,
    fact: NativeExternalFile,
    facts: Vec<NativeExternalFile>,
    version: super::super::Version,
    #[cfg(windows)]
    _pins: Vec<File>,
}
impl NativeExistingPublicationReceipt<'_, '_, '_> {
    pub(crate) fn version(&self) -> super::super::Version {
        self.version
    }
    fn revalidate(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        let staged = &self.writer.staged;
        check()?;
        directory(
            &self.versions,
            staged.plan.backup_directory(),
            staged.plan.backup_directory_identity(),
        )?;
        directory(
            &staged.parent,
            staged
                .plan
                .target()
                .parent()
                .ok_or("Native parent missing")?,
            staged.plan.parent_identity(),
        )?;
        self.writer.backup.recheck(check)?;
        self.writer.validate_metadata(check)?;
        let original = staged
            .original
            .as_ref()
            .ok_or("Retained native original owner missing")?;
        self.original_seal.check(original)?;
        let mut facts =
            staged
                .inventory
                .writer_facts_with_original(&staged.plan, original, check)?;
        let expected = staged
            .plan
            .expected_version()
            .ok_or("Retained native original version missing")?;
        let _original = super::super::owned_rename::Sealed::verify(
            original,
            expected.bytes,
            expected.sha256(),
            staged.inventory.writer_budget(),
            &mut || check(),
        )?;
        self.candidate_seal.check(&staged.temporary)?;
        let _candidate = super::super::owned_rename::Sealed::verify(
            &staged.temporary,
            staged.plan.candidate_bytes(),
            staged.plan.candidate_sha256(),
            staged.inventory.writer_budget(),
            &mut || check(),
        )?;
        named(&staged.temporary, staged.plan.target(), self.fact)?;
        absent(
            &staged
                .plan
                .target()
                .parent()
                .ok_or("Native parent missing")?
                .join(staged.plan.temporary_name()),
        )?;
        facts.push(self.fact);
        facts.sort_by_key(|f| f.identity());
        if facts != self.facts {
            return Err("Native replacement physical debt changed; all versions retained".into());
        }
        self.writer.validate_metadata(check)?;
        check()
    }
}
impl sealed::RootOutcome for NativeExistingPublicationReceipt<'_, '_, '_> {}
impl ExternalOutcomeProof for NativeExistingPublicationReceipt<'_, '_, '_> {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String> {
        let plan = &self.writer.staged.plan;
        if (intent, target, growth) != (plan.intent_sha256(), plan.target_binding(), plan.growth())
        {
            return Err("Native replacement outcome belongs to another intent".into());
        }
        self.revalidate(&|| self.writer.staged.session.cleanup_check())
    }
    fn facts(&self) -> &[NativeExternalFile] {
        &self.facts
    }
    fn retired(&self) -> &[(u64, u64)] {
        &[]
    }
}

fn open_original(path: &Path) -> Result<File, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // DELETE permits handle-based relocation; denying write sharing keeps
        // other writers out while the original and resulting receipt are held.
        options
            .access_mode(0xC0010000)
            .share_mode(5)
            .custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .map_err(|_| "Native original is busy or cannot be retained for relocation")?;
    super::super::native_regular(&file)
        .map_err(|_| "Native original is not a regular exclusive owner")?;
    super::super::native_access::supported(&file)?;
    Ok(file)
}

pub(crate) struct NativeExistingAbortReceipt<'inventory, 'lease, 'session> {
    staged: NativeAbortReceipt<'inventory, 'lease>,
    backup: NativeBackupData<'session>,
}
impl sealed::RootOutcome for NativeExistingAbortReceipt<'_, '_, '_> {}
impl ExternalOutcomeProof for NativeExistingAbortReceipt<'_, '_, '_> {
    fn validate(&self, intent: [u8; 32], target: [u8; 32], growth: u64) -> Result<(), String> {
        self.backup.recheck(&|| Ok(()))?;
        self.staged.validate(intent, target, growth)
    }
    fn facts(&self) -> &[NativeExternalFile] {
        self.staged.facts()
    }
    fn retired(&self) -> &[(u64, u64)] {
        self.staged.retired()
    }
}

#[cfg(test)]
#[path = "native_existing_tests.rs"]
mod tests;
