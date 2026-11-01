//! Live held observations for quota journals, never deserialized file authority.
use super::*;

/// These values describe physical debt, not authenticated editable pixels.
/// Only a live snapshot revalidation can authenticate their current ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(crate) struct NativeExternalFile {
    identity: (u64, u64),
    bytes: u64,
    modified_nanos: u128,
}
impl NativeExternalFile {
    pub(in crate::file_safety) fn observe_held(file: &fs::File) -> Result<Self, String> {
        let (bytes, modified_nanos) = stamp(file)?;
        if link_count(file)? != 1 {
            return Err("Native outcome file link ownership changed".into());
        }
        Ok(Self {
            identity: identity(file)?,
            bytes,
            modified_nanos,
        })
    }
    #[cfg(test)]
    pub(crate) fn for_accounting_tests(
        identity: (u64, u64),
        bytes: u64,
        modified_nanos: u128,
    ) -> Self {
        Self {
            identity,
            bytes,
            modified_nanos,
        }
    }
    pub(crate) fn identity(self) -> (u64, u64) {
        self.identity
    }
    pub(crate) fn bytes(self) -> u64 {
        self.bytes
    }
    pub(crate) fn modified_nanos(self) -> u128 {
        self.modified_nanos
    }
}
pub(crate) struct NativeExternalSnapshot<'a> {
    inventory: &'a NativeInventory<'a>,
    parent: fs::File,
    parent_identity: (u64, u64),
    source: Option<fs::File>,
    // Keep source/directory handles and the borrowed inventory until every
    // observation is consumed; the small path/index envelope drops last.
    _charge: Reservation,
}
impl NativeInventory<'_> {
    pub(crate) fn observe_external<'a>(
        &'a self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<NativeExternalSnapshot<'a>, String> {
        self.validate(check)?;
        let charge = self
            .budget
            .reserve_bytes(256 * 1024, 256 * 1024)
            .map_err(|e| e.to_string())?;
        let path = self
            .lease
            .path()
            .parent()
            .ok_or("Missing external destination parent")?;
        regular_directory(path)?;
        let parent = open_directory(path)?;
        let parent_identity = identity(&parent)?;
        let source = self
            .lease
            .expected()
            .map(|expected| -> Result<fs::File, String> {
                let file = open_native_read(self.lease.path()).map_err(|e| e.to_string())?;
                if identity(&file)? != expected.identity
                    || stamp(&file)? != (expected.bytes, expected.modified)
                    || link_count(&file)? != 1
                {
                    return Err("External destination changed before debt capture".into());
                }
                Ok(file)
            })
            .transpose()?;
        let snapshot = NativeExternalSnapshot {
            inventory: self,
            parent,
            parent_identity,
            source,
            _charge: charge,
        };
        snapshot.revalidate(check)?;
        Ok(snapshot)
    }
}
impl NativeExternalSnapshot<'_> {
    pub(crate) fn uses_budget(&self, budget: &ResidentBudget) -> bool {
        self.inventory.budget.same_pool(budget) && self._charge.uses_budget(budget)
    }
    pub(crate) fn target(&self) -> &Path {
        self.inventory.lease.path()
    }
    pub(crate) fn destination_parent_identity(&self) -> (u64, u64) {
        self.parent_identity
    }
    pub(crate) fn backup_directory(&self) -> &Path {
        &self.inventory.directory
    }
    pub(crate) fn backup_directory_identity(&self) -> (u64, u64) {
        self.inventory.directory_identity
    }
    pub(crate) fn expected(&self) -> Option<Version> {
        self.inventory.lease.expected()
    }
    pub(crate) fn source(&self) -> Option<NativeExternalFile> {
        self.expected().map(|v| NativeExternalFile {
            identity: v.identity,
            bytes: v.bytes,
            modified_nanos: v.modified,
        })
    }
    /// Caller preadmits its serialization/index before copying any names/facts.
    pub(crate) fn backups(
        &self,
    ) -> impl ExactSizeIterator<Item = (&std::ffi::OsStr, NativeExternalFile)> {
        self.inventory.entries.iter().map(|e| {
            (
                e.name.as_os_str(),
                NativeExternalFile {
                    identity: e.identity,
                    bytes: e.bytes,
                    modified_nanos: e
                        .modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_nanos(),
                },
            )
        })
    }
    /// Metadata/name ownership checks only. The final save writer separately
    /// repeats full source/candidate/backup SHA checks outside the Engine lock.
    pub(crate) fn revalidate(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        check()?;
        self.inventory.validate_files(check)?;
        check()?;
        // A caller callback cannot run after this last complete ownership pass.
        self.inventory.validate_files(&|| Ok(()))?;
        let path = self
            .target()
            .parent()
            .ok_or("Missing observed destination parent")?;
        regular_directory(path)?;
        if identity(&self.parent)? != self.parent_identity
            || identity(&open_directory(path)?)? != self.parent_identity
        {
            return Err("External destination parent changed; debt retained".into());
        }
        match (&self.source, self.expected()) {
            (Some(file), Some(expected)) => {
                let named = open_native_identity(self.target()).map_err(|e| e.to_string())?;
                if !regular(self.target())
                    || identity(file)? != expected.identity
                    || identity(&named)? != expected.identity
                    || stamp(file)? != (expected.bytes, expected.modified)
                    || link_count(file)? != 1
                    || link_count(&named)? != 1
                {
                    return Err("External destination changed; debt retained".into());
                }
            }
            (None, None)
                if matches!(fs::symlink_metadata(self.target()), Err(e)
                if e.kind() == std::io::ErrorKind::NotFound) =>
            {
                ()
            }
            _ => return Err("External destination admission changed; debt retained".into()),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (PathBuf, PathBuf, ResidentBudget) {
        let dir = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "pb-native-external-observation-{}",
                crate::engine::id()
            ));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("source.psb");
        fs::write(&path, b"exact current bytes").unwrap();
        (dir, path, ResidentBudget::new(16 * 1024 * 1024).unwrap())
    }
    fn cleanup(dir: &Path) {
        assert_eq!(
            dir.parent().unwrap(),
            fs::canonicalize(std::env::temp_dir()).unwrap()
        );
        assert_eq!(fs::canonicalize(dir).unwrap(), dir);
        assert!(dir
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pb-native-external-observation-"));
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn live_debt_observes_all_physical_files_and_releases_before_backup_mutation() {
        let (dir, path, pool) = setup();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        fs::write(dir.join(".peerbrush-versions/older.psb"), b"older").unwrap();
        let mut inventory = lease.freeze_inventory(&pool, &|| Ok(())).unwrap();
        {
            let snapshot = inventory.observe_external(&|| Ok(())).unwrap();
            assert!(snapshot.uses_budget(&pool.clone()));
            assert!(!snapshot.uses_budget(&ResidentBudget::new(16 * 1024 * 1024).unwrap()));
            snapshot.revalidate(&|| Ok(())).unwrap();
            assert_eq!(snapshot.target(), path);
            assert_eq!(
                snapshot.source().unwrap().identity(),
                lease.expected().unwrap().identity
            );
            assert_eq!(snapshot.source().unwrap().bytes(), 19);
            assert_eq!(
                snapshot.expected().unwrap().sha256(),
                lease.expected().unwrap().sha256()
            );
            assert_eq!(
                snapshot.source().unwrap().modified_nanos(),
                snapshot.expected().unwrap().modified_nanos()
            );
            assert_eq!(
                snapshot.destination_parent_identity(),
                identity(&open_directory(&dir).unwrap()).unwrap()
            );
            assert_eq!(
                snapshot.backup_directory_identity(),
                identity(&open_directory(snapshot.backup_directory()).unwrap()).unwrap()
            );
            let total: u64 = snapshot.backups().map(|(_, f)| f.bytes()).sum();
            assert_eq!(total, 5);
            assert!(snapshot.backups().any(|(name, _)| name == "quota.lock"));
        }
        // The snapshot is an observation borrow, not an irreversible alias or
        // a whole-lease-lifetime borrow that prevents the legitimate next step.
        let backup = inventory.preserve_current(&|| Ok(())).unwrap().unwrap();
        assert_eq!(fs::read(backup).unwrap(), b"exact current bytes");
        drop(inventory);
        drop(lease);
        assert_eq!(pool.used(), 0);
        cleanup(&dir);
    }
    #[test]
    fn observed_source_or_backup_name_replacement_never_becomes_quota_authority() {
        for replace_source in [false, true] {
            let (dir, path, pool) = setup();
            let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
            let older = dir.join(".peerbrush-versions/older.psb");
            fs::write(&older, b"older").unwrap();
            let inventory = lease.freeze_inventory(&pool, &|| Ok(())).unwrap();
            let snapshot = inventory.observe_external(&|| Ok(())).unwrap();
            let replace = if replace_source { &path } else { &older };
            let bytes = fs::read(replace).unwrap();
            fs::rename(replace, dir.join("retained-previous-identity")).unwrap();
            fs::write(replace, &bytes).unwrap();
            assert!(snapshot.revalidate(&|| Ok(())).is_err());
            assert_eq!(fs::read(replace).unwrap(), bytes);
            drop(snapshot);
            drop(inventory);
            drop(lease);
            cleanup(&dir);
        }
    }
    #[test]
    fn newly_created_destination_and_late_membership_or_cancel_are_refused() {
        let (dir, path, pool) = setup();
        fs::remove_file(&path).unwrap();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let inventory = lease.freeze_inventory(&pool, &|| Ok(())).unwrap();
        let snapshot = inventory.observe_external(&|| Ok(())).unwrap();
        assert!(snapshot.source().is_none());
        snapshot.revalidate(&|| Ok(())).unwrap();
        assert!(snapshot
            .revalidate(&|| Err("cancel exact observation".into()))
            .is_err());
        fs::write(&path, b"foreign destination").unwrap();
        assert!(snapshot.revalidate(&|| Ok(())).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(dir.join(".peerbrush-versions/foreign.bin"), b"foreign debt").unwrap();
        assert!(snapshot.revalidate(&|| Ok(())).is_err());
        drop(snapshot);
        drop(inventory);
        drop(lease);
        cleanup(&dir);
    }
}
