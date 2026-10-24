//! Durable preservation before any native startup; never authorizes replacement.
use super::installed;
use crate::settings::{Locations, UpdateProfile};
use crate::sparkle::check::ApprovedCheck;
use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    sync::{atomic::AtomicBool, Arc},
};

const MAX_FILES: usize = 4096;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 1024;
const PATH_BUDGET: &str = "Update checkpoint exceeds its depth or relative-path budget";
const MAX_APP_BYTES: u64 = 512 * 1024 * 1024;
const MAX_PROFILE_BYTES: u64 = 56 * 1024 * 1024;
const MAX_CHECKPOINTS: usize = 3;
const HOLD: &str = "An app update needs recovery. Your settings were kept unchanged. Keep the previous app and update recovery files until the update outcome is verified.";

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    device: u64,
    inode: u64,
    mode: u32,
    links: u64,
    uid: u32,
    gid: u32,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl Stamp {
    fn read(path: &Path) -> Result<Option<Self>, String> {
        let m = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(HOLD.into()),
        };
        Ok(Some(Self {
            device: m.dev(),
            inode: m.ino(),
            mode: m.mode(),
            links: m.nlink(),
            uid: m.uid(),
            gid: m.gid(),
            bytes: m.len(),
            modified: (m.mtime(), m.mtime_nsec()),
            changed: (m.ctime(), m.ctime_nsec()),
        }))
    }
    fn kind(&self) -> u32 {
        self.mode & u32::from(libc::S_IFMT)
    }
    fn safe(&self) -> bool {
        (self.uid == 0 || self.uid == unsafe { libc::geteuid() }) && self.mode & 0o7022 == 0
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn digest(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&SHA256, bytes).as_ref())
}
fn private(path: &Path, create: bool) -> Result<bool, String> {
    for parent in path.ancestors().skip(1) {
        if let Some(value) = Stamp::read(parent)? {
            if value.kind() != u32::from(libc::S_IFDIR) || !value.safe() {
                return Err(HOLD.into());
            }
        }
    }
    if Stamp::read(path)?.is_none() {
        if !create {
            return Ok(false);
        }
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| HOLD)?;
        crate::server::sync_parent(path)?;
    }
    let value = Stamp::read(path)?.ok_or(HOLD)?;
    if value.kind() != u32::from(libc::S_IFDIR)
        || value.uid != unsafe { libc::geteuid() }
        || value.mode & 0o7777 != 0o700
    {
        return Err(HOLD.into());
    }
    for parent in path.ancestors().skip(1) {
        let value = Stamp::read(parent)?.ok_or(HOLD)?;
        if value.kind() != u32::from(libc::S_IFDIR) || !value.safe() {
            return Err(HOLD.into());
        }
    }
    Ok(true)
}
fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut out = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|_| HOLD)?;
    out.write_all(bytes).map_err(|_| HOLD)?;
    out.sync_all().map_err(|_| HOLD)?;
    Ok(())
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_new_file(path, bytes)?;
    crate::server::sync_parent(path)
}
fn read_private(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let value = Stamp::read(path)?.ok_or(HOLD)?;
    if value.kind() != u32::from(libc::S_IFREG)
        || value.links != 1
        || value.uid != unsafe { libc::geteuid() }
        || value.mode & 0o7777 != 0o600
        || value.bytes > limit
    {
        return Err(HOLD.into());
    }
    let bytes = installed::bounded(path, limit)?;
    if Stamp::read(path)? != Some(value) {
        return Err(HOLD.into());
    }
    Ok(bytes)
}

fn read_marker(path: &Path) -> Result<Vec<u8>, String> {
    read_private(path, 4096)
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
struct Intent {
    schema: u32,
    checkpoint: String,
    manifest_sha256: String,
    owner_pid: u32,
}
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
struct Unstarted {
    schema: u32,
    intent_sha256: String,
    owner_pid: u32,
}

fn checkpoints(locations: &Locations) -> PathBuf {
    locations.data.join("update-checkpoints")
}
fn entries(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(root).map_err(|_| HOLD)? {
        let path = entry.map_err(|_| HOLD)?.path();
        let name = path.file_name().and_then(|n| n.to_str()).ok_or(HOLD)?;
        if uuid::Uuid::parse_str(name).map_err(|_| HOLD)?.to_string() != name
            || !private(&path, false)?
            || paths.len() >= MAX_CHECKPOINTS
        {
            return Err(HOLD.into());
        }
        paths.push(path);
    }
    Ok(paths)
}

/// Called with settings.lock held, before profile migration or settings writes.
/// This never repairs, removes, relaunches or treats an SDK probe as absence.
pub(crate) fn startup_guard(locations: &Locations) -> Result<(), String> {
    let root = checkpoints(locations);
    // A normal profile with no journal must retain its existing startup behavior,
    // including platform-managed aliases in temporary fixture ancestors.
    if Stamp::read(&root)?.is_none() {
        return Ok(());
    }
    if !private(&root, false)? {
        return Ok(());
    }
    for path in entries(&root)? {
        inspect_unstarted_attempt(&path)?;
    }
    Ok(())
}

fn inspect_unstarted_attempt(path: &Path) -> Result<(), String> {
    let armed = path.join("native-start-intent.json");
    if Stamp::read(&armed)?.is_none() {
        return Ok(());
    }
    let bytes = read_marker(&armed)?;
    let intent: Intent = serde_json::from_slice(&bytes).map_err(|_| HOLD)?;
    let manifest = read_private(&path.join("checkpoint.json"), 1024 * 1024)?;
    let outcome: Unstarted =
        serde_json::from_slice(&read_marker(&path.join("native-not-started.json"))?)
            .map_err(|_| HOLD)?;
    if intent.schema != 1
        || intent.owner_pid == 0
        || intent.checkpoint != path.file_name().unwrap().to_str().unwrap()
        || intent.manifest_sha256 != digest(&manifest)
        || outcome.schema != 1
        || outcome.owner_pid != intent.owner_pid
        || outcome.intent_sha256 != digest(&bytes)
    {
        return Err(HOLD.into());
    }
    let stored: Manifest = serde_json::from_slice(&manifest).map_err(|_| HOLD)?;
    validate_manifest(&stored)?;
    check_copy(
        &path.join("PeerBrush.app"),
        &stored.app,
        &AtomicBool::new(false),
    )?;
    check_copy(
        &path.join("profile"),
        &stored.profile,
        &AtomicBool::new(false),
    )?;
    Ok(())
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq, Eq)]
#[serde(tag = "kind", deny_unknown_fields)]
enum Entry {
    Directory {
        mode: u32,
    },
    File {
        mode: u32,
        bytes: u64,
        sha256: String,
    },
    Link {
        target: PathBuf,
    },
}
#[derive(Default)]
struct Snapshot {
    entries: BTreeMap<PathBuf, Entry>,
    stamps: BTreeMap<PathBuf, Option<Stamp>>,
    bytes: u64,
    scratch: Vec<u8>,
}
impl Snapshot {
    fn recheck(&self) -> Result<(), String> {
        for (path, value) in &self.stamps {
            if Stamp::read(path)? != *value {
                return Err("App or preferences changed during update preservation; all projects remain open".into());
            }
        }
        Ok(())
    }
    /// Read actual current allowlisted bytes under the final writer authority.
    /// HFS+ timestamps can collide; metadata never substitutes for this read.
    fn authenticate_profile(&self, root: &Path) -> Result<(), String> {
        let mut total = 0_u64;
        for relative in crate::settings::update_files() {
            let path = root.join(&relative);
            let current = Stamp::read(&path)?;
            match (self.entries.get(&relative), self.stamps.get(&path), current) {
                (None, Some(None), None) => {}
                (
                    Some(Entry::File {
                        mode,
                        bytes,
                        sha256,
                    }),
                    Some(Some(_)),
                    Some(value),
                ) => {
                    if value.kind() != u32::from(libc::S_IFREG)
                        || value.links != 1
                        || !value.safe()
                        || value.mode & 0o7777 != *mode
                        || value.bytes != *bytes
                    {
                        return Err(HOLD.into());
                    }
                    let actual = installed::bounded(&path, 4 * 1024 * 1024)?;
                    total = total.checked_add(actual.len() as u64).ok_or(HOLD)?;
                    if total > MAX_PROFILE_BYTES
                        || actual.len() as u64 != *bytes
                        || digest(&actual) != *sha256
                    {
                        return Err(HOLD.into());
                    }
                }
                _ => return Err(HOLD.into()),
            }
            #[cfg(test)]
            tests::after_profile_entry(&relative)?;
        }
        Ok(())
    }
    fn copy(
        &mut self,
        root: &Path,
        relative: &Path,
        destination: &Path,
        app: bool,
        limit: u64,
        cancel: &AtomicBool,
    ) -> Result<(), String> {
        bounded_relative(relative)?;
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err("Update preservation cancelled; original app and settings retained".into());
        }
        if self.stamps.len() >= MAX_FILES {
            return Err("Update checkpoint exceeds its file budget".into());
        }
        let source = root.join(relative);
        let value = Stamp::read(&source)?.ok_or(HOLD)?;
        if !value.safe() && value.kind() != u32::from(libc::S_IFLNK) {
            return Err(HOLD.into());
        }
        let target = destination.join(relative);
        let mode = value.mode & 0o777;
        let entry = match value.kind() {
            k if k == u32::from(libc::S_IFDIR) => {
                // Work privately with owner write permission even when the
                // source is read-only; preserve its exact mode after children.
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&target)
                    .map_err(|_| HOLD)?;
                let directory = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_NONBLOCK)
                    .open(&target)
                    .map_err(|_| HOLD)?;
                let owned = Stamp::read(&target)?.ok_or(HOLD)?;
                let opened = directory.metadata().map_err(|_| HOLD)?;
                if owned.kind() != u32::from(libc::S_IFDIR)
                    || owned.uid != unsafe { libc::geteuid() }
                    || opened.dev() != owned.device
                    || opened.ino() != owned.inode
                {
                    return Err(HOLD.into());
                }
                let mut children = Vec::new();
                for entry in fs::read_dir(&source).map_err(|_| HOLD)? {
                    if children.len() >= MAX_FILES {
                        return Err(HOLD.into());
                    }
                    children.push(entry.map_err(|_| HOLD)?.file_name());
                }
                children.sort();
                self.stamps.insert(source.clone(), Some(value.clone()));
                self.entries
                    .insert(relative.into(), Entry::Directory { mode });
                for child in children {
                    self.copy(root, &relative.join(child), destination, app, limit, cancel)?;
                }
                directory
                    .set_permissions(fs::Permissions::from_mode(mode))
                    .map_err(|_| HOLD)?;
                let restored = Stamp::read(&target)?.ok_or(HOLD)?;
                if restored.kind() != u32::from(libc::S_IFDIR)
                    || restored.device != owned.device
                    || restored.inode != owned.inode
                    || restored.uid != owned.uid
                    || restored.mode & 0o7777 != mode
                {
                    return Err(HOLD.into());
                }
                directory.sync_all().map_err(|_| HOLD)?;
                Entry::Directory { mode }
            }
            k if k == u32::from(libc::S_IFREG) && value.links == 1 => {
                self.bytes = self.bytes.checked_add(value.bytes).ok_or(HOLD)?;
                if self.bytes > limit || (!app && value.bytes > 4 * 1024 * 1024) {
                    return Err("Update checkpoint exceeds its byte budget".into());
                }
                let mut input = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&source)
                    .map_err(|_| HOLD)?;
                let opened = input.metadata().map_err(|_| HOLD)?;
                if opened.dev() != value.device || opened.ino() != value.inode {
                    return Err(HOLD.into());
                }
                let mut output = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&target)
                    .map_err(|_| HOLD)?;
                let mut hash = Context::new(&SHA256);
                let mut count = 0_u64;
                if self.scratch.is_empty() {
                    self.scratch.resize(64 * 1024, 0);
                }
                let buffer = &mut self.scratch;
                loop {
                    if cancel.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(
                            "Update preservation cancelled; original app and settings retained"
                                .into(),
                        );
                    }
                    let length = input.read(buffer).map_err(|_| HOLD)?;
                    if length == 0 {
                        break;
                    }
                    count = count.checked_add(length as u64).ok_or(HOLD)?;
                    if count > value.bytes {
                        return Err(HOLD.into());
                    }
                    hash.update(&buffer[..length]);
                    output.write_all(&buffer[..length]).map_err(|_| HOLD)?;
                }
                if count != value.bytes {
                    return Err(HOLD.into());
                }
                output
                    .set_permissions(fs::Permissions::from_mode(mode))
                    .map_err(|_| HOLD)?;
                output.sync_all().map_err(|_| HOLD)?;
                Entry::File {
                    mode,
                    bytes: count,
                    sha256: hex(hash.finish().as_ref()),
                }
            }
            k if k == u32::from(libc::S_IFLNK) && app => {
                if value.mode & 0o7777 != 0o777
                    || value.links != 1
                    || (value.uid != 0 && value.uid != unsafe { libc::geteuid() })
                {
                    return Err(HOLD.into());
                }
                let link = fs::read_link(&source).map_err(|_| HOLD)?;
                if !pinned_link(relative, &link)? {
                    return Err(HOLD.into());
                }
                std::os::unix::fs::symlink(&link, &target).map_err(|_| HOLD)?;
                normalize_owned_link(&target, &link)?;
                Entry::Link { target: link }
            }
            _ => return Err(HOLD.into()),
        };
        if Stamp::read(&source)? != Some(value.clone()) {
            return Err(HOLD.into());
        }
        self.stamps.insert(source, Some(value));
        self.entries.insert(relative.into(), entry);
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    target: PathBuf,
    publisher_team_id: String,
    build_record: String,
    app: BTreeMap<PathBuf, Entry>,
    profile: BTreeMap<PathBuf, Entry>,
}
fn validate_manifest(value: &Manifest) -> Result<(), String> {
    if value.schema != 1
        || !value.target.is_absolute()
        || value.build_record.len() > 512
        || value.publisher_team_id.len() != 10
        || !value
            .publisher_team_id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        || value.app.len() > MAX_FILES
        || value.profile.len() > 15
    {
        return Err(HOLD.into());
    }
    for (entries, limit) in [
        (&value.app, MAX_APP_BYTES),
        (&value.profile, MAX_PROFILE_BYTES),
    ] {
        if !matches!(entries.get(Path::new("")), Some(Entry::Directory { .. })) {
            return Err(HOLD.into());
        }
        let mut bytes = 0_u64;
        for (path, entry) in entries {
            bounded_relative(path)?;
            let name = path.to_str().ok_or(HOLD)?;
            if !name.is_empty()
                && !matches!(
                    entries.get(path.parent().ok_or(HOLD)?),
                    Some(Entry::Directory { .. })
                )
            {
                return Err(HOLD.into());
            }
            match entry {
                Entry::Directory { mode } | Entry::File { mode, .. } if *mode & !0o755 != 0 => {
                    return Err(HOLD.into())
                }
                Entry::File {
                    bytes: length,
                    sha256,
                    ..
                } => {
                    bytes = bytes.checked_add(*length).ok_or(HOLD)?;
                    if sha256.len() != 64
                        || !sha256
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    {
                        return Err(HOLD.into());
                    }
                }
                Entry::Link { target } if !pinned_link(path, target)? => return Err(HOLD.into()),
                _ => {}
            }
        }
        if bytes > limit {
            return Err(HOLD.into());
        }
    }
    let allowed = crate::settings::update_files();
    for (path, entry) in &value.profile {
        if path.as_os_str().is_empty() || path == Path::new("migration-backup") {
            if !matches!(entry, Entry::Directory { mode: 0o700 }) {
                return Err(HOLD.into());
            }
        } else if !allowed.contains(path)
            || !matches!(entry, Entry::File { bytes, .. } if *bytes <= 4 * 1024 * 1024)
        {
            return Err(HOLD.into());
        }
    }
    if !matches!(
        value.profile.get(Path::new("profile.json")),
        Some(Entry::File { .. })
    ) || !matches!(
        value.profile.get(Path::new("migration-backup")),
        Some(Entry::Directory { mode: 0o700 })
    ) {
        return Err(HOLD.into());
    }
    Ok(())
}
fn bounded_relative(path: &Path) -> Result<(), String> {
    let name = path.to_str().ok_or(PATH_BUDGET)?;
    if name.len() > MAX_PATH_BYTES
        || path.components().count() > MAX_DEPTH
        || (!name.is_empty()
            && name.split('/').any(|part| {
                part.is_empty()
                    || part == "."
                    || part == ".."
                    || part.contains('\\')
                    || part.contains('\0')
            }))
    {
        return Err(PATH_BUDGET.into());
    }
    Ok(())
}

fn pinned_link(relative: &Path, target: &Path) -> Result<bool, String> {
    let prefix = Path::new("Contents/Frameworks/Sparkle.framework");
    let name = relative
        .strip_prefix(prefix)
        .map_err(|_| HOLD)?
        .to_str()
        .ok_or(HOLD)?;
    let pin: serde_json::Value = serde_json::from_str(include_str!(
        "../../../packaging/sparkle/sparkle-2.10.0.json"
    ))
    .map_err(|_| HOLD)?;
    let expected = &pin["framework_entries"][name];
    Ok(expected["kind"] == "symlink" && expected["target"].as_str().map(Path::new) == Some(target))
}

/// Darwin applies umask when creating links. lchmod never follows the target;
/// this is used only after creating an exact pinned link in our owned copy.
fn normalize_owned_link(path: &Path, target: &Path) -> Result<(), String> {
    let before = Stamp::read(path)?.ok_or(HOLD)?;
    if before.kind() != u32::from(libc::S_IFLNK)
        || before.links != 1
        || before.uid != unsafe { libc::geteuid() }
        || fs::read_link(path).map_err(|_| HOLD)? != target
    {
        return Err(HOLD.into());
    }
    let name = CString::new(path.as_os_str().as_bytes()).map_err(|_| HOLD)?;
    if unsafe { libc::lchmod(name.as_ptr(), 0o777) } != 0 {
        return Err(HOLD.into());
    }
    let after = Stamp::read(path)?.ok_or(HOLD)?;
    if after.kind() != u32::from(libc::S_IFLNK)
        || after.device != before.device
        || after.inode != before.inode
        || after.uid != before.uid
        || after.gid != before.gid
        || after.links != 1
        || after.mode & 0o7777 != 0o777
        || fs::read_link(path).map_err(|_| HOLD)? != target
    {
        return Err(HOLD.into());
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum TreeOwner {
    PrivateCapture,
    InstalledHost,
}
impl TreeOwner {
    fn permits(self, uid: u32, current: u32) -> bool {
        uid == current || (matches!(self, Self::InstalledHost) && uid == 0)
    }
}
fn check_copy(
    root: &Path,
    entries: &BTreeMap<PathBuf, Entry>,
    cancel: &AtomicBool,
) -> Result<Snapshot, String> {
    check_tree(root, entries, cancel, TreeOwner::PrivateCapture)
}
fn check_tree(
    root: &Path,
    entries: &BTreeMap<PathBuf, Entry>,
    cancel: &AtomicBool,
    owner: TreeOwner,
) -> Result<Snapshot, String> {
    for relative in entries.keys() {
        bounded_relative(relative)?;
    }
    let mut result = Snapshot::default();
    let mut buffer = vec![0_u8; 64 * 1024];
    for (relative, expected) in entries {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(HOLD.into());
        }
        let path = root.join(relative);
        let value = Stamp::read(&path)?.ok_or(HOLD)?;
        match expected {
            Entry::Directory { mode } => {
                if value.kind() != u32::from(libc::S_IFDIR) || value.mode & 0o7777 != *mode {
                    return Err(HOLD.into());
                }
                let children = entries
                    .keys()
                    .filter(|p| !p.as_os_str().is_empty() && p.parent() == Some(relative))
                    .count();
                let mut actual = 0;
                for child in fs::read_dir(&path).map_err(|_| HOLD)? {
                    let name = relative.join(child.map_err(|_| HOLD)?.file_name());
                    bounded_relative(&name)?;
                    if !entries.contains_key(&name) || actual >= MAX_FILES {
                        return Err(HOLD.into());
                    }
                    actual += 1;
                }
                if actual != children {
                    return Err(HOLD.into());
                }
            }
            Entry::Link { target } => {
                if value.kind() != u32::from(libc::S_IFLNK)
                    || value.mode & 0o7777 != 0o777
                    || value.links != 1
                    || fs::read_link(&path).map_err(|_| HOLD)? != *target
                {
                    return Err(HOLD.into());
                }
            }
            Entry::File {
                mode,
                bytes,
                sha256,
            } => {
                if value.kind() != u32::from(libc::S_IFREG)
                    || value.links != 1
                    || value.mode & 0o7777 != *mode
                    || value.bytes != *bytes
                {
                    return Err(HOLD.into());
                }
                let mut file = OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                    .open(&path)
                    .map_err(|_| HOLD)?;
                let opened = file.metadata().map_err(|_| HOLD)?;
                if opened.dev() != value.device || opened.ino() != value.inode {
                    return Err(HOLD.into());
                }
                let mut hash = Context::new(&SHA256);
                let mut count = 0_u64;
                loop {
                    if cancel.load(std::sync::atomic::Ordering::Acquire) {
                        return Err(HOLD.into());
                    }
                    let length = file.read(&mut buffer).map_err(|_| HOLD)?;
                    if length == 0 {
                        break;
                    }
                    count += length as u64;
                    if count > *bytes {
                        return Err(HOLD.into());
                    }
                    hash.update(&buffer[..length]);
                }
                if count != *bytes || hex(hash.finish().as_ref()) != *sha256 {
                    return Err(HOLD.into());
                }
            }
        }
        if !owner.permits(value.uid, unsafe { libc::geteuid() })
            || Stamp::read(&path)? != Some(value.clone())
        {
            return Err(HOLD.into());
        }
        result.stamps.insert(path, Some(value));
    }
    Ok(result)
}

/// No API here restores a bundle/profile, relaunches, or releases an armed hold.
/// A future coordinator must authenticate a native outcome before adding those.
pub(super) struct Prepared {
    pub verified: installed::Verified,
    path: PathBuf,
    identity: Stamp,
    manifest: Vec<u8>,
    app: Snapshot,
    profile: Snapshot,
    copy: Snapshot,
    _profile: UpdateProfile,
    _owner: super::coordinator::Owner,
    current_profile: Option<ProfileCapture>,
}
impl Prepared {
    /// An advisory change detector, never current app/copy authentication.
    pub fn recheck_metadata(&self) -> Result<(), String> {
        self.check_identity()?;
        self.verified.recheck()?;
        self.app.recheck()?;
        if self.current_profile.is_none() {
            self.profile.recheck()?;
        }
        self.copy.recheck()?;
        Ok(())
    }
    fn check_identity(&self) -> Result<(), String> {
        let value = Stamp::read(&self.path)?.ok_or(HOLD)?;
        if value.device != self.identity.device
            || value.inode != self.identity.inode
            || value.mode != self.identity.mode
            || value.uid != self.identity.uid
            || value.gid != self.identity.gid
        {
            return Err(HOLD.into());
        }
        Ok(())
    }
    pub fn publish(&mut self, workspace: &crate::workspace::Registry) -> Result<(), String> {
        let reviewed = crate::workspace::lifecycle::update_readiness(workspace)?;
        crate::workspace::lifecycle::publish_update_checkpoint(workspace, &reviewed, |tools| {
            self.check_identity()?;
            capture_current_profile(
                &self._profile,
                tools,
                &self.path,
                &self.manifest,
                &mut self.current_profile,
            )
        })
    }
    pub fn retire_profile_capture(&self) -> Result<(), String> {
        self.check_identity()?;
        if Stamp::read(&self.path.join("native-start-intent.json"))?.is_some() {
            return Err(HOLD.into());
        }
        match &self.current_profile {
            Some(capture) => capture.retire(&self.path),
            None => Ok(()),
        }
    }
    pub(super) fn check_in_owner(
        &mut self,
        ctx: &eframe::egui::Context,
    ) -> Result<super::coordinator::RemoteCheck, String> {
        require_captured_handoff()?;
        self.check_identity()?;
        let manifest: Manifest = serde_json::from_slice(&self.manifest).map_err(|_| HOLD)?;
        validate_manifest(&manifest)?;
        self._owner.check(manifest.publisher_team_id, ctx)
    }
    /// Unreachable before the separate captured-handoff refusal is replaced by
    /// a complete native contract. Publishing a checkpoint cannot call this.
    pub fn arm_native_intent(&self) -> Result<(), String> {
        self.check_identity()?;
        if read_private(&self.path.join("checkpoint.json"), 1024 * 1024)? != self.manifest {
            return Err(HOLD.into());
        }
        record_intent(&self.path, &self.manifest)
    }
    /// Only the synchronous startUpdater:NO result may call this. Errors leave
    /// the durable startup barrier in place even when in-memory startup failed.
    pub fn not_started(&self) -> Result<(), String> {
        self.check_identity()?;
        if read_private(&self.path.join("checkpoint.json"), 1024 * 1024)? != self.manifest {
            return Err(HOLD.into());
        }
        record_unstarted(&self.path, &self.manifest)
    }
}

/// A bounded current-profile observation, not a committed exit or restore grant.
/// Initial worker app/profile/capture.json remain immutable and independently bound.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentProfile {
    schema: u32,
    initial_manifest_sha256: String,
    profile: BTreeMap<PathBuf, Entry>,
}
struct ProfileCapture {
    record: Vec<u8>,
}
impl ProfileCapture {
    fn retire(&self, path: &Path) -> Result<(), String> {
        self.retire_with_sync(path, crate::server::sync_parent)
    }
    fn retire_with_sync(
        &self,
        path: &Path,
        sync_parent: impl FnOnce(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        if read_private(&path.join("current-profile.json"), 1024 * 1024)? != self.record {
            return Err(HOLD.into());
        }
        let value = serde_json::to_vec(&serde_json::json!({
            "schema": 1,
            "current_profile_sha256": digest(&self.record),
        }))
        .map_err(|_| HOLD)?;
        let retired = path.join("current-profile-retired.json");
        if let Some(witness) = Stamp::read(&retired)? {
            if read_marker(&retired)? != value {
                return Err(HOLD.into());
            }
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&retired)
                .map_err(|_| HOLD)?;
            let opened = file.metadata().map_err(|_| HOLD)?;
            if opened.dev() != witness.device || opened.ino() != witness.inode {
                return Err(HOLD.into());
            }
            // A prior attempt may have failed file sync or directory sync. An
            // existing matching byte string alone never proves publication.
            file.sync_all().map_err(|_| HOLD)?;
            if Stamp::read(&retired)? != Some(witness) {
                return Err(HOLD.into());
            }
        } else {
            write_new_file(&retired, &value)?;
        }
        sync_parent(&retired)?;
        if read_marker(&retired)? != value
            || read_private(&path.join("current-profile.json"), 1024 * 1024)? != self.record
        {
            return Err(HOLD.into());
        }
        Ok(())
    }
}
fn validate_current_profile(value: &CurrentProfile, initial: &[u8]) -> Result<(), String> {
    if value.schema != 1 || value.initial_manifest_sha256 != digest(initial) {
        return Err(HOLD.into());
    }
    let mut original: Manifest = serde_json::from_slice(initial).map_err(|_| HOLD)?;
    validate_manifest(&original)?;
    original.profile = value.profile.clone();
    validate_manifest(&original)
}
/// Strict actual backup consumer used before returning a current-profile capture.
/// No caller can turn this observation into native start/exit/restore authority.
fn read_current_profile(path: &Path, initial: &[u8]) -> Result<Vec<u8>, String> {
    if read_private(&path.join("capture.json"), 1024 * 1024)? != initial
        || read_private(&path.join("checkpoint.json"), 1024 * 1024)? != initial
        || Stamp::read(&path.join("current-profile-retired.json"))?.is_some()
    {
        return Err(HOLD.into());
    }
    let bytes = read_private(&path.join("current-profile.json"), 1024 * 1024)?;
    let value: CurrentProfile = serde_json::from_slice(&bytes).map_err(|_| HOLD)?;
    validate_current_profile(&value, initial)?;
    check_copy(
        &path.join("current-profile"),
        &value.profile,
        &AtomicBool::new(false),
    )?;
    if read_private(&path.join("current-profile.json"), 1024 * 1024)? != bytes {
        return Err(HOLD.into());
    }
    Ok(bytes)
}
/// Runs only on the owning idle UI thread inside the shared final validator:
/// sorted engines -> registry (no admitted file writers) -> actual ToolStore.
/// Each bounded file pass totals <=56 MiB; app copying/native checks stay outside.
fn capture_current_profile(
    profile: &UpdateProfile,
    tools: &crate::tool_settings::Store,
    path: &Path,
    initial: &[u8],
    capture: &mut Option<ProfileCapture>,
) -> Result<(), String> {
    if capture.is_some()
        || read_private(&path.join("capture.json"), 1024 * 1024)? != initial
        || Stamp::read(&path.join("checkpoint.json"))?.is_some()
        || Stamp::read(&path.join("current-profile.json"))?.is_some()
        || Stamp::read(&path.join("current-profile-retired.json"))?.is_some()
        || Stamp::read(&path.join("native-start-intent.json"))?.is_some()
        || Stamp::read(&path.join("native-not-started.json"))?.is_some()
    {
        return Err(HOLD.into());
    }
    let root = profile.locations.profile();
    tools.authenticate_update_profile(&root)?;
    let destination = path.join("current-profile");
    let current = copy_profile(profile, &destination, &AtomicBool::new(false))?;
    let value = CurrentProfile {
        schema: 1,
        initial_manifest_sha256: digest(initial),
        profile: current.entries.clone(),
    };
    validate_current_profile(&value, initial)?;
    let record = serde_json::to_vec(&value).map_err(|_| HOLD)?;
    if record.len() > 1024 * 1024 {
        return Err(HOLD.into());
    }
    check_copy(&destination, &current.entries, &AtomicBool::new(false))?;
    crate::server::sync_parent(&destination.join("migration-backup"))?;
    crate::server::sync_parent(&destination)?;
    current.authenticate_profile(&root)?;
    tools.authenticate_update_profile(&root)?;
    // Advisory only: catches ordinary between-file replacement after byte reads,
    // not a simultaneous epoch against an uncooperating/coarse-time same-user writer.
    current.recheck()?;
    // The application's retained Prepared already owns this exact capture. Set
    // its record before publication: every later error must remain retireable.
    *capture = Some(ProfileCapture {
        record: record.clone(),
    });
    write_new(&path.join("checkpoint.json"), initial)?;
    write_new(&path.join("current-profile.json"), &record)?;
    // Read back the actual durable copy and strict record before success.
    if read_current_profile(path, initial)? != record {
        return Err(HOLD.into());
    }
    #[cfg(test)]
    tests::after_current_profile_record()?;
    // The owning writer locks still exclude all admitted app writers here.
    tools.authenticate_update_profile(&root)?;
    current.recheck()?;
    Ok(())
}

fn publish_profile_checkpoint(
    profile: &Snapshot,
    root: &Path,
    path: &Path,
    manifest: &[u8],
) -> Result<(), String> {
    profile.authenticate_profile(root)?;
    // Detect ordinary external replacement after an earlier sequential read.
    // This complements byte authentication; it is not a multi-file content epoch.
    profile.recheck()?;
    write_new(&path.join("checkpoint.json"), manifest)
}

/// A worker-verified mutable live NSBundle is not a captured native handoff.
/// Keep this source boundary independent of public pins and startup_enabled.
pub(super) fn require_captured_handoff() -> Result<(), String> {
    Err("App updating is not available in this development build.".into())
}

/// Only the copied owner may arm its own native startup. This is still blocked
/// before any filesystem access by the independent captured-handoff refusal.
pub(super) fn arm_owner_check(
    app: &Path,
    expected_manifest: &str,
    profile: &UpdateProfile,
) -> Result<ArmedCheck, String> {
    require_captured_handoff()?;
    profile.validate()?;
    let path = app.parent().ok_or(HOLD)?;
    let manifest = read_private(&path.join("capture.json"), 1024 * 1024)?;
    if digest(&manifest) != expected_manifest
        || Stamp::read(&path.join("native-start-intent.json"))?.is_some()
        || Stamp::read(&path.join("native-not-started.json"))?.is_some()
    {
        return Err(HOLD.into());
    }
    read_current_profile(path, &manifest)?;
    record_intent(path, &manifest)?;
    let armed = ArmedCheck {
        app: app.into(),
        intent: read_marker(&path.join("native-start-intent.json"))?,
        manifest,
        owner_pid: std::process::id(),
    };
    armed.check_records(profile)?;
    Ok(armed)
}
pub(super) fn owner_check_not_started(app: &Path, manifest: &[u8]) -> Result<(), String> {
    let path = app.parent().ok_or(HOLD)?;
    if read_private(&path.join("capture.json"), 1024 * 1024)? != manifest {
        return Err(HOLD.into());
    }
    record_unstarted(path, manifest)
}

// Constructed only by this process's actual arm/write/readback boundary.
// This retained owner receipt is never deserialized from a status or marker.
pub(super) struct ArmedCheck {
    app: PathBuf,
    manifest: Vec<u8>,
    intent: Vec<u8>,
    owner_pid: u32,
}
impl ArmedCheck {
    fn check_records(&self, profile: &UpdateProfile) -> Result<PathBuf, String> {
        profile.validate()?;
        let path = self.app.parent().ok_or(HOLD)?;
        if !self.app.is_absolute()
            || self.app.file_name() != Some(std::ffi::OsStr::new("PeerBrush.app"))
            || path.parent() != Some(checkpoints(&profile.locations).as_path())
            || self.owner_pid != std::process::id()
            || !private(path, false)?
            || read_private(&path.join("capture.json"), 1024 * 1024)? != self.manifest
            || read_marker(&path.join("native-start-intent.json"))? != self.intent
            || Stamp::read(&path.join("native-not-started.json"))?.is_some()
        {
            return Err(HOLD.into());
        }
        let intent: Intent = serde_json::from_slice(&self.intent).map_err(|_| HOLD)?;
        if intent.schema != 1
            || intent.owner_pid != self.owner_pid
            || intent.manifest_sha256 != digest(&self.manifest)
            || intent.checkpoint
                != path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or(HOLD)?
        {
            return Err(HOLD.into());
        }
        read_current_profile(path, &self.manifest)?;
        Ok(path.into())
    }
    pub(super) fn not_started(&self) -> Result<(), String> {
        owner_check_not_started(&self.app, &self.manifest)
    }
    pub(super) fn recheck(
        &self,
        profile: &UpdateProfile,
        parent_executable: &Path,
        configuration: ApprovedCheck,
        cancel: Arc<AtomicBool>,
    ) -> Result<OwnerBundles, String> {
        require_captured_handoff()?;
        self.recheck_with(profile, parent_executable, &configuration, &cancel, |app| {
            installed::verify(app, configuration.clone(), cancel.clone())?.recheck()
        })
    }
    fn recheck_with(
        &self,
        profile: &UpdateProfile,
        parent_executable: &Path,
        configuration: &ApprovedCheck,
        cancel: &AtomicBool,
        verify_native: impl FnMut(PathBuf) -> Result<(), String>,
    ) -> Result<OwnerBundles, String> {
        let path = self.check_records(profile)?;
        let root = checkpoints(&profile.locations);
        private(&root, false)?;
        let mut before = entries(&root)?;
        if !before.contains(&path) {
            return Err(HOLD.into());
        }
        for other in &before {
            if other != &path {
                inspect_unstarted_attempt(other)?;
            }
        }
        let result = inspect_owner_tree_with(
            &self.app,
            profile,
            parent_executable,
            &digest(&self.manifest),
            configuration,
            cancel,
            verify_native,
        )?;
        self.check_records(profile)?;
        let mut after = entries(&root)?;
        before.sort();
        after.sort();
        if before != after {
            return Err(HOLD.into());
        }
        for other in &after {
            if other != &path {
                inspect_unstarted_attempt(other)?;
            }
        }
        Ok(result)
    }
}

fn record_intent(path: &Path, manifest: &[u8]) -> Result<(), String> {
    let value = Intent {
        schema: 1,
        checkpoint: path.file_name().unwrap().to_str().unwrap().into(),
        manifest_sha256: digest(manifest),
        owner_pid: std::process::id(),
    };
    write_new(
        &path.join("native-start-intent.json"),
        &serde_json::to_vec(&value).map_err(|_| HOLD)?,
    )
}
fn record_unstarted(path: &Path, manifest: &[u8]) -> Result<(), String> {
    let intent = read_marker(&path.join("native-start-intent.json"))?;
    let value: Intent = serde_json::from_slice(&intent).map_err(|_| HOLD)?;
    if value.owner_pid != std::process::id()
        || value.manifest_sha256 != digest(manifest)
        || value.checkpoint != path.file_name().unwrap().to_str().unwrap()
        || value.schema != 1
    {
        return Err(HOLD.into());
    }
    write_new(
        &path.join("native-not-started.json"),
        &serde_json::to_vec(&Unstarted {
            schema: 1,
            intent_sha256: digest(&intent),
            owner_pid: std::process::id(),
        })
        .map_err(|_| HOLD)?,
    )
}

fn copy_profile(
    profile: &UpdateProfile,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<Snapshot, String> {
    profile.validate()?;
    let profile_root = profile.locations.profile();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(destination)
        .map_err(|_| HOLD)?;
    fs::DirBuilder::new()
        .mode(0o700)
        .create(destination.join("migration-backup"))
        .map_err(|_| HOLD)?;
    let mut profile_snapshot = Snapshot::default();
    profile_snapshot
        .entries
        .insert(PathBuf::new(), Entry::Directory { mode: 0o700 });
    profile_snapshot.entries.insert(
        PathBuf::from("migration-backup"),
        Entry::Directory { mode: 0o700 },
    );
    profile_snapshot
        .stamps
        .insert(profile_root.clone(), Stamp::read(&profile_root)?);
    let backup = Stamp::read(&profile_root.join("migration-backup"))?;
    if backup
        .as_ref()
        .is_some_and(|value| value.kind() != u32::from(libc::S_IFDIR) || !value.safe())
    {
        return Err(HOLD.into());
    }
    profile_snapshot
        .stamps
        .insert(profile_root.join("migration-backup"), backup);
    for relative in crate::settings::update_files() {
        let source = profile_root.join(&relative);
        if Stamp::read(&source)?.is_some() {
            profile_snapshot.copy(
                &profile_root,
                &relative,
                destination,
                false,
                MAX_PROFILE_BYTES,
                cancel,
            )?;
        } else {
            profile_snapshot.stamps.insert(source, None);
        }
    }
    profile_snapshot.recheck()?;
    Ok(profile_snapshot)
}

pub(super) fn prepare(
    app: PathBuf,
    configuration: ApprovedCheck,
    profile: UpdateProfile,
    cancel: Arc<AtomicBool>,
) -> Result<Prepared, String> {
    profile.validate()?;
    startup_guard(&profile.locations)?;
    let verified = installed::verify(app.clone(), configuration.clone(), cancel.clone())?;
    private(&profile.locations.data, true)?;
    let root = checkpoints(&profile.locations);
    private(&root, true)?;
    crate::server::sync_parent(&profile.locations.data)?;
    crate::server::sync_parent(&root)?;
    if entries(&root)?.len() >= MAX_CHECKPOINTS {
        return Err("Keep existing update recovery copies; the checkpoint budget is full".into());
    }
    let path = root.join(uuid::Uuid::new_v4().to_string());
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&path)
        .map_err(|_| HOLD)?;
    crate::server::sync_parent(&path)?;
    let mut app_snapshot = Snapshot::default();
    app_snapshot.copy(
        &app,
        Path::new(""),
        &path.join("PeerBrush.app"),
        true,
        MAX_APP_BYTES,
        &cancel,
    )?;
    let destination = path.join("profile");
    let profile_snapshot = copy_profile(&profile, &destination, &cancel)?;
    profile.validate()?;
    app_snapshot.recheck()?;
    profile_snapshot.recheck()?;
    verified.recheck()?;
    // Revalidate the complete copied signed app using native publisher/resource,
    // runtime, identifier, entitlements and exact pinned framework policy.
    installed::verify(
        path.join("PeerBrush.app"),
        configuration.clone(),
        cancel.clone(),
    )?
    .recheck()?;
    let mut copy = check_copy(&path.join("PeerBrush.app"), &app_snapshot.entries, &cancel)?;
    copy.stamps
        .extend(check_copy(&destination, &profile_snapshot.entries, &cancel)?.stamps);
    // Native validation covers signed resources. The independent byte inventory
    // records all copied bytes, including known current preferences, privately.
    let manifest = Manifest {
        schema: 1,
        target: app.clone(),
        publisher_team_id: configuration.publisher_team_id.clone(),
        build_record: crate::build_info::json().into(),
        app: app_snapshot.entries.clone(),
        profile: profile_snapshot.entries.clone(),
    };
    validate_manifest(&manifest)?;
    let manifest = serde_json::to_vec(&manifest).map_err(|_| HOLD)?;
    if manifest.len() > 1024 * 1024 {
        return Err(HOLD.into());
    }
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err("Update preservation cancelled; original app and settings retained".into());
    }
    app_snapshot.recheck()?;
    profile_snapshot.recheck()?;
    verified.recheck()?;
    copy.recheck()?;
    File::open(destination.join("migration-backup"))
        .map_err(|_| HOLD)?
        .sync_all()
        .map_err(|_| HOLD)?;
    File::open(&destination)
        .map_err(|_| HOLD)?
        .sync_all()
        .map_err(|_| HOLD)?;
    let identity = Stamp::read(&path)?.ok_or(HOLD)?;
    // This is the worker observation, not the final writer-locked publication.
    // The child authenticates it through inherited IPC, never an argv path.
    write_new(&path.join("capture.json"), &manifest)?;
    let owner = super::coordinator::launch(
        &path.join("PeerBrush.app"),
        &profile,
        path.file_name()
            .and_then(|name| name.to_str())
            .ok_or(HOLD)?,
        digest(&manifest),
        &configuration,
        &cancel,
    )?;
    Ok(Prepared {
        verified,
        path,
        identity,
        manifest,
        app: app_snapshot,
        profile: profile_snapshot,
        copy,
        _profile: profile,
        _owner: owner,
        current_profile: None,
    })
}

/// Only the authenticated same-binary child uses this derived private layout.
/// Neither target nor storage paths are supplied through command arguments.
pub(super) fn owner_copy(
    executable: &Path,
    locations: &Locations,
    name: &str,
) -> Result<PathBuf, String> {
    if uuid::Uuid::parse_str(name).map_err(|_| HOLD)?.to_string() != name {
        return Err(HOLD.into());
    }
    let root = checkpoints(locations);
    let attempt = root.join(name);
    let app = attempt.join("PeerBrush.app");
    if !executable.is_absolute() || executable != app.join("Contents/MacOS/peerbrush") {
        return Err(HOLD.into());
    }
    if !private(&root, false)? || !private(&attempt, false)? {
        return Err(HOLD.into());
    }
    Ok(app)
}

/// Paths derived only by the authenticated copied-owner consumer below. These
/// describe bundle roles; they grant neither native startup nor replacement.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct OwnerBundles {
    pub(super) owner: PathBuf,
    pub(super) host: PathBuf,
    pub(super) application: PathBuf,
}
impl OwnerBundles {
    pub(super) fn require_running_copy(
        &self,
        executable: &Path,
        main_bundle: &Path,
    ) -> Result<(), String> {
        if !self.owner.is_absolute()
            || !self.host.is_absolute()
            || self.owner == self.host
            || self.application != self.host
            || executable != self.owner.join("Contents/MacOS/peerbrush")
            || main_bundle != self.owner
        {
            return Err(HOLD.into());
        }
        Ok(())
    }
    pub(super) fn framework(&self) -> PathBuf {
        self.owner.join("Contents/Frameworks/Sparkle.framework")
    }
}

pub(super) fn inspect_owner_copy(
    app: &Path,
    profile: &UpdateProfile,
    parent_executable: &Path,
    expected_manifest: &str,
    configuration: ApprovedCheck,
    cancel: Arc<AtomicBool>,
) -> Result<OwnerBundles, String> {
    inspect_owner_copy_with(
        app,
        profile,
        parent_executable,
        expected_manifest,
        &configuration,
        &cancel,
        |app| installed::verify(app, configuration.clone(), cancel.clone())?.recheck(),
    )
}
fn inspect_owner_copy_with(
    app: &Path,
    profile: &UpdateProfile,
    parent_executable: &Path,
    expected_manifest: &str,
    configuration: &ApprovedCheck,
    cancel: &AtomicBool,
    verify_native: impl FnMut(PathBuf) -> Result<(), String>,
) -> Result<OwnerBundles, String> {
    let attempt = app.parent().ok_or(HOLD)?;
    if Stamp::read(&attempt.join("native-start-intent.json"))?.is_some() {
        return Err(HOLD.into());
    }
    startup_guard(&profile.locations)?;
    inspect_owner_tree_with(
        app,
        profile,
        parent_executable,
        expected_manifest,
        configuration,
        cancel,
        verify_native,
    )
}
fn inspect_owner_tree_with(
    app: &Path,
    profile: &UpdateProfile,
    parent_executable: &Path,
    expected_manifest: &str,
    configuration: &ApprovedCheck,
    cancel: &AtomicBool,
    mut verify_native: impl FnMut(PathBuf) -> Result<(), String>,
) -> Result<OwnerBundles, String> {
    let attempt = app.parent().ok_or(HOLD)?;
    let bytes = read_private(&attempt.join("capture.json"), 1024 * 1024)?;
    if digest(&bytes) != expected_manifest {
        return Err(HOLD.into());
    }
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|_| HOLD)?;
    validate_manifest(&manifest)?;
    if manifest.target.join("Contents/MacOS/peerbrush") != parent_executable
        || manifest.build_record != crate::build_info::json()
        || manifest.publisher_team_id != configuration.publisher_team_id
    {
        return Err(HOLD.into());
    }
    profile.validate()?;
    // Actual code identity comes from the kernel audit peer + Security checks.
    // Full signed app/resources/framework checks and independent byte parity
    // establish only this capture, never a future installation/rollback permit.
    verify_native(manifest.target.clone())?;
    verify_native(app.into())?;
    // A same-signer/same-version target is insufficient: authenticate the exact
    // original source tree captured for this owner before choosing bundle roles.
    let installed = check_tree(
        &manifest.target,
        &manifest.app,
        cancel,
        TreeOwner::InstalledHost,
    )?;
    let app_copy = check_copy(app, &manifest.app, cancel)?;
    let preferences = check_copy(&attempt.join("profile"), &manifest.profile, cancel)?;
    installed.recheck()?;
    app_copy.recheck()?;
    preferences.recheck()?;
    if manifest.target == app {
        return Err(HOLD.into());
    }
    Ok(OwnerBundles {
        owner: app.into(),
        host: manifest.target.clone(),
        application: manifest.target,
    })
}

#[cfg(test)]
#[path = "checkpoint_tests.rs"]
mod tests;
