//! Freeze external destination/backup debt before admitting a native save.
use super::*;
use crate::large_document::{Reservation, ResidentBudget};
use std::{ffi::OsString, time::SystemTime};

const MAX_ENTRIES: usize = 4096;
// Two simultaneous 4096x1024-byte name sets, entry/vector headers and bounded
// transient path observations must all fit, rather than just the name payloads.
const INVENTORY_BYTES: usize = 12 * 1024 * 1024;
#[path = "native_backup.rs"]
mod native_backup;
#[path = "native_observation.rs"]
mod native_observation;
pub(crate) use native_observation::{NativeExternalFile, NativeExternalSnapshot};
struct Entry {
    name: OsString,
    file: fs::File,
    identity: (u64, u64),
    bytes: u64,
    modified: SystemTime,
}
pub(crate) struct NativeInventory<'a> {
    lease: &'a NativeLease,
    directory: PathBuf,
    held_directory: fs::File,
    directory_identity: (u64, u64),
    quota: LockedFile,
    quota_identity: (u64, u64),
    entries: Vec<Entry>,
    backup_bytes: u64,
    budget: ResidentBudget,
    // Names, both enumeration vectors and retained file metadata are bounded
    // before their allocations. Kernel handle memory is outside this pool.
    _charge: Reservation,
}
impl NativeLease {
    pub(crate) fn freeze_inventory(
        &self,
        budget: &ResidentBudget,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<NativeInventory<'_>, String> {
        check()?;
        let charge = budget
            .reserve_bytes(INVENTORY_BYTES, INVENTORY_BYTES)
            .map_err(|e| e.to_string())?;
        let directory = self
            .path()
            .parent()
            .ok_or("Missing native destination parent")?
            .join(".peerbrush-versions");
        regular_directory(&directory)?;
        let held_directory = open_directory(&directory)?;
        let directory_identity = identity(&held_directory)?;
        let quota_path = directory.join("quota.lock");
        if fs::symlink_metadata(&quota_path).is_ok() && !regular(&quota_path) {
            return Err("Native previous-save quota is not a regular file".into());
        }
        let mut quota_options = fs::OpenOptions::new();
        quota_options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            quota_options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            quota_options.custom_flags(0x00200000);
        }
        let quota_file = quota_options.open(&quota_path).map_err(|e| e.to_string())?;
        quota_file.try_lock().map_err(|_| {
            "Previous-save inventory is busy; retry without changing the destination"
        })?;
        let quota = LockedFile(quota_file);
        let quota_identity = identity(&quota.0)?;
        if quota.0.metadata().map_err(|e| e.to_string())?.len() != 0 || link_count(&quota.0)? != 1 {
            return Err("Native previous-save quota ownership changed".into());
        }
        let mut inventory = NativeInventory {
            lease: self,
            directory,
            held_directory,
            directory_identity,
            quota,
            quota_identity,
            entries: Vec::new(),
            backup_bytes: 0,
            budget: budget.clone(),
            _charge: charge,
        };
        inventory
            .entries
            .try_reserve_exact(MAX_ENTRIES)
            .map_err(|_| "Native save inventory allocation refused")?;
        for name in inventory.names(check)? {
            check()?;
            let entry = Entry::capture(&inventory.directory, name)?;
            inventory.backup_bytes = inventory
                .backup_bytes
                .checked_add(entry.bytes)
                .ok_or("Native previous-save inventory byte overflow")?;
            inventory.entries.push(entry);
        }
        if inventory.backup_bytes > FilePolicy::Source36.backup_bytes() {
            return Err("Source36 previous-save storage exceeds its directory admission".into());
        }
        inventory.validate(check)?;
        Ok(inventory)
    }
}
fn regular_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Native previous-save directory alias refused".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err("Native previous-save directory reparse point refused".into());
        }
    }
    Ok(())
}
fn open_directory(path: &Path) -> Result<fs::File, String> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(
            libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Metadata-only directory handles require BACKUP_SEMANTICS. Opening the
        // reparse point itself lets the separate named checks reject aliases.
        options.access_mode(0).custom_flags(0x02000000 | 0x00200000);
    }
    options.open(path).map_err(|e| e.to_string())
}
fn open_inventory_file(path: &Path) -> Result<fs::File, String> {
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .map_err(|e| e.to_string())?
    };
    #[cfg(not(unix))]
    let file = open_for_identity(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("Native save inventory file type changed".into());
    }
    Ok(file)
}
impl Entry {
    fn capture(directory: &Path, name: OsString) -> Result<Self, String> {
        let path = directory.join(&name);
        if !regular(&path) {
            return Err("Native previous-save inventory contains an alias or directory".into());
        }
        let file = open_inventory_file(&path)?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        let entry = Self {
            name,
            identity: identity(&file)?,
            bytes: metadata.len(),
            modified: metadata.modified().map_err(|e| e.to_string())?,
            file,
        };
        entry.validate(directory)?;
        Ok(entry)
    }
    fn validate(&self, directory: &Path) -> Result<(), String> {
        let path = directory.join(&self.name);
        let metadata = self.file.metadata().map_err(|e| e.to_string())?;
        if !regular(&path) {
            return Err("Native save inventory name changed type".into());
        }
        let named = open_inventory_file(&path)?;
        if !regular(&path)
            || identity(&named)? != self.identity
            || identity(&self.file)? != self.identity
            || link_count(&named)? != 1
            || link_count(&self.file)? != 1
            || metadata.len() != self.bytes
            || metadata.modified().ok() != Some(self.modified)
        {
            return Err("Native previous-save inventory changed; all files retained".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("Native previous-save inventory reparse point refused".into());
            }
        }
        Ok(())
    }
}
impl NativeInventory<'_> {
    pub(in crate::file_safety) fn writer_lease(&self) -> &NativeLease {
        self.lease
    }
    pub(in crate::file_safety) fn writer_budget(&self) -> &ResidentBudget {
        &self.budget
    }
    /// Final accounting observes the original backup set independently of the
    /// destination lease, whose old version is intentionally replaced on success.
    pub(in crate::file_safety) fn writer_facts(
        &self,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Vec<NativeExternalFile>, String> {
        self.validate_files(check)?;
        let mut facts = Vec::new();
        facts
            .try_reserve_exact(self.entries.len() + 1)
            .map_err(|_| "Native outcome fact allocation refused")?;
        if facts.capacity() > MAX_ENTRIES + 1 {
            return Err("Native outcome fact allocation exceeds admission".into());
        }
        for entry in &self.entries {
            check()?;
            facts.push(NativeExternalFile::observe_held(&entry.file)?);
        }
        self.validate_files(check)?;
        Ok(facts)
    }
    fn names(&self, check: &dyn Fn() -> Result<(), String>) -> Result<Vec<OsString>, String> {
        let mut names = Vec::new();
        names
            .try_reserve_exact(MAX_ENTRIES)
            .map_err(|_| "Native inventory enumeration allocation refused")?;
        for item in fs::read_dir(&self.directory).map_err(|e| e.to_string())? {
            check()?;
            let name = item.map_err(|e| e.to_string())?.file_name();
            if names.len() == MAX_ENTRIES || name.as_encoded_bytes().len() > 1024 {
                return Err(
                    "Native previous-save inventory exceeds bounded entry admission".into(),
                );
            }
            names.push(name);
        }
        names.sort();
        Ok(names)
    }
    pub(crate) fn backup_bytes(&self) -> u64 {
        self.backup_bytes
    }
    /// Existing files remain disk debt, including the old destination even
    /// though a successful replacement will later unlink that directory entry.
    pub(crate) fn external_bytes(&self) -> Result<u64, String> {
        self.backup_bytes
            .checked_add(self.lease.expected().map_or(0, |v| v.bytes))
            .ok_or_else(|| "Native external file inventory overflow".into())
    }
    pub(crate) fn prospective_bytes(
        &self,
        destination_temporary_bytes: u64,
    ) -> Result<u64, String> {
        self.external_bytes()?
            .checked_add(self.lease.expected().map_or(0, |v| v.bytes))
            .and_then(|v| v.checked_add(destination_temporary_bytes))
            .ok_or_else(|| "Native destination coexistence overflow".into())
    }
    pub(crate) fn validate(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        self.validate_files(check)?;
        self.lease.validate(check)?;
        check()
    }
    fn validate_files(&self, check: &dyn Fn() -> Result<(), String>) -> Result<(), String> {
        check()?;
        regular_directory(&self.directory)?;
        if identity(&self.held_directory)? != self.directory_identity
            || identity(&open_directory(&self.directory)?)? != self.directory_identity
            || identity(&self.quota.0)? != self.quota_identity
            || identity(&open_inventory_file(&self.directory.join("quota.lock"))?)?
                != self.quota_identity
        {
            return Err("Native previous-save inventory owner changed; files retained".into());
        }
        let names = self.names(check)?;
        if names.len() != self.entries.len()
            || names
                .iter()
                .zip(&self.entries)
                .any(|(name, entry)| name != &entry.name)
        {
            return Err("Native previous-save inventory membership changed; files retained".into());
        }
        for entry in &self.entries {
            check()?;
            entry.validate(&self.directory)?;
        }
        check()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (PathBuf, PathBuf, ResidentBudget) {
        let dir = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pb-native-inventory-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("source.psb");
        fs::write(&path, b"old native source").unwrap();
        (dir, path, ResidentBudget::new(16 * 1024 * 1024).unwrap())
    }
    #[test]
    fn frozen_debt_counts_all_files_and_releases_pool_after_last_owner() {
        let (dir, path, budget) = setup();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        fs::write(
            dir.join(".peerbrush-versions/retained.bin"),
            b"existing backup",
        )
        .unwrap();
        let inventory = lease.freeze_inventory(&budget, &|| Ok(())).unwrap();
        assert_eq!(inventory.backup_bytes(), 15);
        assert_eq!(inventory.external_bytes().unwrap(), 32);
        assert_eq!(inventory.prospective_bytes(71).unwrap(), 120);
        assert!(inventory.prospective_bytes(u64::MAX).is_err());
        assert_eq!(budget.used(), INVENTORY_BYTES);
        assert!(lease.freeze_inventory(&budget, &|| Ok(())).is_err());
        inventory.validate(&|| Ok(())).unwrap();
        drop(inventory);
        assert_eq!(budget.used(), 0);
        drop(lease);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn foreign_same_bytes_replacement_and_new_membership_are_retained() {
        for replace in [false, true] {
            let (dir, path, budget) = setup();
            let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
            let backup = dir.join(".peerbrush-versions/retained.bin");
            fs::write(&backup, b"same bytes").unwrap();
            let inventory = lease.freeze_inventory(&budget, &|| Ok(())).unwrap();
            if replace {
                fs::rename(&backup, dir.join("held-old.bin")).unwrap();
                fs::write(&backup, b"same bytes").unwrap();
            } else {
                fs::write(dir.join(".peerbrush-versions/foreign.bin"), b"foreign").unwrap();
            }
            assert!(inventory.validate(&|| Ok(())).is_err());
            assert_eq!(fs::read(&backup).unwrap(), b"same bytes");
            drop(inventory);
            drop(lease);
            fs::remove_dir_all(dir).unwrap();
        }
    }
    #[test]
    fn insufficient_residency_and_cancel_refuse_before_inventory_creation() {
        let (dir, path, budget) = setup();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        assert!(lease
            .freeze_inventory(&ResidentBudget::new(1).unwrap(), &|| Ok(()))
            .is_err());
        assert!(!dir.join(".peerbrush-versions/quota.lock").exists());
        assert!(lease
            .freeze_inventory(&budget, &|| Err("cancel".into()))
            .is_err());
        assert!(!dir.join(".peerbrush-versions/quota.lock").exists());
        assert_eq!(budget.used(), 0);
        drop(lease);
        fs::remove_dir_all(dir).unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn fifo_identity_recheck_returns_without_waiting_for_a_writer() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, time::Duration};
        let (dir, path, _) = setup();
        let fifo = dir.join("replaced.pipe");
        let named = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(named.as_ptr(), 0o600) }, 0);
        let (send, receive) = mpsc::channel();
        std::thread::spawn(move || {
            let refused = open_inventory_file(&fifo).is_err();
            let _ = send.send(refused);
        });
        assert!(receive
            .recv_timeout(Duration::from_secs(2))
            .expect("FIFO inventory open blocked cancellation"));
        fs::remove_file(path).unwrap();
        fs::remove_file(dir.join("replaced.pipe")).unwrap();
        fs::remove_dir(dir).unwrap();
    }
}
