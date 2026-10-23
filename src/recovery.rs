//! Complete immutable recovery versions. No source file is used as a destination.
use crate::{engine, psd, server, workspace};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};

pub const VERSIONS_PER_PROJECT: usize = 3;
pub const MAX_PROJECTS: usize = 64;
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FILE: u64 = 256 * 1024 * 1024;
pub type Versions = HashMap<String, (String, u64, Value)>;

#[cfg(test)]
thread_local! {
    static CLEANUP_OBSERVER: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub version: u32,
    pub snapshot: String,
    pub project_id: String,
    pub document_id: String,
    pub revision: u64,
    pub name: String,
    pub original_path: Option<PathBuf>,
    pub bit_depth: u16,
    pub timestamp_ms: u64,
    pub bytes: u64,
    pub checksum: String,
    pub file: String,
}
impl Snapshot {
    fn valid(&self) -> bool {
        self.version == 2
            && [self.snapshot.as_str(), &self.project_id, &self.document_id]
                .iter()
                .all(|id| uuid::Uuid::parse_str(id).is_ok())
            && self.file == format!("recovery-{}-{}.psd", self.project_id, self.snapshot)
            && matches!(self.bit_depth, 8 | 16)
            && self.bytes <= MAX_FILE
            && self.checksum.len() == 16
    }
    fn manifest(&self, dir: &Path) -> PathBuf {
        dir.join(&self.file).with_extension("json")
    }
}
fn gate(dir: &Path) -> Arc<Mutex<()>> {
    static GATES: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
    let mut gates = GATES.get_or_init(Default::default).lock().unwrap();
    gates.retain(|_, gate| gate.strong_count() > 0);
    if let Some(gate) = gates.get(dir).and_then(Weak::upgrade) {
        return gate;
    }
    let gate = Arc::new(Mutex::new(()));
    gates.insert(dir.to_owned(), Arc::downgrade(&gate));
    gate
}
// Corruption detection, not an authentication or security signature.
fn checksum(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ *byte as u64).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}
fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}
fn bounded_read(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    if !regular(path) {
        return Err("Recovery is missing or is not a regular local file".into());
    }
    let mut bytes = vec![];
    fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("Recovery exceeds its file limit".into());
    }
    Ok(bytes)
}
/// The immutable sidecar is the commit marker. A mutable index is not needed.
pub fn catalog(dir: &Path) -> Vec<Snapshot> {
    let mut entries = vec![];
    if let Ok(files) = fs::read_dir(dir) {
        for file in files.flatten() {
            let path = file.path();
            let name = file.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("recovery-") || !name.ends_with(".json") {
                continue;
            }
            let Ok(bytes) = bounded_read(&path, 16 * 1024) else {
                continue;
            };
            let Ok(entry) = serde_json::from_slice::<Snapshot>(&bytes) else {
                continue;
            };
            if entry.valid() && entry.manifest(dir) == path && regular(&dir.join(&entry.file)) {
                entries.push(entry);
            }
        }
    }
    entries.sort_by(|a, b| {
        b.timestamp_ms
            .cmp(&a.timestamp_ms)
            .then(b.revision.cmp(&a.revision))
            .then(a.snapshot.cmp(&b.snapshot))
    });
    entries
}
fn remove(dir: &Path, entry: &Snapshot) -> Result<(), String> {
    // Remove the marker first. A crash can leave an unlisted data file, never a
    // partly overwritten listed version. Restrict both paths to validated IDs.
    if !entry.valid() {
        return Err("Invalid recovery identity".into());
    }
    for path in [entry.manifest(dir), dir.join(&entry.file)] {
        match fs::remove_file(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(format!("Cannot discard recovery: {e}")),
        }
    }
    server::sync_parent(&dir.join(&entry.file))
}
pub fn discard(dir: &Path, snapshot: &str) -> Result<(), String> {
    let gate = gate(dir);
    let _guard = gate.lock().unwrap();
    let entry = catalog(dir)
        .into_iter()
        .find(|e| e.snapshot == snapshot)
        .ok_or("Recovery version no longer exists; refresh the list")?;
    remove(dir, &entry)
}
pub fn discard_in(
    workspace: &workspace::Registry,
    dir: &Path,
    snapshot: &str,
) -> Result<(), String> {
    let _operation = server::FileOperation::begin_workspace(workspace)?;
    discard(dir, snapshot)
}
fn discard_project(dir: &Path, project: &str) -> Result<(), String> {
    #[cfg(test)]
    CLEANUP_OBSERVER.with(|slot| {
        let observe = slot.borrow_mut().take();
        if let Some(observe) = observe {
            observe();
        }
    });
    for entry in catalog(dir).into_iter().filter(|e| e.project_id == project) {
        remove(dir, &entry)?;
    }
    Ok(())
}
fn cleanup_orphans(dir: &Path) -> Result<(), String> {
    let files = fs::read_dir(dir).map_err(|e| e.to_string())?;
    for file in files.flatten() {
        let name = file.file_name();
        let name = name.to_string_lossy();
        let Some(stem) = name
            .strip_prefix("recovery-")
            .and_then(|s| s.split_once('.').map(|(stem, _)| stem))
        else {
            continue;
        };
        if !stem.is_ascii()
            || stem.len() != 73
            || stem.as_bytes()[36] != b'-'
            || uuid::Uuid::parse_str(&stem[..36]).is_err()
            || uuid::Uuid::parse_str(&stem[37..]).is_err()
        {
            continue;
        }
        let path = file.path();
        if !regular(&path) {
            continue;
        }
        let temporary = name
            .strip_suffix(".tmp")
            .and_then(|s| s.rsplit_once('.'))
            .is_some_and(|(_, id)| uuid::Uuid::parse_str(id).is_ok());
        let orphan = name.ends_with(".psd") && !regular(&path.with_extension("json"));
        if temporary || orphan {
            fs::remove_file(path).map_err(|e| format!("Cannot clean incomplete recovery: {e}"))?;
        }
    }
    Ok(())
}
fn persist(
    dir: &Path,
    project: &str,
    doc: &engine::Document,
    path: Option<PathBuf>,
    bytes: &[u8],
) -> Result<Snapshot, String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err("Autosave exceeds the 256 MiB encoded-file limit".into());
    }
    let entries = catalog(dir);
    // Retry failed pruning before allocating another version, bounding failed writes.
    for old in entries
        .iter()
        .filter(|e| e.project_id == project)
        .skip(VERSIONS_PER_PROJECT)
    {
        remove(dir, old)?;
    }
    let entries = catalog(dir);
    if let Some(entry) = entries.iter().find(|e| {
        e.project_id == project
            && e.document_id == doc.id
            && e.revision == doc.revision
            && e.checksum == checksum(bytes)
    }) {
        return Ok(entry.clone());
    }
    let retained: Vec<_> = entries
        .iter()
        .filter(|e| e.project_id == project)
        .take(VERSIONS_PER_PROJECT - 1)
        .collect();
    let other: Vec<_> = entries.iter().filter(|e| e.project_id != project).collect();
    let projects: HashSet<_> = other.iter().map(|e| &e.project_id).collect();
    let legacy = legacy(dir);
    let legacy_bytes = legacy
        .iter()
        .filter_map(|(_, p)| fs::metadata(p).ok().map(|m| m.len()))
        .sum::<u64>();
    if projects.len() + legacy.len() >= MAX_PROJECTS
        || other
            .iter()
            .chain(retained.iter())
            .map(|e| e.bytes)
            .sum::<u64>()
            + legacy_bytes
            + bytes.len() as u64
            > MAX_BYTES
    {
        return Err("Recovery storage is full (1 GiB / 64 projects); save work or explicitly discard old recoveries".into());
    }
    let snapshot = engine::id();
    let entry = Snapshot {
        version: 2,
        file: format!("recovery-{project}-{snapshot}.psd"),
        snapshot,
        project_id: project.into(),
        document_id: doc.id.clone(),
        revision: doc.revision,
        name: doc.name.clone(),
        original_path: path,
        bit_depth: doc.bit_depth,
        timestamp_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64,
        bytes: bytes.len() as u64,
        checksum: checksum(bytes),
    };
    let marker = serde_json::to_vec(&entry).map_err(|e| e.to_string())?;
    if marker.len() > 16 * 1024 {
        return Err("Recovery metadata exceeds its limit".into());
    }
    server::atomic_write(&dir.join(&entry.file), bytes)?;
    if let Err(error) = server::atomic_write(&entry.manifest(dir), &marker) {
        let _ = fs::remove_file(dir.join(&entry.file));
        return Err(error);
    }
    // Old complete versions are pruned only after the new commit marker is durable.
    for old in entries
        .iter()
        .filter(|e| e.project_id == project)
        .skip(VERSIONS_PER_PROJECT - 1)
    {
        remove(dir, old)?;
    }
    Ok(entry)
}
/// Encoding runs outside the engine lock. Newer work stays dirty and is picked up
/// by the next tick; a checkpoint never changes saved_revision or original files.
pub fn checkpoint(workspace: &workspace::Registry, dir: &Path, previous: &mut Versions) {
    let _operation = {
        let state = workspace.lock().unwrap();
        if state.exiting {
            return;
        }
        server::FileOperation::track(state.file_operations.clone())
    };
    let gate = gate(dir);
    let _guard = gate.lock().unwrap();
    let mut failures = workspace.lock().unwrap().recovery_failures.clone();
    if let Err(error) = cleanup_orphans(dir) {
        let mut state = workspace.lock().unwrap();
        state.recovery_dir = Some(dir.to_owned());
        state.recovery_error = Some(format!("Autosave failed · {error}"));
        state.recovery_failures.insert("@storage".into(), error);
        return;
    }
    failures.remove("@storage");
    let retired = workspace.lock().unwrap().recovery_retired.clone();
    for project in retired {
        match discard_project(dir, &project) {
            Ok(()) => {
                workspace
                    .lock()
                    .unwrap()
                    .recovery_retired
                    .retain(|id| id != &project);
                failures.remove(&project);
            }
            Err(error) => {
                failures.insert(project, error);
            }
        }
    }
    let entries = workspace::entries_in(workspace);
    let live: HashSet<_> = entries.iter().map(|(id, _, _)| id.clone()).collect();
    previous.retain(|id, _| live.contains(id));
    for (project, _, shared) in entries {
        let (doc, path) = match shared.try_lock() {
            Ok(e) if e.closed || e.doc.read_only || e.doc.revision == e.saved_revision => {
                drop(e);
                previous.remove(&project);
                if let Err(error) = discard_project(dir, &project) {
                    failures.insert(project, error);
                } else {
                    failures.remove(&project);
                }
                continue;
            }
            Ok(e) => {
                if previous
                    .get(&project)
                    .is_some_and(|(id, rev, _)| id == &e.doc.id && *rev == e.doc.revision)
                {
                    continue;
                }
                (e.doc.clone(), e.path.clone())
            }
            Err(_) => continue,
        };
        let result = psd::encode(&doc).and_then(|bytes| {
            // Replacement/closing during encoding must not create a new recovery.
            if shared.try_lock().is_ok_and(|e| {
                e.closed
                    || e.doc.id != doc.id
                    || e.doc.revision != doc.revision
                    || e.saved_revision == e.doc.revision
            }) {
                return Ok(None);
            }
            persist(dir, &project, &doc, path, &bytes).map(Some)
        });
        match result {
            Ok(Some(entry)) => {
                failures.remove(&project);
                previous.insert(project, (doc.id, doc.revision, json!(entry)));
            }
            Ok(None) => {
                failures.remove(&project);
            }
            Err(error) => {
                failures.insert(project, format!("{}: {error}", doc.name));
            }
        }
    }
    let mut state = workspace.lock().unwrap();
    state.recovery_dir = Some(dir.to_owned());
    state.recovery_error = (!failures.is_empty()).then(|| {
        format!(
            "Autosave failed · {}",
            failures.values().cloned().collect::<Vec<_>>().join(" · ")
        )
    });
    state.recovery_failures = failures;
}
pub fn restore(
    workspace: &workspace::Registry,
    dir: &Path,
    snapshot: &str,
    control: &crate::loading::Control,
    initiating: Option<&str>,
) -> Result<server::Shared, String> {
    let entry = catalog(dir)
        .into_iter()
        .find(|e| e.snapshot == snapshot)
        .ok_or("Recovery version no longer exists; refresh the list")?;
    control.begin()?;
    let bytes = bounded_read(&dir.join(&entry.file), MAX_FILE)?;
    if bytes.len() as u64 != entry.bytes || checksum(&bytes) != entry.checksum {
        return Err("Recovery version is incomplete or damaged; choose an earlier version".into());
    }
    let mut doc = psd::decode_slice(&bytes, control)?;
    control.check()?;
    if doc.read_only || doc.bit_depth != entry.bit_depth {
        return Err(
            "Recovery cannot be restored as editable native-depth work; its file is retained"
                .into(),
        );
    }
    doc.name = entry.name;
    let mut e = engine::Engine::new();
    e.doc = doc;
    e.saved_revision = u64::MAX;
    e.status = "Recovered into an unsaved tab · choose Save As · recovery retained".into();
    // Engine::new has no destination or external file version. Register assigns a
    // fresh runtime document identity; recovery files remain until explicit discard.
    workspace::register_in(workspace, e, initiating)
}

/// Compatibility discovery for older single-file recovery indexes.
pub fn legacy(dir: &Path) -> Vec<(String, PathBuf)> {
    let mut entries = vec![];
    let mut seen = HashSet::new();
    for file in ["recoveries.json", "previous-recoveries.json"] {
        let Ok(bytes) = bounded_read(&dir.join(file), 65536) else {
            continue;
        };
        let Ok(index) = serde_json::from_slice::<Value>(&bytes) else {
            continue;
        };
        for entry in index["projects"]
            .as_array()
            .into_iter()
            .flatten()
            .take(workspace::MAX_PROJECTS)
        {
            let Some(project) = entry["project_id"].as_str() else {
                continue;
            };
            let filename = format!("recovery-{project}.psd");
            if uuid::Uuid::parse_str(project).is_err()
                || entry["file"] != filename
                || !seen.insert(project.to_owned())
            {
                continue;
            }
            let path = dir.join(filename);
            if regular(&path) {
                entries.push((
                    entry["name"].as_str().unwrap_or("Recovered project").into(),
                    path,
                ));
            }
        }
    }
    if regular(&dir.join("recovery.psd")) {
        entries.push(("Last autosave (legacy)".into(), dir.join("recovery.psd")));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn source(engine: &engine::Engine) -> Value {
        let history = |entries: &[engine::History]| {
            entries.iter().map(|entry| json!({
                "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
                "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted,
            })).collect::<Vec<_>>()
        };
        json!({"document":engine.doc,"png":engine.doc.export_png().unwrap(),
            "undo":history(&engine.undo),"redo":history(&engine.redo),
            "saved_revision":engine.saved_revision,"path":engine.path,"closed":engine.closed})
    }

    #[test]
    fn clean_cleanup_releases_engine_but_retains_workspace_guard_and_later_native_work() {
        for depth in [8, 16] {
            let dir = std::env::temp_dir().join(format!("pb-recovery-lock-{}", engine::id()));
            fs::create_dir(&dir).unwrap();
            let mut engine = engine::Engine::new();
            engine.doc = engine::Document::new_depth(12, 8, depth).unwrap();
            engine.doc.layers[0]
                .pixels
                .set16(0, 0, [12347, 33559, 51237, 65535]);
            let layer = engine.doc.layers[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"fill","layer":layer,
                "rect":[1,1,2,2],"color":[17,29,43,255]})],
                    None,
                    None,
                    "Earlier human work",
                )
                .unwrap();
            let shared = Arc::new(Mutex::new(engine));
            let registry = workspace::attach(&shared);
            let mut previous = Versions::new();
            checkpoint(&registry, &dir, &mut previous);
            let old = catalog(&dir).pop().expect("Real dirty recovery");
            assert_eq!(catalog(&dir).len(), 1);
            let original = dir.join("Original.psd");
            server::save(&shared, &original).unwrap();
            let original_bytes = fs::read(&original).unwrap();
            let before = source(&shared.lock().unwrap());
            let reviewed = workspace::lifecycle::update_readiness(&registry).unwrap();
            let observed = Arc::new(AtomicBool::new(false));
            let expected = Arc::new(Mutex::new(None));
            let observed_shared = shared.clone();
            let observed_registry = registry.clone();
            let observed_dir = dir.clone();
            let observed_old = old.clone();
            let observed_flag = observed.clone();
            let observed_expected = expected.clone();
            CLEANUP_OBSERVER.with(|slot| {
                assert!(slot.borrow().is_none());
                *slot.borrow_mut() = Some(Box::new(move || {
                    let engine = observed_shared.try_lock().unwrap_or_else(|_| {
                        panic!("Recovery storage cleanup must not hold the document engine")
                    });
                    assert_eq!(engine.doc.revision, engine.saved_revision);
                    assert!(!engine.closed);
                    drop(engine);
                    assert!(catalog(&observed_dir)
                        .iter()
                        .any(|entry| entry.snapshot == observed_old.snapshot));
                    assert!(observed_dir.join(&observed_old.file).is_file());
                    let storage_gate = gate(&observed_dir);
                    assert!(
                        storage_gate.try_lock().is_err(),
                        "Cleanup keeps the existing storage gate"
                    );
                    assert!(
                        observed_registry
                            .lock()
                            .unwrap()
                            .file_operations
                            .load(Ordering::Acquire)
                            > 0
                    );
                    let error =
                        workspace::lifecycle::finish_update_exit(&observed_registry, &reviewed)
                            .unwrap_err();
                    assert!(error.contains("workspace/recovery"), "{error}");
                    assert!(!observed_registry.lock().unwrap().exiting);
                    let mut engine = observed_shared.lock().unwrap();
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"fill","layer":layer,
                        "rect":[2,1,4,3],"color":[53,97,173,255]})],
                            None,
                            None,
                            "Human work during cleanup",
                        )
                        .unwrap();
                    *observed_expected.lock().unwrap() = Some(source(&engine));
                    observed_flag.store(true, Ordering::Release);
                }));
            });
            checkpoint(&registry, &dir, &mut previous);
            assert!(observed.load(Ordering::Acquire));
            CLEANUP_OBSERVER.with(|slot| assert!(slot.borrow().is_none()));
            assert!(catalog(&dir).is_empty());
            assert!(!dir.join(&old.file).exists());
            assert!(!old.manifest(&dir).exists());
            assert_eq!(
                registry
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire),
                0
            );
            assert!(!registry.lock().unwrap().exiting);
            let expected = expected.lock().unwrap().take().unwrap();
            {
                let engine = shared.lock().unwrap();
                assert_eq!(source(&engine), expected);
                assert_eq!(engine.doc.revision, old.revision + 1);
                assert_eq!(engine.doc.bit_depth, depth);
                assert_eq!(
                    engine.doc.layers[0].pixels.get16(2, 1),
                    [53 * 257, 97 * 257, 173 * 257, 65535]
                );
                assert_eq!(
                    engine.undo.len(),
                    before["undo"].as_array().unwrap().len() + 1
                );
                assert_eq!(expected["undo"][0], before["undo"][0]);
                assert!(engine.redo.is_empty());
                assert!(!engine.closed);
            }
            checkpoint(&registry, &dir, &mut previous);
            let entries = catalog(&dir);
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].revision, old.revision + 1);
            assert_eq!(entries[0].bit_depth, depth);
            let recovered = psd::decode(&fs::read(dir.join(&entries[0].file)).unwrap()).unwrap();
            let engine = shared.lock().unwrap();
            assert_eq!(source(&engine), expected);
            assert_eq!(recovered.bit_depth, depth);
            assert!(!recovered.read_only);
            assert_eq!(
                recovered.layers[0].pixels.rgba16(),
                engine.doc.layers[0].pixels.rgba16()
            );
            assert_eq!(
                recovered.export_png().unwrap(),
                engine.doc.export_png().unwrap()
            );
            assert_eq!(
                recovered.layers[0].pixels.get16(0, 0),
                engine.doc.layers[0].pixels.get16(0, 0)
            );
            if depth == 16 {
                assert_eq!(
                    recovered.layers[0].pixels.get16(0, 0),
                    [12347, 33559, 51237, 65535]
                );
            }
            drop(engine);
            assert_eq!(fs::read(&original).unwrap(), original_bytes);
            assert_eq!(
                registry
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire),
                0
            );
            assert!(registry.lock().unwrap().recovery_error.is_none());
            drop(shared);
            drop(registry);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
