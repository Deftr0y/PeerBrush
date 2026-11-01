//! Cooperative save guards and byte-exact previous-file preservation.
//! Fingerprints detect accidental changes; they are not authentication hashes.
use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};
#[path = "file_permissions.rs"]
mod permissions;
pub(crate) use permissions::preserve as preserve_permissions;
#[cfg(windows)]
pub(crate) use permissions::wide_absolute_file_path;

#[derive(Clone, Copy)]
enum FilePolicy {
    Legacy,
    Source36,
}
impl FilePolicy {
    fn file_bytes(self) -> u64 {
        match self {
            Self::Legacy => 256 * 1024 * 1024,
            Self::Source36 => 64 * 1024 * 1024 * 1024,
        }
    }
    fn backup_bytes(self) -> u64 {
        match self {
            Self::Legacy => 1024 * 1024 * 1024,
            Self::Source36 => 64 * 1024 * 1024 * 1024,
        }
    }
    fn exceeded(self) -> &'static str {
        match self {
            Self::Legacy => "File exceeds the 256 MiB safety-check limit; original retained",
            Self::Source36 => {
                "File exceeds the 64 GiB Source36 safety-check limit; original retained"
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub identity: (u64, u64),
    pub bytes: u64,
    modified: u128,
    fingerprint: [u8; 32],
}

pub(crate) fn open_for_identity(path: &Path) -> std::io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const OPEN_REPARSE_POINT: u32 = 0x00200000;
        // Repeated identity checks need metadata, without acquiring file-data access.
        options.access_mode(0).custom_flags(OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(unix)]
pub(crate) fn identity(file: &fs::File) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    Ok((m.dev(), m.ino()))
}
#[cfg(windows)]
fn file_identity_and_links(file: &fs::File) -> Result<((u64, u64), u64), String> {
    use std::os::windows::io::AsRawHandle;
    #[repr(C)]
    #[derive(Default)]
    struct Info {
        attributes: u32,
        created: [u32; 2],
        accessed: [u32; 2],
        written: [u32; 2],
        volume: u32,
        size_high: u32,
        size_low: u32,
        links: u32,
        index_high: u32,
        index_low: u32,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetFileInformationByHandle(handle: *mut std::ffi::c_void, info: *mut Info) -> i32;
    }
    let mut info = Info::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    Ok((
        (
            info.volume as u64,
            ((info.index_high as u64) << 32) | info.index_low as u64,
        ),
        info.links as u64,
    ))
}

#[cfg(windows)]
pub(crate) fn identity(file: &fs::File) -> Result<(u64, u64), String> {
    file_identity_and_links(file).map(|info| info.0)
}
#[cfg(windows)]
pub(crate) fn link_count(file: &fs::File) -> Result<u64, String> {
    file_identity_and_links(file).map(|info| info.1)
}
#[cfg(unix)]
pub(crate) fn link_count(file: &fs::File) -> Result<u64, String> {
    use std::os::unix::fs::MetadataExt;
    file.metadata()
        .map(|metadata| metadata.nlink())
        .map_err(|error| error.to_string())
}

fn stamp(file: &fs::File) -> Result<(u64, u128), String> {
    let m = file.metadata().map_err(|e| e.to_string())?;
    if !m.is_file() {
        return Err("Destination is not a regular file".into());
    }
    Ok((
        m.len(),
        m.modified()
            .map_err(|e| e.to_string())?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    ))
}
pub(crate) fn hash(bytes: &[u8], state: &mut [u64; 2]) {
    for &b in bytes {
        state[0] = (state[0] ^ b as u64).wrapping_mul(0x100000001b3);
        state[1] = (state[1] ^ b as u64).wrapping_mul(0x9e3779b185ebca87);
    }
}
pub(crate) fn finish_fingerprint(context: ring::digest::Context) -> [u8; 32] {
    context.finish().as_ref().try_into().unwrap()
}
pub fn version(path: &Path) -> Result<Version, String> {
    version_with_check(path, || Ok(()))
}
pub(crate) fn version_with_check(
    path: &Path,
    check: impl Fn() -> Result<(), String>,
) -> Result<Version, String> {
    version_bounded(path, FilePolicy::Legacy, check)
}
fn version_bounded(
    path: &Path,
    policy: FilePolicy,
    check: impl Fn() -> Result<(), String>,
) -> Result<Version, String> {
    check()?;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let identity = identity(&file)?;
    let before = stamp(&file)?;
    if before.0 > policy.file_bytes() {
        return Err(policy.exceeded().into());
    }
    let mut fingerprint = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        check()?;
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > policy.file_bytes() {
            return Err("File grew beyond its safety-check limit; original retained".into());
        }
        fingerprint.update(&buffer[..n]);
    }
    check()?;
    if stamp(&file)? != before
        || total != before.0
        || identity != self::identity(&fs::File::open(path).map_err(|e| e.to_string())?)?
    {
        return Err("File changed while checking its version; current work preserved".into());
    }
    Ok(Version {
        identity,
        bytes: total,
        modified: before.1,
        fingerprint: finish_fingerprint(fingerprint),
    })
}
pub fn snapshot(path: &Path) -> Result<Option<Version>, String> {
    snapshot_bounded(path, FilePolicy::Legacy, || Ok(()))
}
fn snapshot_bounded(
    path: &Path,
    policy: FilePolicy,
    check: impl Fn() -> Result<(), String>,
) -> Result<Option<Version>, String> {
    check()?;
    match fs::metadata(path) {
        Ok(_) => version_bounded(path, policy, check).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
pub(crate) fn published_version(path: &Path, bytes: &[u8]) -> Result<Version, String> {
    let mut expected = ring::digest::Context::new(&ring::digest::SHA256);
    expected.update(bytes);
    verified_version(
        path,
        bytes.len() as u64,
        finish_fingerprint(expected),
        || Ok(()),
    )
}
pub(crate) fn verified_version(
    path: &Path,
    bytes: u64,
    expected: [u8; 32],
    check: impl Fn() -> Result<(), String>,
) -> Result<Version, String> {
    let actual = version_with_check(path, check)?;
    if actual.bytes != bytes || actual.fingerprint != expected {
        return Err("Published file changed before verification. Current work remains open; inspect the destination and use Save As.".into());
    }
    Ok(actual)
}
pub fn canonical_destination(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return fs::canonicalize(path).map_err(|e| e.to_string());
    }
    if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Destination is a broken file alias; choose a regular file path".into());
    }
    let name = path.file_name().ok_or("Destination needs a file name")?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    Ok(fs::canonicalize(parent)
        .map_err(|e| e.to_string())?
        .join(name))
}
pub fn same_destination(a: &Path, b: &Path) -> Result<bool, String> {
    if canonical_destination(a)? == canonical_destination(b)? {
        return Ok(true);
    }
    Ok(match (snapshot(a)?, snapshot(b)?) {
        (Some(a), Some(b)) => a.identity == b.identity,
        _ => false,
    })
}
fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
}
pub(crate) fn destination_key(path: &Path) -> [u64; 2] {
    let mut key = [0xcbf29ce484222325, 0x84222325cbf29ce4];
    #[cfg(windows)]
    hash(
        path.as_os_str().to_string_lossy().to_lowercase().as_bytes(),
        &mut key,
    );
    #[cfg(not(windows))]
    hash(path.as_os_str().as_encoded_bytes(), &mut key);
    key
}

/// A stable lock inode is never unlinked while another process might hold it.
struct LockedFile(fs::File);
impl Drop for LockedFile {
    fn drop(&mut self) {
        // A concurrently launched process can inherit a descriptor. Release
        // our cooperative lease explicitly before its originating handle closes.
        let _ = self.0.unlock();
    }
}
pub struct Lease {
    _file: LockedFile,
    pub path: PathBuf,
    pub expected: Option<Version>,
    policy: FilePolicy,
}
impl Lease {
    pub fn acquire(path: &Path) -> Result<Self, String> {
        Self::acquire_bounded(path, FilePolicy::Legacy, || Ok(()))
    }
    fn acquire_bounded(
        path: &Path,
        policy: FilePolicy,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        let path = canonical_destination(path)?;
        let dir = path
            .parent()
            .ok_or("Missing destination directory")?
            .join(".peerbrush-versions");
        if dir.exists()
            && !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
        {
            return Err("Save safety directory is not a regular directory".into());
        }
        fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create previous-save storage: {e}"))?;
        let key = destination_key(&path);
        let lock = dir.join(format!("{:016x}{:016x}.lock", key[0], key[1]));
        if lock.exists() && !regular(&lock) {
            return Err("Save lock is not a regular file".into());
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock)
            .map_err(|e| e.to_string())?;
        file.try_lock().map_err(|_| {
            "Another PeerBrush save is using this directory. Retry when it finishes.".to_string()
        })?;
        let file = LockedFile(file);
        let expected = snapshot_bounded(&path, policy, check)?;
        Ok(Self {
            _file: file,
            path,
            expected,
            policy,
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        self.validate_checked(|| Ok(()))
    }
    fn validate_checked(&self, check: impl Fn() -> Result<(), String>) -> Result<(), String> {
        if snapshot_bounded(&self.path, self.policy, check)? != self.expected {
            return Err(
                "Destination changed while saving. Current work preserved; use a new Save As path."
                    .into(),
            );
        }
        Ok(())
    }
    /// Keep up to three preceding byte-exact files, never prune before publishing.
    pub fn preserve(&self) -> Result<Option<PathBuf>, String> {
        self.preserve_checked(|| Ok(()))
    }
    fn preserve_checked(
        &self,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Option<PathBuf>, String> {
        let Some(expected) = self.expected else {
            return Ok(None);
        };
        self.validate_checked(&check)?;
        let dir = self.path.parent().unwrap().join(".peerbrush-versions");
        let quota = dir.join("quota.lock");
        if quota.exists() && !regular(&quota) {
            return Err("Previous-save quota lock is not a regular file".into());
        }
        let quota = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(quota)
            .map_err(|e| e.to_string())?;
        quota.lock().map_err(|e| e.to_string())?;
        let _quota = LockedFile(quota);
        let total = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| regular(&e.path()))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .try_fold(0u64, |total, bytes| {
                total
                    .checked_add(bytes)
                    .ok_or("Previous-save byte accounting overflow")
            })?;
        if total.saturating_add(expected.bytes) > self.policy.backup_bytes() {
            return Err(match self.policy {
                FilePolicy::Legacy => "Previous-save storage is full (1 GiB per directory). Preserve or remove old versions before overwriting; Save As to a new directory is available.",
                FilePolicy::Source36 => "Source36 previous-save storage is full (64 GiB per directory). Current file retained; preserve old versions or choose a new Save As directory.",
            }.into());
        }
        let key = destination_key(&self.path);
        let name = format!(
            "{:016x}{:016x}-{}.{}",
            key[0],
            key[1],
            crate::engine::id(),
            self.path.extension().unwrap_or_default().to_string_lossy()
        );
        let backup = dir.join(name);
        let mut source = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        let mut output = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&backup)
            .map_err(|e| e.to_string())?;
        let backup_identity = identity(&output)?;
        let result = (|| {
            preserve_permissions(&self.path, &backup)?;
            let mut buffer = [0u8; 64 * 1024];
            let mut copied = 0u64;
            loop {
                check()?;
                let n = source.read(&mut buffer).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                copied = copied
                    .checked_add(n as u64)
                    .ok_or("Previous-save length overflow")?;
                if copied > expected.bytes {
                    return Err(
                        "Previous file grew while preserving it; destination retained".into(),
                    );
                }
                output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
            }
            if copied != expected.bytes {
                return Err(
                    "Previous file changed while preserving it; destination retained".into(),
                );
            }
            output.flush().map_err(|e| e.to_string())?;
            output.sync_all().map_err(|e| e.to_string())?;
            drop(output);
            let saved = version_bounded(&backup, self.policy, &check)?;
            if saved.identity != backup_identity
                || saved.bytes != expected.bytes
                || saved.fingerprint != expected.fingerprint
            {
                return Err("Previous-save verification failed; original file retained".into());
            }
            self.validate_checked(&check)?;
            crate::server::sync_parent(&backup)?;
            Ok(Some(backup.clone()))
        })();
        if result.is_err() {
            // A replacement at this freshly generated name is never ours to delete.
            if open_for_identity(&backup)
                .ok()
                .and_then(|file| identity(&file).ok())
                == Some(backup_identity)
            {
                let _ = fs::remove_file(&backup);
            }
        }
        result
    }
    pub fn prune(&self, newest: &Path) {
        let Some(prefix) = newest
            .file_name()
            .and_then(|s| s.to_str())
            .and_then(|s| s.get(..33))
        else {
            return;
        };
        let Ok(entries) = fs::read_dir(newest.parent().unwrap()) else {
            return;
        };
        let mut files: Vec<_> = entries
            .flatten()
            .filter(|e| {
                regular(&e.path())
                    && e.file_name().to_str().is_some_and(|s| {
                        s.starts_with(prefix)
                            && s.len() > 69
                            && s.get(33..69)
                                .is_some_and(|s| uuid::Uuid::parse_str(s).is_ok())
                    })
            })
            .collect();
        files.sort_by_key(|e| {
            (
                e.path() != newest,
                std::cmp::Reverse(e.metadata().and_then(|m| m.modified()).ok()),
            )
        });
        for old in files.into_iter().skip(3) {
            if old.path() != newest {
                let _ = fs::remove_file(old.path());
            }
        }
    }
}

/// Explicit Source36 save guard. Legacy callers retain their existing file and
/// backup limits. This lease grants no scene/codec/publication authority; the
/// native save driver must retain its SourceOperation and completed scene proof.
pub(crate) struct NativeLease(Lease);
impl NativeLease {
    pub(crate) fn acquire(
        path: &Path,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        Lease::acquire_bounded(path, FilePolicy::Source36, check).map(Self)
    }
    pub(crate) fn path(&self) -> &Path {
        &self.0.path
    }
    pub(crate) fn expected(&self) -> Option<Version> {
        self.0.expected
    }
    pub(crate) fn same_destination(
        &self,
        source: &Path,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<bool, String> {
        if canonical_destination(source)? == self.0.path {
            return Ok(true);
        }
        Ok(
            match (
                snapshot_bounded(source, FilePolicy::Source36, check)?,
                self.0.expected,
            ) {
                (Some(a), Some(b)) => a.identity == b.identity,
                _ => false,
            },
        )
    }
    pub(crate) fn validate(&self, check: impl Fn() -> Result<(), String>) -> Result<(), String> {
        self.0.validate_checked(check)
    }
    pub(crate) fn preserve(
        &self,
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Option<PathBuf>, String> {
        self.0.preserve_checked(check)
    }
    pub(crate) fn prune(&self, newest: &Path) {
        self.0.prune(newest)
    }
    pub(crate) fn verified_version(
        &self,
        path: &Path,
        bytes: u64,
        sha256: [u8; 32],
        check: impl Fn() -> Result<(), String>,
    ) -> Result<Version, String> {
        let actual = version_bounded(path, FilePolicy::Source36, check)?;
        if actual.bytes != bytes || actual.fingerprint != sha256 {
            return Err("Source36 file changed before final verification; current work and previous version retained".into());
        }
        Ok(actual)
    }
}

pub fn source_conflict(path: Option<&Path>, expected: Option<Version>) -> bool {
    match (path, expected) {
        (Some(path), Some(expected)) => {
            let policy = if expected.bytes > FilePolicy::Legacy.file_bytes() {
                FilePolicy::Source36
            } else {
                FilePolicy::Legacy
            };
            snapshot_bounded(path, policy, || Ok(())).map_or(true, |v| v != Some(expected))
        }
        _ => false,
    }
}

#[cfg(test)]
mod native_lease_tests {
    use super::*;
    use std::cell::Cell;
    fn directory() -> PathBuf {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!("pb-native-save-guard-{}", crate::engine::id()));
        fs::create_dir(&path).unwrap();
        path
    }
    fn cleanup(path: &Path) {
        assert_eq!(fs::canonicalize(path).unwrap(), path);
        assert_eq!(
            path.parent().unwrap(),
            fs::canonicalize(std::env::temp_dir()).unwrap()
        );
        assert!(path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pb-native-save-guard-"));
        fs::remove_dir_all(path).unwrap();
    }
    #[test]
    fn explicit_native_lease_preserves_exact_small_file_and_verifies_final_bytes() {
        let dir = directory();
        let path = dir.join("native.psb");
        let bytes: Vec<_> = (0..200123).map(|i| (i * 29 + 17) as u8).collect();
        fs::write(&path, &bytes).unwrap();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let calls = Cell::new(0);
        let backup = lease
            .preserve(|| {
                calls.set(calls.get() + 1);
                Ok(())
            })
            .unwrap()
            .unwrap();
        assert!(calls.get() > 8);
        assert_eq!(fs::read(&backup).unwrap(), bytes);
        assert!(lease.same_destination(&path, || Ok(())).unwrap());
        let sha = finish_fingerprint({
            let mut h = ring::digest::Context::new(&ring::digest::SHA256);
            h.update(&bytes);
            h
        });
        assert_eq!(
            lease
                .verified_version(&path, bytes.len() as u64, sha, || Ok(()))
                .unwrap(),
            lease.expected().unwrap()
        );
        assert!(lease
            .verified_version(&path, bytes.len() as u64, [0; 32], || Ok(()))
            .is_err());
        lease.validate(|| Ok(())).unwrap();
        fs::write(&path, b"changed source").unwrap();
        assert!(lease.validate(|| Ok(())).is_err());
        assert_eq!(fs::read(&backup).unwrap(), bytes);
        drop(lease);
        cleanup(&dir);
    }
    #[test]
    fn cancelled_stream_and_missing_destination_still_run_admission() {
        let dir = directory();
        let path = dir.join("native.psb");
        fs::write(&path, vec![71; 200123]).unwrap();
        let calls = Cell::new(0);
        assert!(version_bounded(&path, FilePolicy::Source36, || {
            calls.set(calls.get() + 1);
            if calls.get() == 3 {
                Err("cancelled exact stream".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(calls.get(), 3);
        assert_eq!(fs::metadata(&path).unwrap().len(), 200123);
        assert!(
            NativeLease::acquire(&dir.join("absent.psb"), || Err("closed session".into())).is_err()
        );
        assert!(!dir.join("absent.psb").exists());
        cleanup(&dir);
    }
    #[test]
    fn late_foreign_backup_replacement_is_refused_and_retained() {
        let dir = directory();
        let path = dir.join("native.psb");
        let bytes = vec![31; 200123];
        fs::write(&path, &bytes).unwrap();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let foreign = std::cell::RefCell::new(None);
        let held = dir.join("original-owned-partial.psb");
        let result = lease.preserve(|| {
            if foreign.borrow().is_none() {
                let file = fs::read_dir(dir.join(".peerbrush-versions"))
                    .unwrap()
                    .flatten()
                    .map(|e| e.path())
                    .find(|p| {
                        p.extension().is_some_and(|e| e == "psb")
                            && fs::metadata(p).is_ok_and(|m| m.len() > 0)
                    });
                if let Some(file) = file {
                    fs::rename(&file, &held).unwrap();
                    fs::write(&file, b"foreign bytes stay").unwrap();
                    *foreign.borrow_mut() = Some(file);
                }
            }
            Ok(())
        });
        assert!(result.is_err());
        let foreign = foreign.into_inner().unwrap();
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign bytes stay");
        assert_eq!(fs::read(&path).unwrap(), bytes);
        assert!(held.exists());
        drop(lease);
        cleanup(&dir);
    }
}
#[derive(Clone)]
pub struct PreviousVersion {
    pub path: PathBuf,
    pub timestamp_ms: u64,
    pub bytes: u64,
}
pub fn previous_versions(path: &Path) -> Result<Vec<PreviousVersion>, String> {
    let path = canonical_destination(path)?;
    let key = destination_key(&path);
    let prefix = format!("{:016x}{:016x}-", key[0], key[1]);
    let dir = path.parent().unwrap().join(".peerbrush-versions");
    if !dir.exists() {
        return Ok(Vec::new());
    }
    if !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink()) {
        return Err("Previous-save storage is not a regular directory".into());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file = entry.path();
        if !regular(&file)
            || !file
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("psd") || e.eq_ignore_ascii_case("psb"))
        {
            continue;
        }
        if !entry.file_name().to_str().is_some_and(|s| {
            s.starts_with(&prefix)
                && s.get(33..69)
                    .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
        }) {
            continue;
        }
        let m = entry.metadata().map_err(|e| e.to_string())?;
        entries.push(PreviousVersion {
            path: file,
            bytes: m.len(),
            timestamp_ms: m
                .modified()
                .map_err(|e| e.to_string())?
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .min(u64::MAX as u128) as u64,
        });
    }
    entries.sort_by_key(|e| std::cmp::Reverse(e.timestamp_ms));
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    #[test]
    fn identity_handle_denies_data_access_and_distinguishes_replaced_names() {
        let parent = std::env::temp_dir().canonicalize().unwrap();
        let directory = parent.join(format!("pb-identity-access-{}", crate::engine::id()));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("owned.bin");
        let moved = directory.join("moved.bin");
        let original = b"owned native bytes\x27\x11\x4e\x23\x75\x37";
        fs::write(&path, original).unwrap();
        let held = fs::File::open(&path).unwrap();
        let original_identity = identity(&held).unwrap();
        let mut named = open_for_identity(&path).unwrap();
        assert_eq!(identity(&named).unwrap(), original_identity);
        assert_eq!(named.metadata().unwrap().len(), original.len() as u64);
        assert_eq!(
            named.read(&mut [0; 1]).unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            named.write(b"forbidden").unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert_eq!(fs::read(&path).unwrap(), original);
        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"foreign bytes retained").unwrap();
        let foreign = open_for_identity(&path).unwrap();
        assert_ne!(identity(&foreign).unwrap(), original_identity);
        assert_eq!(identity(&named).unwrap(), original_identity);
        assert_eq!(identity(&held).unwrap(), original_identity);
        assert_eq!(fs::read(&path).unwrap(), b"foreign bytes retained");
        assert_eq!(fs::read(&moved).unwrap(), original);
        drop(foreign);
        drop(named);
        drop(held);
        fs::remove_file(path).unwrap();
        fs::remove_file(moved).unwrap();
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn save_leases_release_before_duplicated_descriptors_close() {
        let dir = std::env::temp_dir().join(format!("pb-duplicated-lease-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("Artwork.psd");
        fs::write(&path, b"original source").unwrap();
        let lease = Lease::acquire(&path).unwrap();
        let descriptor = lease._file.0.try_clone().unwrap();
        assert!(Lease::acquire(&path).is_err());
        drop(lease);
        let next = Lease::acquire(&path).unwrap();
        assert!(Lease::acquire(&path).is_err());
        drop(descriptor);
        assert!(Lease::acquire(&path).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"original source");
        drop(next);
        assert!(Lease::acquire(&path).is_ok());
        let quota_path = dir.join(".peerbrush-versions/quota.lock");
        let quota = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&quota_path)
            .unwrap();
        quota.lock().unwrap();
        let descriptor = quota.try_clone().unwrap();
        let guard = LockedFile(quota);
        drop(guard);
        let next = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(quota_path)
            .unwrap();
        next.try_lock().unwrap();
        let next = LockedFile(next);
        drop(descriptor);
        drop(next);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn content_fingerprints_match_standard_sha256_vectors_across_chunks() {
        let vectors = [
            (
                Vec::new(),
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                b"abc".to_vec(),
                "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            ),
            (
                vec![b'a'; 1_000_000],
                "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0",
            ),
        ];
        for (bytes, expected) in vectors {
            for chunk in [1, 63, 64, 65, 4093, 65535, 65536, 65537] {
                if bytes.len() > 65536 && chunk < 4093 {
                    continue;
                }
                let mut context = ring::digest::Context::new(&ring::digest::SHA256);
                for part in bytes.chunks(chunk) {
                    context.update(part);
                }
                let actual = finish_fingerprint(context)
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                assert_eq!(actual, expected, "{} bytes / {chunk}", bytes.len());
            }
        }
    }

    #[test]
    fn content_version_readback_matches_buffered_and_streamed_fingerprints() {
        let path = std::env::temp_dir().join(format!("pb-sha256-readback-{}", crate::engine::id()));
        let bytes = vec![b'a'; 1_000_000];
        fs::write(&path, &bytes).unwrap();
        let version = version(&path).unwrap();
        let hex = version
            .fingerprint
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(
            hex,
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
        assert_eq!(published_version(&path, &bytes).unwrap(), version);
        let mut context = ring::digest::Context::new(&ring::digest::SHA256);
        for chunk in bytes.chunks(4093) {
            context.update(chunk);
        }
        let expected = finish_fingerprint(context);
        assert_eq!(
            verified_version(&path, bytes.len() as u64, expected, || Ok(())).unwrap(),
            version
        );
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        let mut changed = bytes.clone();
        changed[65536] ^= 1;
        fs::write(&path, &changed).unwrap();
        fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(verified_version(&path, bytes.len() as u64, expected, || Ok(())).is_err());
        assert!(source_conflict(Some(&path), Some(version)));
        assert_eq!(fs::read(&path).unwrap(), changed);
        fs::remove_file(path).unwrap();
    }

    #[test]
    #[ignore = "Opt-in isolated file-content hash component; run alone from a clean build"]
    fn content_fingerprint_component_diagnostic() {
        use std::{hint::black_box, time::Instant};
        let build: serde_json::Value = serde_json::from_str(crate::build_info::json()).unwrap();
        assert_eq!(build["tree"], "clean");
        assert_eq!(build["commit"].as_str().unwrap().len(), 40);
        let bytes: Vec<u8> = (0..32 * 1024 * 1024usize)
            .map(|i| (i.wrapping_mul(73).wrapping_add(19) & 255) as u8)
            .collect();
        let before = ring::digest::digest(&ring::digest::SHA256, &bytes);
        let expected: [u8; 32] = before.as_ref().try_into().unwrap();
        let mut legacy = [0xcbf29ce484222325, 0x84222325cbf29ce4];
        hash(&bytes, &mut legacy);
        let mut records = Vec::new();
        for trial in 1..=4 {
            let algorithms = if trial % 2 == 1 {
                ["legacy-dual-fnv", "sha256"]
            } else {
                ["sha256", "legacy-dual-fnv"]
            };
            for (position, algorithm) in algorithms.into_iter().enumerate() {
                let started = Instant::now();
                let valid = if algorithm == "legacy-dual-fnv" {
                    let mut actual = [0xcbf29ce484222325, 0x84222325cbf29ce4];
                    for chunk in bytes.chunks(64 * 1024) {
                        hash(black_box(chunk), &mut actual);
                    }
                    let actual = black_box(actual);
                    let elapsed = started.elapsed();
                    (actual == legacy, elapsed)
                } else {
                    let mut context = ring::digest::Context::new(&ring::digest::SHA256);
                    for chunk in bytes.chunks(64 * 1024) {
                        context.update(black_box(chunk));
                    }
                    let actual = black_box(finish_fingerprint(context));
                    let elapsed = started.elapsed();
                    (actual == expected, elapsed)
                };
                assert!(valid.0);
                records.push(serde_json::json!({"trial":trial,"algorithm":algorithm,"position":position+1,
                    "bytes":bytes.len(),"chunk_bytes":65536,"hash_ms":valid.1.as_secs_f64()*1000.,"exact_fingerprint":true}));
            }
        }
        assert_eq!(
            ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref(),
            expected.as_slice()
        );
        println!(
            "\n{}",
            serde_json::json!({"format":"PeerBrushContentFingerprintComponentV1","build":build,
            "profile":if cfg!(debug_assertions) {"test-debug-assertions"} else {"test-release"},
            "normal_application":false,"native_window":false,"records":records,
            "note":"Balanced in-memory 32 MiB content-hash component only. File I/O, metadata, ownership, full Open/Save, lock time, memory, cache and scheduling are not qualified by these timings."})
        );
    }
}
