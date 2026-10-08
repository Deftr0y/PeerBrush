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
}
impl Store {
    pub fn new(root: PathBuf, budget: usize) -> std::io::Result<Self> {
        fs::create_dir_all(&root)?;
        fs::write(root.join("owner"), b"PeerBrush disposable derived cache v1")?;
        Ok(Self {
            root,
            budget: budget.min(BUDGET),
            bytes: 0,
            items: HashMap::new(),
            order: VecDeque::new(),
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
        let _ = fs::remove_dir(&self.root);
    }
}
static DISK: OnceLock<Mutex<Option<Store>>> = OnceLock::new();
pub fn configure(state: &Path) {
    // Unique instance directory prevents stale application-version results being reused.
    // Clean abandoned cache directories only while the workspace instance lock is held.
    let root = state.join("derived-cache");
    if let Ok(entries) = fs::read_dir(&root) {
        for entry in entries.flatten() {
            let p = entry.path();
            if entry.file_name().to_string_lossy().starts_with("instance-")
                && entry
                    .file_type()
                    .is_ok_and(|t| t.is_dir() && !t.is_symlink())
            {
                if fs::read(p.join("owner"))
                    .is_ok_and(|b| b == b"PeerBrush disposable derived cache v1")
                {
                    if let Ok(files) = fs::read_dir(&p) {
                        for file in files.flatten() {
                            let name = file.file_name();
                            let name = name.to_string_lossy();
                            if file
                                .file_type()
                                .is_ok_and(|t| t.is_file() && !t.is_symlink())
                                && (name == "owner"
                                    || name.ends_with(".cache")
                                    || name.ends_with(".partial"))
                            {
                                let _ = fs::remove_file(file.path());
                            }
                        }
                    }
                    let _ = fs::remove_dir(p);
                }
            }
        }
    }
    let store = Store::new(
        root.join(format!("instance-{}", crate::engine::id())),
        BUDGET,
    )
    .ok();
    *DISK.get_or_init(|| Mutex::new(None)).lock().unwrap() = store;
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
