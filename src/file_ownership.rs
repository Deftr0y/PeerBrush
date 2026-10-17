//! Runtime ownership of normalized file paths and physical identities.
use std::{
    collections::HashMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

struct Lock {
    _file: fs::File,
    // Unix inode numbers can be reused after unlink while a logical lock lives.
    // Pin those inodes. Windows replacement must close source/staged handles.
    #[cfg(unix)]
    _source: Option<fs::File>,
    project: String,
    key: String,
}
impl Drop for Lock {
    fn drop(&mut self) {
        // Closing this handle alone can leave flock alive on a duplicated
        // descriptor inherited during a concurrent subprocess launch. The
        // final PeerBrush owner must release it before a new open can claim it.
        // File close remains the fallback if explicit unlock fails.
        let _ = self._file.unlock();
    }
}
fn locks() -> &'static Mutex<HashMap<String, Weak<Lock>>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Weak<Lock>>>> = OnceLock::new();
    LOCKS.get_or_init(Default::default)
}
fn path_key(path: &Path) -> String {
    let h = crate::file_safety::destination_key(path);
    format!("path-{:016x}{:016x}", h[0], h[1])
}
fn identity_key(identity: (u64, u64)) -> String {
    format!("identity-{:016x}-{:016x}", identity.0, identity.1)
}
fn directory() -> Result<PathBuf, String> {
    #[cfg(unix)]
    let suffix = {
        extern "C" {
            fn geteuid() -> u32;
        }
        format!("-{}", unsafe { geteuid() })
    };
    #[cfg(not(unix))]
    let suffix = String::new();
    let dir = std::env::temp_dir().join(format!("PeerBrush-open-files{suffix}"));
    if dir.exists()
        && !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
    {
        return Err("Open-file ownership storage is not a regular directory".into());
    }
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        extern "C" {
            fn geteuid() -> u32;
        }
        let m = fs::metadata(&dir).map_err(|e| e.to_string())?;
        if m.uid() != unsafe { geteuid() } {
            return Err("Open-file storage belongs to another user".into());
        }
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}
fn claim(
    key: String,
    project: &str,
    reuse: bool,
    source: Option<fs::File>,
) -> Result<Arc<Lock>, String> {
    let mut registry = locks().lock().unwrap();
    registry.retain(|_, v| v.strong_count() > 0);
    if let Some(lock) = registry.get(&key).and_then(Weak::upgrade) {
        if reuse && lock.project == project {
            return Ok(lock);
        }
        return Err(format!(
            "File already open in project {}. Select that project; its work is preserved.",
            lock.project
        ));
    }
    let path = directory()?.join(format!("{key}.lock"));
    if path.exists()
        && !fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
    {
        return Err("Open-file lock is not a regular file".into());
    }
    let record = path.with_extension("owner");
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| e.to_string())?;
    if file.try_lock().is_err() {
        let mut project = String::new();
        if let Ok(info) = fs::File::open(&record) {
            let _ = info.take(128).read_to_string(&mut project);
        }
        let project = if project.len() <= 64
            && project
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !project.is_empty()
        {
            format!(" (project {project})")
        } else {
            String::new()
        };
        return Err(format!("File already open or opening in another PeerBrush instance{project}. Return to that instance; its work is preserved."));
    }
    #[cfg(not(unix))]
    drop(source);
    let lock = Arc::new(Lock {
        _file: file,
        #[cfg(unix)]
        _source: source,
        project: project.into(),
        key: key.clone(),
    });
    if record.exists()
        && !fs::symlink_metadata(&record).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
    {
        return Err("Open-file owner record is not a regular file".into());
    }
    let mut info = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(&record)
        .map_err(|e| e.to_string())?;
    info.write_all(project.as_bytes())
        .map_err(|e| e.to_string())?;
    info.sync_all().map_err(|e| e.to_string())?;
    registry.insert(key, Arc::downgrade(&lock));
    Ok(lock)
}
pub struct Ownership {
    pub path: PathBuf,
    project: String,
    files: Mutex<Option<Vec<Arc<Lock>>>>,
}
impl Ownership {
    pub fn open(path: &Path, project: &str) -> Result<Arc<Self>, String> {
        Self::acquire(path, project, false)
    }
    pub fn save(path: &Path, project: &str) -> Result<Arc<Self>, String> {
        Self::acquire(path, project, true)
    }
    fn acquire(path: &Path, project: &str, reuse: bool) -> Result<Arc<Self>, String> {
        let path = crate::file_safety::canonical_destination(path)?;
        let mut keys = vec![(path_key(&path), None)];
        if path.exists() {
            let source = fs::File::open(&path).map_err(|e| e.to_string())?;
            keys.push((
                identity_key(crate::file_safety::identity(&source)?),
                Some(source),
            ));
        }
        keys.sort_by(|a, b| a.0.cmp(&b.0));
        let files = keys
            .into_iter()
            .map(|(key, source)| claim(key, project, reuse, source))
            .collect::<Result<_, _>>()?;
        Ok(Arc::new(Self {
            path,
            project: project.into(),
            files: Mutex::new(Some(files)),
        }))
    }
    /// Own the staged inode before it becomes visible under its destination name.
    pub(crate) fn prepare(&self, temporary: &Path) -> Result<(), String> {
        let source = fs::File::open(temporary).map_err(|e| e.to_string())?;
        let key = identity_key(crate::file_safety::identity(&source)?);
        let lock = claim(key, &self.project, true, Some(source))?;
        self.files
            .lock()
            .unwrap()
            .as_mut()
            .ok_or("File ownership was released")?
            .push(lock);
        Ok(())
    }
    pub(crate) fn published(&self, identity: (u64, u64)) {
        let wanted = [path_key(&self.path), identity_key(identity)];
        if let Some(files) = self.files.lock().unwrap().as_mut() {
            files.retain(|file| wanted.contains(&file.key));
        }
    }
    pub(crate) fn release(&self) {
        self.files.lock().unwrap().take();
    }
}
pub(crate) fn local_project(path: &Path) -> Option<String> {
    let path = crate::file_safety::canonical_destination(path).ok()?;
    let mut keys = vec![path_key(&path)];
    if let Ok(file) = fs::File::open(&path) {
        if let Ok(id) = crate::file_safety::identity(&file) {
            keys.push(identity_key(id));
        }
    }
    let registry = locks().lock().unwrap();
    keys.iter().find_map(|key| {
        registry
            .get(key)
            .and_then(Weak::upgrade)
            .map(|lock| lock.project.clone())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn physical_identity_stays_pinned_after_external_unlink_and_staged_publication() {
        let dir = std::env::temp_dir().join(format!("pb-pinned-owner-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("Artwork.psd");
        fs::write(&path, b"original source").unwrap();
        let owner = Ownership::open(&path, "project").unwrap();
        let pinned = {
            let files = owner.files.lock().unwrap();
            let source = files
                .as_ref()
                .unwrap()
                .iter()
                .find_map(|lock| lock._source.as_ref())
                .unwrap();
            source.try_clone().unwrap()
        };
        let original = crate::file_safety::identity(&pinned).unwrap();
        fs::remove_file(&path).unwrap();
        let other = dir.join("Unrelated.psd");
        fs::write(&other, b"unrelated source").unwrap();
        let other_identity =
            crate::file_safety::identity(&fs::File::open(&other).unwrap()).unwrap();
        assert_ne!(other_identity, original);
        assert!(Ownership::open(&other, "other-project").is_ok());
        let mut contents = String::new();
        (&pinned).read_to_string(&mut contents).unwrap();
        assert_eq!(contents, "original source");
        drop(pinned);
        // The logical owner still retains the unlinked source, even after the
        // test's duplicate handle is closed.
        assert_eq!(
            owner
                .files
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .iter()
                .filter_map(|lock| lock._source.as_ref())
                .count(),
            1
        );
        owner.release();
        let owner = Ownership::save(&path, "project").unwrap();
        let staged = dir.join("Staged.psd");
        fs::write(&staged, b"new source").unwrap();
        let identity = crate::file_safety::identity(&fs::File::open(&staged).unwrap()).unwrap();
        owner.prepare(&staged).unwrap();
        fs::rename(&staged, &path).unwrap();
        owner.published(identity);
        let pins = owner.files.lock().unwrap();
        let sources = pins
            .as_ref()
            .unwrap()
            .iter()
            .filter_map(|lock| lock._source.as_ref())
            .collect::<Vec<_>>();
        assert_eq!(sources.len(), 1);
        assert_eq!(crate::file_safety::identity(sources[0]).unwrap(), identity);
        drop(pins);
        assert!(Ownership::open(&path, "other-project").is_err());
        owner.release();
        assert!(Ownership::open(&path, "other-project").is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn final_owner_releases_os_locks_even_with_duplicated_descriptors() {
        let dir = std::env::temp_dir().join(format!("pb-duplicated-owner-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("Artwork.psd");
        fs::write(&path, b"source").unwrap();
        let alias = dir.join("Alias.psd");
        fs::hard_link(&path, &alias).unwrap();
        let owner = Ownership::open(&path, "original-project").unwrap();
        let descriptors = owner
            .files
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .iter()
            .map(|lock| lock._file.try_clone().unwrap())
            .collect::<Vec<_>>();
        assert!(Ownership::open(&alias, "other-project").is_err());
        drop(owner);
        // Keep duplicated handles alive across reacquisition, reproducing the
        // kernel lock lifetime of a concurrently forked process deterministically.
        let reopened = Ownership::open(&path, "next-project").unwrap();
        assert!(Ownership::open(&alias, "other-project").is_err());
        drop(descriptors);
        assert!(Ownership::open(&alias, "other-project").is_err());
        drop(reopened);
        assert!(Ownership::open(&alias, "other-project").is_ok());
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn staged_identity_is_owned_before_publication_and_release_reaches_snapshots() {
        let dir = std::env::temp_dir().join(format!("pb-staged-owner-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("Artwork.psd");
        fs::write(&path, b"old source").unwrap();
        let owner = Ownership::open(&path, "project").unwrap();
        let snapshot = owner.clone();
        let staged = dir.join("staged.tmp");
        fs::write(&staged, b"new source").unwrap();
        owner.prepare(&staged).unwrap();
        let alias = dir.join("alias.psd");
        fs::hard_link(&staged, &alias).unwrap();
        assert!(Ownership::open(&alias, "other").is_err());
        owner.release();
        assert!(Ownership::open(&alias, "other").is_ok());
        drop(snapshot);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn simultaneous_pending_claims_admit_exactly_one_owner() {
        let dir = std::env::temp_dir().join(format!("pb-pending-owner-{}", crate::engine::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("Artwork.psd");
        fs::write(&path, b"source").unwrap();
        let start = Arc::new(std::sync::Barrier::new(8));
        let finish = Arc::new(std::sync::Barrier::new(8));
        let mut threads = vec![];
        for n in 0..8 {
            let (path, start, finish) = (path.clone(), start.clone(), finish.clone());
            threads.push(std::thread::spawn(move || {
                start.wait();
                let owner = Ownership::open(&path, &format!("project-{n}"));
                finish.wait();
                owner.is_ok()
            }));
        }
        assert_eq!(
            threads
                .into_iter()
                .map(|t| t.join().unwrap())
                .filter(|ok| *ok)
                .count(),
            1
        );
        fs::remove_dir_all(dir).unwrap();
    }
}
