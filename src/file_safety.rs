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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    pub identity: (u64, u64),
    pub bytes: u64,
    modified: u128,
    fingerprint: [u64; 2],
}

#[cfg(unix)]
pub(crate) fn identity(file: &fs::File) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    Ok((m.dev(), m.ino()))
}
#[cfg(windows)]
pub(crate) fn identity(file: &fs::File) -> Result<(u64, u64), String> {
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
        info.volume as u64,
        ((info.index_high as u64) << 32) | info.index_low as u64,
    ))
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
fn hash(bytes: &[u8], state: &mut [u64; 2]) {
    for &b in bytes {
        state[0] = (state[0] ^ b as u64).wrapping_mul(0x100000001b3);
        state[1] = (state[1] ^ b as u64).wrapping_mul(0x9e3779b185ebca87);
    }
}
pub fn version(path: &Path) -> Result<Version, String> {
    version_with_check(path, || Ok(()))
}
pub(crate) fn version_with_check(
    path: &Path,
    check: impl Fn() -> Result<(), String>,
) -> Result<Version, String> {
    check()?;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let identity = identity(&file)?;
    let before = stamp(&file)?;
    if before.0 > 256 * 1024 * 1024 {
        return Err("File exceeds the 256 MiB safety-check limit; original retained".into());
    }
    let mut fingerprint = [0xcbf29ce484222325, 0x84222325cbf29ce4];
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        check()?;
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > 256 * 1024 * 1024 {
            return Err("File grew beyond its safety-check limit; original retained".into());
        }
        hash(&buffer[..n], &mut fingerprint);
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
        fingerprint,
    })
}
pub fn snapshot(path: &Path) -> Result<Option<Version>, String> {
    match fs::metadata(path) {
        Ok(_) => version(path).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}
pub(crate) fn published_version(path: &Path, bytes: &[u8]) -> Result<Version, String> {
    let mut expected = [0xcbf29ce484222325, 0x84222325cbf29ce4];
    hash(bytes, &mut expected);
    let actual = version(path)?;
    if actual.bytes != bytes.len() as u64 || actual.fingerprint != expected {
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
pub struct Lease {
    _file: fs::File,
    pub path: PathBuf,
    pub expected: Option<Version>,
}
impl Drop for Lease {
    fn drop(&mut self) {
        // A concurrently spawned child can temporarily inherit this handle.
        // Closing only our descriptor would leave the completed save locked
        // until that child closes its copy. Release the lease explicitly.
        let _ = self._file.unlock();
    }
}
impl Lease {
    pub fn acquire(path: &Path) -> Result<Self, String> {
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
        let mut lease = Self {
            _file: file,
            path,
            expected: None,
        };
        lease.expected = snapshot(&lease.path)?;
        Ok(lease)
    }
    pub fn validate(&self) -> Result<(), String> {
        if snapshot(&self.path)? != self.expected {
            return Err(
                "Destination changed while saving. Current work preserved; use a new Save As path."
                    .into(),
            );
        }
        Ok(())
    }
    /// Keep up to three preceding byte-exact files, never prune before publishing.
    pub fn preserve(&self) -> Result<Option<PathBuf>, String> {
        let Some(expected) = self.expected else {
            return Ok(None);
        };
        self.validate()?;
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
        let total = fs::read_dir(&dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| regular(&e.path()))
            .filter_map(|e| e.metadata().ok())
            .map(|m| m.len())
            .sum::<u64>();
        if total.saturating_add(expected.bytes) > 1024 * 1024 * 1024 {
            return Err("Previous-save storage is full (1 GiB per directory). Preserve or remove old versions before overwriting; Save As to a new directory is available.".into());
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
        let result = (|| {
            preserve_permissions(&self.path, &backup)?;
            let copied = std::io::copy(&mut (&mut source).take(expected.bytes + 1), &mut output)
                .map_err(|e| e.to_string())?;
            if copied != expected.bytes {
                return Err(
                    "Previous file changed while preserving it; destination retained".into(),
                );
            }
            output.flush().map_err(|e| e.to_string())?;
            output.sync_all().map_err(|e| e.to_string())?;
            drop(output);
            let saved = version(&backup)?;
            if saved.bytes != expected.bytes || saved.fingerprint != expected.fingerprint {
                return Err("Previous-save verification failed; original file retained".into());
            }
            self.validate()?;
            crate::server::sync_parent(&backup)?;
            Ok(Some(backup.clone()))
        })();
        if result.is_err() {
            let _ = fs::remove_file(&backup);
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

pub fn source_conflict(path: Option<&Path>, expected: Option<Version>) -> bool {
    match (path, expected) {
        (Some(path), Some(expected)) => snapshot(path).map_or(true, |v| v != Some(expected)),
        _ => false,
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

    #[test]
    fn completed_save_releases_lease_while_an_inherited_handle_remains_open() {
        let dir = std::env::temp_dir().join(format!("pb-save-lease-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("drawing.psd");
        let lease = Lease::acquire(&path).unwrap();
        // Duplicate the open file description just as a concurrent process
        // launch can before exec closes its inherited descriptors.
        let inherited = lease._file.try_clone().unwrap();
        assert!(Lease::acquire(&path).is_err());
        drop(lease);
        let next = Lease::acquire(&path).expect("the completed save must release its lock");
        assert!(Lease::acquire(&path).is_err());
        drop(inherited);
        assert!(Lease::acquire(&path).is_err());
        drop(next);
        assert!(Lease::acquire(&path).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
}
