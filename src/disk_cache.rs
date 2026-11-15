//! Disposable derived images. Cache failure always falls back to source evaluation.
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
pub const BUDGET: usize = 512 * 1024 * 1024;
pub const ENTRY_BUDGET: usize = 256 * 1024 * 1024;
const MAGIC: &[u8; 8] = b"PBCACHE1";
const LEGACY_OWNER: &[u8] = b"PeerBrush disposable derived cache v1";
const OWNER: &[u8] = b"PeerBrush disposable derived cache v2";
fn digest(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
}
pub struct Store {
    root: PathBuf,
    budget: usize,
    bytes: usize,
    items: HashMap<String, (PathBuf, usize)>,
    order: VecDeque<String>,
    owner: Option<fs::File>,
}
impl Store {
    pub fn new(root: PathBuf, budget: usize) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        let mut owner = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join("owner"))?;
        owner.try_lock().map_err(std::io::Error::from)?;
        owner.write_all(OWNER)?;
        Ok(Self {
            root,
            budget: budget.min(BUDGET),
            bytes: 0,
            items: HashMap::new(),
            order: VecDeque::new(),
            owner: Some(owner),
        })
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    fn remove(&mut self, key: &str) {
        if let Some((p, n)) = self.items.remove(key) {
            let _ = fs::remove_file(p);
            self.bytes -= n;
        }
        self.order.retain(|k| k != key);
    }
    pub fn put(&mut self, key: &str, width: u32, height: u32, depth: u16, bytes: &[u8]) {
        if crate::raster::check_size(width, height).is_err() {
            return;
        }
        let expected = width as u64 * height as u64 * 4 * if depth == 16 { 2 } else { 1 };
        if ![8, 16].contains(&depth)
            || key.len() > 512
            || bytes.len() as u64 != expected
            || bytes.len() > ENTRY_BUDGET
        {
            return;
        }
        let size = bytes.len() + 42 + key.len();
        if size > self.budget {
            return;
        }
        self.remove(key);
        while self.bytes + size > self.budget {
            let Some(old) = self.order.front().cloned() else {
                break;
            };
            self.remove(&old);
        }
        let path = self.root.join(format!("{}.cache", crate::engine::id()));
        let temp = path.with_extension("partial");
        let result = (|| -> std::io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temp)?;
            file.write_all(MAGIC)?;
            file.write_all(&width.to_be_bytes())?;
            file.write_all(&height.to_be_bytes())?;
            file.write_all(&depth.to_be_bytes())?;
            file.write_all(&(key.len() as u64).to_be_bytes())?;
            file.write_all(&(bytes.len() as u64).to_be_bytes())?;
            file.write_all(&digest(bytes).to_be_bytes())?;
            file.write_all(key.as_bytes())?;
            for chunk in bytes.chunks(65536) {
                file.write_all(chunk)?;
            }
            drop(file);
            fs::rename(&temp, &path)
        })();
        if result.is_err() {
            let _ = fs::remove_file(temp);
            return;
        }
        self.bytes += size;
        self.items.insert(key.into(), (path, size));
        self.order.push_back(key.into());
    }
    pub fn get(&mut self, key: &str, width: u32, height: u32, depth: u16) -> Option<Vec<u8>> {
        if crate::raster::check_size(width, height).is_err()
            || ![8, 16].contains(&depth)
            || key.len() > 512
        {
            return None;
        }
        let (path, size) = self.items.get(key)?.clone();
        let result = (|| -> Option<Vec<u8>> {
            let mut file = fs::File::open(path).ok()?;
            if file.metadata().ok()?.len() != size as u64 {
                return None;
            }
            let mut header = [0; 42];
            file.read_exact(&mut header).ok()?;
            let key_len = u64::from_be_bytes(header[18..26].try_into().ok()?) as usize;
            let n = u64::from_be_bytes(header[26..34].try_into().ok()?) as usize;
            if &header[..8] != MAGIC
                || header[8..12] != width.to_be_bytes()
                || header[12..16] != height.to_be_bytes()
                || header[16..18] != depth.to_be_bytes()
                || key_len != key.len()
                || n > ENTRY_BUDGET
                || n as u64 != width as u64 * height as u64 * 4 * if depth == 16 { 2 } else { 1 }
                || n.checked_add(42 + key_len) != Some(size)
            {
                return None;
            }
            let mut saved_key = vec![0; key_len];
            file.read_exact(&mut saved_key).ok()?;
            if saved_key != key.as_bytes() {
                return None;
            }
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(n).ok()?;
            bytes.resize(n, 0);
            file.read_exact(&mut bytes).ok()?;
            if digest(&bytes) != u64::from_be_bytes(header[34..42].try_into().ok()?) {
                return None;
            }
            Some(bytes)
        })();
        if result.is_none() {
            self.remove(key);
        } else {
            self.order.retain(|k| k != key);
            self.order.push_back(key.into());
        }
        result
    }
}
impl Drop for Store {
    fn drop(&mut self) {
        for (p, _) in self.items.values() {
            let _ = fs::remove_file(p);
        }
        let _ = fs::remove_file(self.root.join("owner"));
        drop(self.owner.take());
        let _ = fs::remove_dir(&self.root);
    }
}
static DISK: OnceLock<Mutex<Option<Store>>> = OnceLock::new();
pub(crate) fn configure(state: &Path) -> PathBuf {
    // Unique instance directory prevents stale application-version results being reused.
    let current = state
        .join("derived-cache")
        .join(format!("instance-{}", crate::engine::id()));
    let store = Store::new(current.clone(), BUDGET).ok();
    *DISK.get_or_init(|| Mutex::new(None)).lock().unwrap() = store;
    current
}

fn ordinary_directory(path: &Path) -> bool {
    path.symlink_metadata().is_ok_and(|metadata| {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return false;
            }
        }
        metadata.is_dir()
    })
}

/// The instance owns this cleanup until it finishes or observes shutdown.
pub(crate) fn clean_abandoned(current: &Path, cancelled: impl Fn() -> bool) {
    let Some(root) = current.parent().filter(|path| ordinary_directory(path)) else {
        return;
    };
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    'instances: for entry in entries.flatten() {
        if cancelled() {
            return;
        }
        let path = entry.path();
        if path == current
            || !entry
                .file_name()
                .to_str()
                .and_then(|name| name.strip_prefix("instance-"))
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            || !ordinary_directory(&path)
        {
            continue;
        }
        let marker_path = path.join("owner");
        let Ok(owner) = crate::file_safety::open_native_read(&marker_path) else {
            continue;
        };
        let mut marker = Vec::new();
        if (&owner)
            .take(OWNER.len() as u64 + 1)
            .read_to_end(&mut marker)
            .is_err()
            || (marker != OWNER && marker != LEGACY_OWNER)
            || owner.try_lock().is_err()
        {
            continue;
        }
        let Ok(files) = fs::read_dir(&path) else {
            continue;
        };
        for file in files.flatten() {
            if cancelled() {
                return;
            }
            let name = file.file_name();
            let name = name.to_string_lossy();
            if (name.ends_with(".cache") || name.ends_with(".partial"))
                && file.file_type().is_ok_and(|kind| kind.is_file())
                && fs::remove_file(file.path())
                    .is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound)
            {
                // Keep the marker so a later session can retry a busy file.
                continue 'instances;
            }
        }
        if cancelled() {
            return;
        }
        let _ = fs::remove_file(marker_path);
        drop(owner);
        let _ = fs::remove_dir(path);
    }
}
pub(crate) fn spill(key: &str, w: u32, h: u32, depth: u16, bytes: &[u8]) {
    if let Some(store) = DISK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_mut()
    {
        store.put(key, w, h, depth, bytes);
    }
}
pub(crate) fn read(key: &str, w: u32, h: u32, depth: u16) -> Option<Vec<u8>> {
    DISK.get_or_init(|| Mutex::new(None))
        .lock()
        .ok()?
        .as_mut()?
        .get(key, w, h, depth)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_cleanup_preserves_live_and_foreign_files_and_honors_shutdown() {
        let root =
            std::env::temp_dir().join(format!("peerbrush-cache-test-{}", crate::engine::id()));
        let directory = || root.join(format!("instance-{}", crate::engine::id()));
        let current = directory();
        let active = directory();
        let abandoned = directory();
        let legacy = directory();
        let foreign = directory();
        let live = Store::new(active.clone(), BUDGET).unwrap();
        fs::write(active.join("render.cache"), b"live").unwrap();
        for (path, marker) in [
            (&current, OWNER),
            (&abandoned, OWNER),
            (&legacy, LEGACY_OWNER),
            (&foreign, b"foreign marker".as_slice()),
        ] {
            fs::create_dir(path).unwrap();
            fs::write(path.join("owner"), marker).unwrap();
            fs::write(path.join("render.cache"), b"fixture").unwrap();
        }
        fs::write(abandoned.join("original.psd"), b"artwork").unwrap();

        clean_abandoned(&current, || true);
        assert!(abandoned.join("render.cache").is_file());
        assert!(abandoned.join("owner").is_file());

        clean_abandoned(&current, || false);
        assert!(!legacy.exists());
        assert!(!abandoned.join("render.cache").exists());
        assert_eq!(
            fs::read(abandoned.join("original.psd")).unwrap(),
            b"artwork"
        );
        for path in [current, active, foreign] {
            assert!(path.join("render.cache").is_file());
        }
        drop(live);
        fs::remove_dir_all(root).unwrap();
    }
}
