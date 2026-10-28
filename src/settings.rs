//! Per-user preferences. Runtime discovery/recovery stays in its existing workspace.
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsString,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

const FILES: &[&str] = &[
    "brushes.json",
    "export-presets.json",
    "filters.json",
    "shortcuts.json",
    "tool-settings.json",
    "updates.json",
];

#[derive(Clone, Debug, Serialize)]
pub struct Locations {
    pub config: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
}
impl Locations {
    pub fn user() -> Result<Self, String> {
        Self::from_environment(std::env::consts::OS, |key| std::env::var_os(key))
    }
    fn from_environment(os: &str, env: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        let required = |key: &str| {
            env(key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .ok_or_else(|| format!("Cannot locate absolute {key} for per-user settings"))
        };
        Ok(match os {
            "windows" => Self {
                config: required("APPDATA")?.join("PeerBrush"),
                data: required("LOCALAPPDATA")?.join("PeerBrush/Data"),
                cache: required("LOCALAPPDATA")?.join("PeerBrush/Cache"),
            },
            "macos" => {
                let home = required("HOME")?;
                Self {
                    config: home.join("Library/Application Support/PeerBrush/Config"),
                    data: home.join("Library/Application Support/PeerBrush/Data"),
                    cache: home.join("Library/Caches/PeerBrush"),
                }
            }
            "linux" => {
                let home = required("HOME")?;
                let base = |key, fallback: &str| {
                    env(key)
                        .map(PathBuf::from)
                        .filter(|p| p.is_absolute())
                        .unwrap_or_else(|| home.join(fallback))
                };
                Self {
                    config: base("XDG_CONFIG_HOME", ".config").join("peerbrush"),
                    data: base("XDG_DATA_HOME", ".local/share").join("peerbrush"),
                    cache: base("XDG_CACHE_HOME", ".cache").join("peerbrush"),
                }
            }
            _ => return Err("Per-user settings are supported on Windows, macOS and Linux".into()),
        })
    }
    pub fn profile(&self) -> PathBuf {
        self.config.join("settings-v1")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    migrated_files: Vec<String>,
}

/// Kept alive for the application session, serializing profile edits and migration.
pub struct Profile {
    pub locations: Locations,
    _lock: std::sync::Arc<Lock>,
}
/// A default-profile lease can only come from the session holding settings.lock.
/// Explicit --state-dir sessions cannot request installed-app native staging or checkpoints.
#[cfg(any(windows, target_os = "macos", test))]
#[derive(Clone)]
pub struct UpdateProfile {
    pub(crate) locations: Locations,
    _lock: std::sync::Arc<Lock>,
}
#[cfg(any(windows, target_os = "macos", test))]
impl Profile {
    pub fn update_profile(&self) -> UpdateProfile {
        UpdateProfile {
            locations: self.locations.clone(),
            _lock: self._lock.clone(),
        }
    }
}
#[cfg(any(windows, target_os = "macos", test))]
impl UpdateProfile {
    #[cfg(target_os = "macos")]
    pub(crate) fn inherited_lock(&self) -> Result<fs::File, String> {
        self._lock
            .0
            .try_clone()
            .map_err(|_| "Settings lock cannot be inherited".into())
    }
    /// The coordinator calls this only after authenticating its inherited peer.
    /// The descriptor is the parent's actual lock, not a newly acquired lock.
    #[cfg(target_os = "macos")]
    pub(crate) fn from_inherited(locations: Locations, file: fs::File) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let opened = file
            .metadata()
            .map_err(|_| "Inherited settings lock unavailable")?;
        let path = fs::symlink_metadata(locations.config.join("settings.lock"))
            .map_err(|_| "Inherited settings lock path unavailable")?;
        if !opened.is_file()
            || !path.is_file()
            || alias(&path)
            || opened.nlink() != 1
            || path.nlink() != 1
            || opened.uid() != unsafe { libc::geteuid() }
            || (opened.dev(), opened.ino()) != (path.dev(), path.ino())
        {
            return Err("Inherited settings lock differs from the actual profile lock".into());
        }
        use std::os::fd::AsRawFd;
        // The pinned Darwin implementation uses flock's shared open description.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err("Inherited descriptor does not retain the parent's settings lock".into());
        }
        let profile = Self {
            locations,
            _lock: std::sync::Arc::new(Lock(file)),
        };
        profile.validate()?;
        Ok(profile)
    }
    pub(crate) fn validate(&self) -> Result<(), String> {
        Profile {
            locations: self.locations.clone(),
            _lock: self._lock.clone(),
        }
        .validate_manifest()?;
        for name in FILES {
            let path = self.locations.profile().join(name);
            if read_legacy(&path)?.is_some() {
                validate(name, &path)?;
            }
        }
        Ok(())
    }
}
#[cfg(any(windows, target_os = "macos", test))]
pub(crate) fn update_files() -> Vec<std::path::PathBuf> {
    let mut paths = vec![std::path::PathBuf::from("profile.json")];
    for name in FILES {
        paths.push((*name).into());
        paths.push(std::path::Path::new("migration-backup").join(name));
    }
    paths
}
struct Lock(fs::File);

fn alias(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Junctions and other reparse points are aliases even when they are not
        // reported as symbolic links. Standard OS location ancestors may redirect.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        // Darwin flock is shared by inherited copies of this open description.
        // Closing the last descriptor releases it; explicit unlock would also
        // release a surviving capture owner's protection.
        #[cfg(not(target_os = "macos"))]
        let _ = self.0.unlock();
    }
}
impl Lock {
    fn acquire(path: &Path) -> Result<Self, String> {
        if let Ok(metadata) = fs::symlink_metadata(path) {
            if !metadata.is_file() || alias(&metadata) {
                return Err("Settings lock must be a regular file".into());
            }
        }
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| e.to_string())?;
        file.try_lock().map_err(|_| {
            "Settings are in use. Close the PeerBrush session using this profile or source workspace and retry.".to_string()
        })?;
        Ok(Self(file))
    }
}

fn private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() && !alias(&m) => return Ok(()),
        Ok(_) => return Err("Settings directory must be a regular directory".into()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.to_string()),
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|e| e.to_string())
}

impl Profile {
    /// The independent Windows gate already excludes new admissions. Reacquire
    /// this process's own byte-range lock after the actual parent's kernel exit;
    /// do not infer inherited ownership or run migration in the coordinator.
    #[cfg(windows)]
    pub(crate) fn lock_for_windows_owner(
        locations: Locations,
        gate: &crate::updates::windows::gate::Gate,
    ) -> Result<Self, String> {
        gate.validate_owner(&locations.config)?;
        let lock = Lock::acquire(&locations.config.join("settings.lock"))?;
        let profile = Self {
            locations,
            _lock: std::sync::Arc::new(lock),
        };
        profile.update_profile().validate()?;
        Ok(profile)
    }
    /// First default launch copies validated legacy preferences, never a blank reset.
    pub fn open(locations: Locations, legacy: &Path) -> Result<Self, String> {
        let profile = Self::lock(locations)?;
        if fs::symlink_metadata(profile.locations.profile())
            .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
        {
            profile.copy_legacy(legacy, || Ok(()))?;
        }
        profile.validate_manifest()?;
        Ok(profile)
    }
    pub fn migrate(locations: Locations, source: &Path) -> Result<Self, String> {
        if !source.is_dir() {
            return Err("Choose an existing legacy settings directory".into());
        }
        let profile = Self::lock(locations)?;
        profile.copy_legacy(source, || Ok(()))?;
        Ok(profile)
    }
    fn lock(locations: Locations) -> Result<Self, String> {
        private_directory(&locations.config)?;
        // Retain admission through byte-range lock acquisition. A surviving
        // Windows owner denies new admission independently of the old process.
        #[cfg(windows)]
        let _admission = crate::updates::windows::gate::Gate::admit(&locations.config)?;
        let lock = Lock::acquire(&locations.config.join("settings.lock"))?;
        #[cfg(target_os = "macos")]
        crate::sparkle::macos::checkpoint::startup_guard(&locations)?;
        Ok(Self {
            locations,
            _lock: std::sync::Arc::new(lock),
        })
    }
    fn validate_manifest(&self) -> Result<(), String> {
        let root = self.locations.profile();
        if !fs::symlink_metadata(&root).is_ok_and(|m| m.is_dir() && !alias(&m)) {
            return Err("Settings profile must be a regular directory; original retained".into());
        }
        let bytes = crate::tool_settings::read_owned(&root.join("profile.json"))?
            .ok_or("Settings profile is incomplete; preserve it before retrying migration")?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .map_err(|e| format!("Unreadable settings profile: {e}"))?;
        if manifest.version != 1
            || manifest.migrated_files.len() > FILES.len()
            || manifest.migrated_files.windows(2).any(|w| w[0] >= w[1])
            || manifest
                .migrated_files
                .iter()
                .any(|s| !FILES.contains(&s.as_str()))
        {
            return Err("Unsupported settings profile; original retained".into());
        }
        Ok(())
    }
    fn copy_legacy(
        &self,
        source: &Path,
        before_publish: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let destination = self.locations.profile();
        match fs::symlink_metadata(&destination) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => {
                return Err("Settings profile already exists; migration never overwrites it".into())
            }
        }
        let source_lock = match fs::symlink_metadata(source) {
            Ok(m) if m.is_dir() && !alias(&m) => {
                Some(Lock::acquire(&source.join("instance.lock"))?)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            _ => return Err("Legacy settings need a regular source directory".into()),
        };
        let stage = self
            .locations
            .config
            .join(format!("migration-{}", crate::engine::id()));
        private_directory(&stage)?;
        private_directory(&stage.join("migration-backup"))?;
        let mut originals = Vec::new();
        for name in FILES {
            let path = source.join(name);
            let original = read_legacy(&path)?;
            originals.push((path.clone(), original.clone()));
            let Some(bytes) = original else { continue };
            let target = stage.join(name);
            write_new(&target, &bytes)?;
            crate::file_safety::preserve_permissions(&path, &target)?;
            validate(name, &target)?;
            let backup = stage.join("migration-backup").join(name);
            write_new(&backup, &bytes)?;
            crate::file_safety::preserve_permissions(&path, &backup)?;
        }
        let manifest = Manifest {
            version: 1,
            migrated_files: originals
                .iter()
                .filter(|(_, b)| b.is_some())
                .map(|(p, _)| p.file_name().unwrap().to_str().unwrap().to_owned())
                .collect(),
        };
        write_new(
            &stage.join("profile.json"),
            &serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?,
        )?;
        before_publish()?;
        for (path, bytes) in &originals {
            if read_legacy(path)? != *bytes {
                return Err("Legacy settings changed during migration; original retained, retry after closing its writer".into());
            }
        }
        crate::server::sync_parent(&stage.join("migration-backup/entry"))?;
        crate::server::sync_parent(&stage.join("profile.json"))?;
        publish_directory(&stage, &destination)?;
        crate::server::sync_parent(&destination)?;
        drop(source_lock);
        Ok(())
    }
}

fn read_legacy(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let m = match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        other => other.map_err(|e| e.to_string())?,
    };
    if !m.is_file() || alias(&m) || m.len() > 4 * 1024 * 1024 {
        return Err("Legacy settings must be regular files of at most 4 MiB".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err("Legacy settings grew beyond 4 MiB".into());
    }
    Ok(Some(bytes))
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}
fn validate(name: &str, path: &Path) -> Result<(), String> {
    let error = match name {
        "brushes.json" => crate::brush_library::Library::load(path.into()).error,
        "filters.json" => crate::filter_library::Library::load(path.into()).error,
        "export-presets.json" => {
            crate::production_export::library::Library::load(path.into()).error
        }
        "tool-settings.json" => crate::tool_settings::Store::load(path.into()).error,
        "shortcuts.json" => crate::ui::validate_shortcut_preferences(path).err(),
        "updates.json" => crate::updates::validate_preferences(path).err(),
        _ => return Err("Unknown settings file".into()),
    };
    error.map_or(Ok(()), |e| Err(format!("Cannot migrate {name}: {e}")))
}

#[cfg(windows)]
fn publish_directory(from: &Path, to: &Path) -> Result<(), String> {
    // Windows directory rename fails if any destination already exists.
    fs::rename(from, to).map_err(|e| e.to_string())
}
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn publish_directory(from: &Path, to: &Path) -> Result<(), String> {
    use std::{
        ffi::{c_char, CString},
        os::unix::ffi::OsStrExt,
    };
    let from = CString::new(from.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    let to = CString::new(to.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    let result = {
        extern "C" {
            fn renameat2(
                oldfd: i32,
                old: *const c_char,
                newfd: i32,
                new: *const c_char,
                flags: u32,
            ) -> i32;
        }
        unsafe { renameat2(-100, from.as_ptr(), -100, to.as_ptr(), 1) }
    };
    #[cfg(target_os = "macos")]
    let result = {
        extern "C" {
            fn renamex_np(old: *const c_char, new: *const c_char, flags: u32) -> i32;
        }
        unsafe { renamex_np(from.as_ptr(), to.as_ptr(), 4) }
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
fn publish_directory(_: &Path, _: &Path) -> Result<(), String> {
    Err("Safe profile publication is unsupported on this platform".into())
}

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;
