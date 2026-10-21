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
    fingerprint: [u8; 32],
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
    check()?;
    let mut file = fs::File::open(path).map_err(|e| e.to_string())?;
    let identity = identity(&file)?;
    let before = stamp(&file)?;
    if before.0 > 256 * 1024 * 1024 {
        return Err("File exceeds the 256 MiB safety-check limit; original retained".into());
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
        if total > 256 * 1024 * 1024 {
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
    match fs::metadata(path) {
        Ok(_) => version(path).map(Some),
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
        let file = LockedFile(file);
        let expected = snapshot(&path)?;
        Ok(Self {
            _file: file,
            path,
            expected,
        })
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
        let _quota = LockedFile(quota);
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
