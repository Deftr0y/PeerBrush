//! Complete immutable recovery versions. No source file is used as a destination.
pub(crate) mod backed;
mod native;
mod worker;
use crate::{engine, psd, server, workspace};
pub(crate) use native::{Catalog as NativeCatalog, DISCARD_AVAILABLE as NATIVE_DISCARD_AVAILABLE};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Seek},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Arc, Mutex, MutexGuard, OnceLock, Weak},
    time::{SystemTime, UNIX_EPOCH},
};
pub(crate) use worker::Worker;

pub(crate) struct Available {
    pub versions: Vec<Snapshot>,
    pub unpaired: Vec<Unpaired>,
    pub resident_error: Option<&'static str>,
    pub legacy: Vec<(String, PathBuf)>,
    pub native: NativeCatalog,
}
/// Off-lock discovery. The returned native rows retain their own memory admission;
/// rows are display metadata, never restoration or source-retirement permission.
pub(crate) fn catalog_in(
    workspace: &workspace::Registry,
    dir: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Available, String> {
    let _operation = server::FileOperation::begin_workspace(workspace)?;
    check()?;
    let resident = scan_resident(dir, check)?;
    let legacy = legacy(dir);
    let native = match native::catalog(workspace, check) {
        Ok(catalog) => catalog,
        Err(_) => {
            check()?;
            NativeCatalog::unavailable()
        }
    };
    check()?;
    Ok(Available {
        versions: resident.entries,
        unpaired: resident.unpaired,
        resident_error: resident.incomplete.then_some(RESIDENT_CATALOG_ERROR),
        legacy,
        native,
    })
}

pub const VERSIONS_PER_PROJECT: usize = 3;
pub const MAX_PROJECTS: usize = 64;
pub const MAX_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_FILE: u64 = 256 * 1024 * 1024;
const RESIDENT_CATALOG_ERROR: &str =
    "Some recovery records are unreadable or incomplete. Existing resident recovery files are retained. Save current work until the inventory is repaired.";
#[derive(Default)]
pub struct Versions {
    resident: HashMap<String, (String, u64, Value)>,
    native: HashMap<String, native::Saved>,
}
impl Versions {
    pub fn new() -> Self {
        Self::default()
    }
}

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

#[derive(Serialize)]
pub(crate) struct Unpaired {
    pub snapshot: String,
    pub timestamp_ms: u64,
    pub bytes: u64,
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
const CHECKSUM_START: u64 = 0xcbf29ce484222325;
fn checksum_chunk(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ *byte as u64).wrapping_mul(0x100000001b3)
    })
}
fn checksum(bytes: &[u8]) -> String {
    format!("{:016x}", checksum_chunk(CHECKSUM_START, bytes))
}
fn regular(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}
fn bounded_read(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let mut bytes = vec![];
    crate::file_safety::open_native_read(path)
        .map_err(|e| e.to_string())?
        .take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > max {
        return Err("Recovery exceeds its file limit".into());
    }
    Ok(bytes)
}

pub(crate) fn preserve_previous_index(dir: &Path) -> Result<(), String> {
    let Ok(bytes) = bounded_read(&dir.join("recoveries.json"), 65535) else {
        return Ok(());
    };
    if serde_json::from_slice::<Value>(&bytes).is_ok_and(|index|
        index["projects"].as_array().is_some_and(|projects| !projects.is_empty()))
    {
        server::atomic_write(&dir.join("previous-recoveries.json"), &bytes)?;
    }
    Ok(())
}

/// The immutable sidecar is the commit marker. A mutable index is not needed.
pub fn catalog(dir: &Path) -> Vec<Snapshot> {
    scan_resident(dir, &|| Ok(())).map_or_else(|_| Vec::new(), |catalog| catalog.entries)
}

#[derive(Default)]
struct ResidentCatalog {
    entries: Vec<Snapshot>,
    unpaired: Vec<Unpaired>,
    incomplete: bool,
}

fn scan_resident(dir: &Path, check: &dyn Fn() -> Result<(), String>) -> Result<ResidentCatalog, String> {
    let mut catalog = ResidentCatalog::default();
    let files = match fs::read_dir(dir) {
        Ok(files) => files,
        Err(error) => {
            catalog.incomplete = error.kind() != std::io::ErrorKind::NotFound;
            return Ok(catalog);
        }
    };
    for file in files {
        check()?;
        let Ok(file) = file else {
            catalog.incomplete = true;
            continue;
        };
        let name = file.file_name();
        let name = name.to_string_lossy();
        if recovery_extension(&name) == Some("psd") {
            // An unpaired final PSD may be usable after interrupted publication.
            let path = file.path();
            if let Err(error) = fs::symlink_metadata(path.with_extension("json")) {
                catalog.incomplete = true;
                if error.kind() == std::io::ErrorKind::NotFound {
                    if let Ok(held) = crate::file_safety::open_native_read(&path) {
                        if let Ok(entry) = unpaired_entry(&name, &held) {
                            catalog.unpaired.push(entry);
                        }
                    }
                }
            }
            continue;
        }
        if !name.starts_with("recovery-") || !name.ends_with(".json") {
            continue;
        }
        let path = file.path();
        let entry = bounded_read(&path, 16 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Snapshot>(&bytes).ok())
            .filter(|entry| entry.valid() && entry.manifest(dir) == path);
        let Some(entry) = entry else {
            catalog.incomplete |= recovery_extension(&name) == Some("json");
            continue;
        };
        let data = crate::file_safety::open_native_read(&dir.join(&entry.file))
            .and_then(|file| file.metadata());
        if let Ok(data) = data {
            catalog.incomplete |= data.len() != entry.bytes;
            catalog.entries.push(entry);
        } else {
            catalog.incomplete = true;
        }
    }
    catalog.entries.sort_by(|a, b| {
        b.timestamp_ms
            .cmp(&a.timestamp_ms)
            .then(b.revision.cmp(&a.revision))
            .then(a.snapshot.cmp(&b.snapshot))
    });
    catalog.unpaired.sort_unstable_by(|a, b| {
        b.timestamp_ms.cmp(&a.timestamp_ms).then(a.snapshot.cmp(&b.snapshot))
    });
    Ok(catalog)
}

fn complete_resident_catalog(dir: &Path) -> Result<Vec<Snapshot>, String> {
    let catalog = scan_resident(dir, &|| Ok(()))?;
    if catalog.incomplete {
        return Err(RESIDENT_CATALOG_ERROR.into());
    }
    Ok(catalog.entries)
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
    if snapshot.starts_with("native:") {
        return native::discard(workspace, snapshot);
    }
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
    for entry in complete_resident_catalog(dir)?.into_iter().filter(|e| e.project_id == project) {
        remove(dir, &entry)?;
    }
    Ok(())
}
fn recovery_extension(name: &str) -> Option<&str> {
    recovery_name(name).map(|(_, _, extension)| extension)
}

fn recovery_name(name: &str) -> Option<(&str, &str, &str)> {
    let (stem, extension) = name.strip_prefix("recovery-")?.split_once('.')?;
    (stem.is_ascii()
        && stem.len() == 73
        && stem.as_bytes()[36] == b'-'
        && uuid::Uuid::parse_str(&stem[..36]).is_ok()
        && uuid::Uuid::parse_str(&stem[37..]).is_ok())
    .then(|| (&stem[..36], &stem[37..], extension))
}

fn cleanup_orphans(dir: &Path) -> Result<(), String> {
    let files = fs::read_dir(dir).map_err(|e| e.to_string())?;
    for file in files.flatten() {
        let name = file.file_name();
        let name = name.to_string_lossy();
        let Some(extension) = recovery_extension(&name) else {
            continue;
        };
        let path = file.path();
        if !regular(&path) {
            continue;
        }
        let temporary = extension
            .strip_suffix(".tmp")
            .is_some_and(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok());
        if temporary {
            fs::remove_file(path).map_err(|e| format!("Cannot clean incomplete recovery: {e}"))?;
        }
    }
    Ok(())
}
fn verify_checkpoint_data(dir: &Path, entry: &Snapshot) -> std::io::Result<bool> {
    let file = crate::file_safety::open_native_read(&dir.join(&entry.file))?;
    if file.metadata()?.len() != entry.bytes {
        return Ok(false);
    }
    let mut file = file.take(entry.bytes + 1);
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    let mut hash = CHECKSUM_START;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        hash = checksum_chunk(hash, &buffer[..read]);
    }
    Ok(bytes == entry.bytes && format!("{hash:016x}") == entry.checksum)
}

fn prune_versions<'a>(
    dir: &Path,
    versions: impl Iterator<Item = &'a Snapshot> + Clone,
) -> Result<bool, String> {
    let mut older = versions.clone().skip(VERSIONS_PER_PROJECT).peekable();
    if older.peek().is_none() {
        return Ok(false);
    }
    for retained in versions.take(VERSIONS_PER_PROJECT) {
        if !verify_checkpoint_data(dir, retained).unwrap_or(false) {
            return Err(format!(
                "Recovery version {} is damaged or unreadable; earlier versions retained",
                retained.snapshot
            ));
        }
    }
    for old in older {
        remove(dir, old)?;
    }
    Ok(true)
}

fn persist<T>(
    dir: &Path,
    project: &str,
    doc: &engine::Document,
    path: Option<PathBuf>,
    bytes: &[u8],
    validate: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<Snapshot, String> {
    if bytes.len() as u64 > MAX_FILE {
        return Err("Autosave exceeds the 256 MiB encoded-file limit".into());
    }
    let mut entries = complete_resident_catalog(dir)?;
    // Retry failed pruning before allocating another version, bounding failed writes.
    if prune_versions(dir, entries.iter().filter(|e| e.project_id == project))? {
        entries = complete_resident_catalog(dir)?;
    }
    let checksum = checksum(bytes);
    if let Some(entry) = entries.iter().find(|e| {
        e.project_id == project
            && e.document_id == doc.id
            && e.revision == doc.revision
            && e.checksum == checksum
            && verify_checkpoint_data(dir, e).unwrap_or(false)
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
        checksum,
    };
    let marker = marker_bytes(&entry)?;
    server::atomic_create(&dir.join(&entry.file), bytes, validate)?;
    // A failed or uncertain marker publication must leave the complete PSD available.
    server::atomic_create(&entry.manifest(dir), &marker, |_| Ok(()))?;
    // Give the new checkpoint a retention slot even if the clock moved back.
    prune_versions(
        dir,
        std::iter::once(&entry).chain(entries.iter().filter(|e| e.project_id == project)),
    )?;
    Ok(entry)
}

fn marker_bytes(entry: &Snapshot) -> Result<Vec<u8>, String> {
    if !entry.valid() {
        return Err("Invalid recovery metadata".into());
    }
    let marker = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
    if marker.len() > 16 * 1024 {
        return Err("Recovery metadata exceeds its limit".into());
    }
    Ok(marker)
}

fn resident_checkpoint_guard<'a>(
    shared: &'a server::Shared,
    doc: &engine::Document,
) -> Result<Option<MutexGuard<'a, engine::Engine>>, String> {
    let current = shared
        .try_lock()
        .map_err(|_| "Recovery source is busy; checkpoint deferred")?;
    let view = engine::view::current(&current);
    let unchanged = !current.closed
        && !view.read_only()
        && engine::view::DocumentView::Resident(doc).same_observation(view)
        && current.saved_revision != view.revision();
    Ok(unchanged.then_some(current))
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
        let cleanup = previous
            .native
            .get(&project)
            .map_or(Ok(()), |saved| {
                native::discard_project(workspace, &project, saved)
            })
            .and_then(|_| discard_project(dir, &project));
        match cleanup {
            Ok(()) => {
                previous.native.remove(&project);
                workspace
                    .lock()
                    .unwrap()
                    .recovery_retired
                    .retain(|id| id != &project);
                failures.remove(&project);
            }
            Err(error) if error == native::FOREGROUND_ACTIVE => {}
            Err(error) => {
                failures.insert(project, error);
            }
        }
    }
    let entries = workspace::entries_in(workspace);
    let live: HashSet<_> = entries.iter().map(|(id, _, _)| id.clone()).collect();
    previous.resident.retain(|id, _| live.contains(id));
    let retired = workspace.lock().unwrap().recovery_retired.clone();
    previous
        .native
        .retain(|id, _| live.contains(id) || retired.contains(id));
    enum Source {
        Resident(Arc<engine::Document>, Option<PathBuf>),
        Native(engine::scene::SceneSnapshot),
        NativeCurrent,
    }
    for (project, _, shared) in entries {
        let source = match shared.try_lock() {
            Ok(e) => {
                let view = engine::view::current(&e);
                if view.native().is_some() && e.file_operations.load(Ordering::Acquire) != 0 {
                    continue;
                }
                if e.closed || view.read_only() || view.revision() == e.saved_revision {
                    drop(e);
                    previous.resident.remove(&project);
                    let cleanup = previous
                        .native
                        .get(&project)
                        .map_or(Ok(()), |saved| {
                            native::discard_project(workspace, &project, saved)
                        })
                        .and_then(|_| discard_project(dir, &project));
                    match cleanup {
                        Ok(()) => {
                            previous.native.remove(&project);
                            failures.remove(&project);
                        }
                        Err(error) if error == native::FOREGROUND_ACTIVE => {}
                        Err(error) => {
                            failures.insert(project, error);
                        }
                    }
                    continue;
                }
                match e.doc_snapshot() {
                    engine::view::DocumentSnapshot::Resident(doc) => {
                        if previous
                            .resident
                            .get(&project)
                            .is_some_and(|(id, revision, _)| {
                                id == view.id() && *revision == view.revision()
                            })
                        {
                            continue;
                        }
                        Ok(Source::Resident(doc, e.path.clone()))
                    }
                    engine::view::DocumentSnapshot::Native(document) => {
                        if previous.native.get(&project).is_some_and(|saved| {
                            saved.key == document.key() && !saved.prune_pending
                        }) {
                            Ok(Source::NativeCurrent)
                        } else {
                            e.capture_scene(None)
                                .and_then(|scene| scene.into_native())
                                .map(Source::Native)
                        }
                    }
                }
            }
            Err(_) => continue,
        };
        let (doc, path) = match source {
            Ok(Source::NativeCurrent) => {
                if let Some(saved) = previous.native.get(&project) {
                    match saved.check_runtime(workspace) {
                        Ok(()) => {
                            failures.remove(&project);
                        }
                        Err(error) => {
                            failures.insert(project, error);
                        }
                    }
                }
                continue;
            }
            Ok(Source::Resident(doc, path)) => (doc, path),
            Ok(Source::Native(scene)) => {
                if (!previous.native.contains_key(&project)
                    && previous.native.len() >= MAX_PROJECTS)
                    || previous.native.try_reserve(1).is_err()
                {
                    failures.insert(
                        project,
                        "Native recovery worker admission is full; existing recoveries retained"
                            .into(),
                    );
                    continue;
                }
                let result = native::checkpoint(
                    workspace,
                    dir,
                    &shared,
                    &scene,
                    previous.native.get(&project),
                    &|| Ok(()),
                );
                match result {
                    Ok(outcome) => {
                        if let Some(error) = outcome.error {
                            failures.insert(project.clone(), error);
                        } else {
                            failures.remove(&project);
                        }
                        previous.resident.remove(&project);
                        previous.native.insert(project, outcome.saved);
                    }
                    Err(error) if error == native::FOREGROUND_ACTIVE => {}
                    Err(error) => {
                        failures.insert(project, error);
                    }
                }
                continue;
            }
            Err(error) => {
                failures.insert(project, error);
                continue;
            }
        };
        let result = psd::encode(&doc).and_then(|bytes| {
            if resident_checkpoint_guard(&shared, &doc)?.is_none() {
                return Ok(None);
            }
            native::admit_resident(workspace, dir, &project, bytes.len() as u64)?;
            // Hold the observed document through the PSD move. Once the data is
            // visible, finish its record even if a later edit advances the source.
            persist(dir, &project, &doc, path, &bytes, |_| {
                resident_checkpoint_guard(&shared, &doc)?
                    .ok_or_else(|| "Recovery source changed or was saved; checkpoint deferred".into())
            })
            .map(Some)
        });
        match result {
            Ok(Some(entry)) => {
                failures.remove(&project);
                previous
                    .resident
                    .insert(project, (doc.id.clone(), doc.revision, json!(entry)));
            }
            Ok(None) => {
                failures.remove(&project);
            }
            Err(error) if error == native::FOREGROUND_ACTIVE => {}
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
    if snapshot.starts_with("unpaired:") {
        let _operation = server::FileOperation::begin_workspace(workspace)?;
        let checked = check_unpaired(dir, snapshot, control)?;
        return register_recovered(workspace, checked.doc, initiating);
    }
    let entry = catalog(dir).into_iter().find(|e| e.snapshot == snapshot);
    let Some(entry) = entry else {
        return native::restore(workspace, snapshot, control, initiating);
    };
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
    register_recovered(workspace, doc, initiating)
}

fn unpaired_entry(name: &str, file: &fs::File) -> Result<Unpaired, String> {
    let (bytes, modified) = crate::file_safety::stamp(file)?;
    if !(26..=MAX_FILE).contains(&bytes) {
        return Err("Unpaired recovery is outside the resident PSD file limit".into());
    }
    let (volume, file_id) = crate::file_safety::identity(file)?;
    let mut fingerprint = ring::digest::Context::new(&ring::digest::SHA256);
    fingerprint.update(&volume.to_be_bytes());
    fingerprint.update(&file_id.to_be_bytes());
    fingerprint.update(&bytes.to_be_bytes());
    fingerprint.update(&modified.to_be_bytes());
    let digest = fingerprint.finish();
    let token = uuid::Uuid::from_bytes(digest.as_ref()[..16].try_into().unwrap());
    Ok(Unpaired {
        snapshot: format!("unpaired:{name}:{token}"),
        timestamp_ms: (modified / 1_000_000).min(u64::MAX as u128) as u64,
        bytes,
    })
}

struct CheckedUnpaired<'a> {
    name: &'a str,
    path: PathBuf,
    file: fs::File,
    version: crate::file_safety::Version,
    timestamp_ms: u64,
    doc: engine::Document,
}

fn check_unpaired<'a>(
    dir: &Path,
    snapshot: &'a str,
    control: &crate::loading::Control,
) -> Result<CheckedUnpaired<'a>, String> {
    let (name, token) = snapshot
        .strip_prefix("unpaired:")
        .and_then(|id| id.split_once(':'))
        .ok_or("Choose a listed unpaired recovery")?;
    if recovery_extension(name) != Some("psd") || token.len() != 36 {
        return Err("Choose a listed unpaired recovery".into());
    }
    let path = dir.join(name);
    let marker = path.with_extension("json");
    let check_marker = || -> Result<(), String> {
        if !matches!(fs::symlink_metadata(&marker), Err(error) if error.kind() == std::io::ErrorKind::NotFound) {
            return Err("Unpaired recovery marker changed; refresh the list".into());
        }
        Ok(())
    };
    control.begin()?;
    check_marker()?;
    let mut file = crate::file_safety::open_native_read(&path).map_err(|e| e.to_string())?;
    let candidate = unpaired_entry(name, &file)?;
    if candidate.snapshot != snapshot {
        return Err("Unpaired recovery file changed; refresh the list".into());
    }
    let version = crate::file_safety::resident_source_version(
        &path, &mut file, &mut [0; 64 * 1024], &|| control.check(),
    )?;
    if unpaired_entry(name, &file)?.snapshot != snapshot {
        return Err("Unpaired recovery file changed; refresh the list".into());
    }
    let doc = psd::decode_unpaired_autosave(&mut file, control, version)?;
    crate::file_safety::check_native_source_binding(&path, &file, version)?;
    check_marker()?;
    control.check()?;
    Ok(CheckedUnpaired {
        name,
        path,
        file,
        version,
        timestamp_ms: candidate.timestamp_ms,
        doc,
    })
}

pub(crate) fn repair_unpaired(
    workspace: &workspace::Registry,
    dir: &Path,
    snapshot: &str,
    control: &crate::loading::Control,
) -> Result<Snapshot, String> {
    let _operation = server::FileOperation::begin_workspace(workspace)?;
    let mut checked = check_unpaired(dir, snapshot, control)?;
    let (project, id, _) = recovery_name(checked.name).ok_or("Invalid recovery filename")?;
    let snapshot_id = uuid::Uuid::parse_str(id).map_err(|e| e.to_string())?;

    // The format-2 checksum must describe the same bytes the decoder accepted.
    checked.file.rewind().map_err(|e| e.to_string())?;
    let mut reader = (&mut checked.file).take(checked.version.bytes + 1);
    let mut buffer = [0; 64 * 1024];
    let mut bytes = 0;
    let mut hash = CHECKSUM_START;
    let mut sha = ring::digest::Context::new(&ring::digest::SHA256);
    loop {
        control.check()?;
        let read = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if read == 0 {
            break;
        }
        bytes += read as u64;
        hash = checksum_chunk(hash, &buffer[..read]);
        sha.update(&buffer[..read]);
    }
    if bytes != checked.version.bytes
        || crate::file_safety::finish_fingerprint(sha) != checked.version.sha256()
    {
        return Err("Unpaired recovery file changed; refresh the list".into());
    }
    let entry = Snapshot {
        version: 2,
        snapshot: id.into(),
        project_id: project.into(),
        document_id: checked.doc.id,
        revision: checked.doc.revision,
        name: checked.doc.name,
        original_path: None,
        bit_depth: checked.doc.bit_depth,
        timestamp_ms: checked.timestamp_ms,
        bytes,
        checksum: format!("{hash:016x}"),
        file: checked.name.into(),
    };
    let marker = marker_bytes(&entry)?;
    let gate = gate(dir);
    let _guard = gate.lock().unwrap();
    let catalog = scan_resident(dir, &|| control.check())?;
    if catalog.entries.iter().any(|entry| {
        uuid::Uuid::parse_str(&entry.snapshot).ok() == Some(snapshot_id)
    }) {
        return Err("A recovery record already uses this snapshot identity; refresh the list".into());
    }
    server::atomic_create(&entry.manifest(dir), &marker, |_| {
        crate::file_safety::check_native_source_binding(&checked.path, &checked.file, checked.version)?;
        control.check()
    })?;
    Ok(entry)
}

fn register_recovered(
    workspace: &workspace::Registry,
    doc: engine::Document,
    initiating: Option<&str>,
) -> Result<server::Shared, String> {
    let mut e = engine::Engine::from_document(doc);
    e.saved_revision = u64::MAX;
    e.status = "Recovered into an unsaved tab · choose Save As · recovery retained".into();
    // Register assigns a fresh runtime identity. Recovery files remain until explicit discard.
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

    #[test]
    fn saving_during_resident_checkpoint_staging_leaves_the_earlier_recovery() {
        let fixture = crate::engine::native::worker::tests::Fixture::new();
        let dir = fixture.path();
        let mut engine = engine::Engine::new();
        let project = engine.project_id.clone();
        let mut doc = engine::Document::new_depth(2, 1, 16).unwrap();
        let old = persist(dir, &project, &doc, None, &psd::encode(&doc).unwrap(), |_| Ok(()))
            .unwrap();
        doc.layers[0].pixels.set16(1, 0, [12347, 23459, 34571, 65535]);
        doc.revision += 1;
        engine.resident_state_mut().unwrap().document = doc.clone();
        let shared = Arc::new(Mutex::new(engine));
        assert!(resident_checkpoint_guard(&shared, &doc).unwrap().is_some());

        let bytes = psd::encode(&doc).unwrap();
        let original = dir.join("Original.psd");
        let result = persist(dir, &project, &doc, None, &bytes, |staged| {
            assert_eq!(fs::metadata(staged).unwrap().len(), bytes.len() as u64);
            server::save(&shared, &original).unwrap();
            resident_checkpoint_guard(&shared, &doc)?.ok_or_else(|| "Source saved".into())
        });
        assert_eq!(result.unwrap_err(), "Source saved");
        let inventory = scan_resident(dir, &|| Ok(())).unwrap();
        assert!(!inventory.incomplete);
        assert!(inventory.unpaired.is_empty());
        assert_eq!(inventory.entries.len(), 1);
        assert_eq!(inventory.entries[0].snapshot, old.snapshot);
        assert!(verify_checkpoint_data(dir, &old).unwrap());
        assert!(original.is_file());
    }

    #[test]
    fn cleanup_preserves_unpaired_psds_occupied_markers_and_unrelated_files() {
        let fixture = crate::engine::native::worker::tests::Fixture::new();
        let dir = fixture.path();
        let project = engine::id();
        let mut document = engine::Document::new_depth(2, 1, 16).unwrap();
        let sample = [12347, 23459, 34571, 65535];
        document.layers[0].pixels.set16(1, 0, sample);
        let encoded = psd::encode(&document).unwrap();
        let entry = persist(dir, &project, &document, None, &encoded, |_| Ok(())).unwrap();
        let good = persist(dir, &engine::id(), &document, None, &encoded, |_| Ok(())).unwrap();
        let marker = entry.manifest(dir);
        let marker_bytes = fs::read(&marker).unwrap();
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();

        let uncommitted = dir.join(format!("recovery-{project}-{}.psd", engine::id()));
        let temporary = uncommitted.with_extension(format!("{}.tmp", engine::id()));
        let retained_psd = uncommitted.with_extension("backup.psd");
        let retained_tmp = uncommitted.with_extension(format!("backup.{}.tmp", engine::id()));
        for path in [&uncommitted, &temporary, &retained_psd, &retained_tmp] {
            fs::write(path, &encoded).unwrap();
        }
        cleanup_orphans(dir).unwrap();
        assert!(!temporary.exists());
        assert!(marker.is_dir());
        for path in [&dir.join(&entry.file), &uncommitted, &retained_psd, &retained_tmp] {
            assert_eq!(fs::read(path).unwrap(), encoded);
        }
        let shared = Arc::new(Mutex::new(engine::Engine::new()));
        let registry = workspace::attach(&shared);
        let available = catalog_in(&registry, dir, &|| Ok(())).unwrap();
        assert_eq!(available.resident_error, Some(RESIDENT_CATALOG_ERROR));
        assert_eq!(available.versions.len(), 1);
        assert_eq!(available.versions[0].snapshot, good.snapshot);
        assert_eq!(available.unpaired.len(), 1);
        let candidate = &available.unpaired[0];
        let restored = restore(&registry, dir, &candidate.snapshot, &Default::default(), None).unwrap();
        {
            let restored = restored.lock().unwrap();
            let recovered = restored.resident().unwrap();
            assert_eq!(recovered.bit_depth, 16);
            assert_eq!(recovered.layers[0].pixels.get16(1, 0), sample);
            assert_ne!(recovered.id, document.id);
            assert!(restored.path.is_none() && restored.file_ownership.is_none());
            assert_ne!(restored.saved_revision, recovered.revision);
        }
        assert_eq!(fs::read(&uncommitted).unwrap(), encoded);
        let resource_bytes = u32::from_be_bytes(encoded[30..34].try_into().unwrap()) as usize;
        let conventional = [&encoded[..30], &[0; 4], &encoded[34 + resource_bytes..]].concat();
        fs::write(&uncommitted, conventional).unwrap();
        assert!(restore(&registry, dir, &candidate.snapshot, &Default::default(), None)
            .err().unwrap().contains("file changed"));
        assert!(repair_unpaired(&registry, dir, &candidate.snapshot, &Default::default())
            .unwrap_err().contains("file changed"));
        let current = catalog_in(&registry, dir, &|| Ok(())).unwrap();
        assert!(restore(&registry, dir, &current.unpaired[0].snapshot, &Default::default(), None)
            .err().unwrap().contains("no unique PeerBrush source"));
        assert!(repair_unpaired(&registry, dir, &current.unpaired[0].snapshot, &Default::default())
            .unwrap_err().contains("no unique PeerBrush source"));
        let mut damaged_source = encoded.clone();
        let source = damaged_source.windows(4).position(|bytes| bytes == b"PBR1").unwrap() + 4;
        damaged_source[source] = 0;
        fs::write(&uncommitted, damaged_source).unwrap();
        let current = catalog_in(&registry, dir, &|| Ok(())).unwrap();
        assert!(repair_unpaired(&registry, dir, &current.unpaired[0].snapshot, &Default::default())
            .unwrap_err().contains("no valid editable PeerBrush source"));
        assert!(!uncommitted.with_extension("json").exists());
        assert_eq!(persist(dir, &project, &document, None, &encoded, |_| Ok(())).unwrap_err(), RESIDENT_CATALOG_ERROR);
        assert!(native::admit_resident(&registry, dir, &project, encoded.len() as u64).is_err());
        assert!(discard_project(dir, &good.project_id).is_err());
        assert_eq!(fs::read(dir.join(&good.file)).unwrap(), encoded);

        fs::remove_dir(&marker).unwrap();
        fs::write(&marker, marker_bytes).unwrap();
        let unpaired = catalog_in(&registry, dir, &|| Ok(())).unwrap();
        assert_eq!(unpaired.resident_error, Some(RESIDENT_CATALOG_ERROR));
        assert_eq!(unpaired.versions.len(), 2);
        fs::write(&uncommitted, &encoded).unwrap();
        let current = catalog_in(&registry, dir, &|| Ok(())).unwrap();
        let candidate = &current.unpaired[0];
        let competing_marker = uncommitted.with_extension("json");
        fs::write(&competing_marker, b"other record").unwrap();
        assert!(repair_unpaired(&registry, dir, &candidate.snapshot, &Default::default())
            .unwrap_err().contains("marker changed"));
        assert_eq!(fs::read(&competing_marker).unwrap(), b"other record");
        fs::remove_file(&competing_marker).unwrap();
        let record = repair_unpaired(&registry, dir, &candidate.snapshot, &Default::default()).unwrap();
        assert_eq!(record.project_id, project);
        assert_eq!(record.document_id, document.id);
        assert_eq!(record.revision, document.revision);
        assert_eq!(record.bit_depth, 16);
        assert_eq!(record.timestamp_ms, candidate.timestamp_ms);
        assert!(record.original_path.is_none());
        assert!(verify_checkpoint_data(dir, &record).unwrap());
        assert_eq!(fs::read(&uncommitted).unwrap(), encoded);
        let repaired = catalog_in(&registry, dir, &|| Ok(())).unwrap();
        assert!(repaired.resident_error.is_none());
        assert!(repaired.unpaired.is_empty());
        assert_eq!(repaired.versions.len(), 3);
        let retained = persist(dir, &project, &document, None, &encoded, |_| Ok(())).unwrap();
        assert!(retained.snapshot == record.snapshot || retained.snapshot == entry.snapshot);
        let restored = restore(&registry, dir, &record.snapshot, &Default::default(), None).unwrap();
        let restored = restored.lock().unwrap();
        let recovered = restored.resident().unwrap();
        assert_eq!(recovered.bit_depth, 16);
        assert_eq!(recovered.layers[0].pixels.get16(1, 0), sample);
    }

    #[test]
    fn startup_copies_the_bounded_recovery_index_without_replacing_it_with_an_oversized_index() {
        let dir = std::env::temp_dir().join(format!("pb-recovery-index-{}", engine::id()));
        fs::create_dir(&dir).unwrap();
        let current = dir.join("recoveries.json");
        let previous = dir.join("previous-recoveries.json");
        let bytes = br#"{"projects":[{"file":"fixture.psd"}]}"#;
        fs::write(&current, bytes).unwrap();
        preserve_previous_index(&dir).unwrap();
        assert_eq!(fs::read(&previous).unwrap(), bytes);
        let mut oversized = br#"{"projects":[{"file":"too-large.psd"}]}"#.to_vec();
        oversized.resize(65536, b' ');
        fs::write(&current, &oversized).unwrap();
        preserve_previous_index(&dir).unwrap();
        assert_eq!(fs::read(&previous).unwrap(), bytes);
        assert_eq!(fs::read(&current).unwrap(), oversized);
        fs::remove_file(current).unwrap();
        fs::remove_file(previous).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    fn source(engine: &engine::Engine) -> Value {
        let history = |entries: &[engine::History]| {
            entries.iter().map(|entry| json!({
                "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
                "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted,
            })).collect::<Vec<_>>()
        };
        json!({"document":engine.resident_state().expect("resident fixture").document,"png":engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
            "undo":history(&engine.resident_state().expect("resident fixture").undo),"redo":history(&engine.resident_state().expect("resident fixture").redo),
            "saved_revision":engine.saved_revision,"path":engine.path,"closed":engine.closed})
    }

    #[test]
    fn clean_cleanup_releases_engine_but_retains_workspace_guard_and_later_native_work() {
        for depth in [8, 16] {
            let dir = std::env::temp_dir().join(format!("pb-recovery-lock-{}", engine::id()));
            fs::create_dir(&dir).unwrap();
            let mut engine = engine::Engine::new();
            engine
                .resident_state_mut()
                .expect("resident fixture")
                .document = engine::Document::new_depth(12, 8, depth).unwrap();
            engine
                .resident_state_mut()
                .expect("resident fixture")
                .document
                .layers[0]
                .pixels
                .set16(0, 0, [12347, 33559, 51237, 65535]);
            let layer = engine
                .resident_state()
                .expect("resident fixture")
                .document
                .layers[0]
                .id
                .clone();
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
                    assert_eq!(
                        engine
                            .resident_state()
                            .expect("resident fixture")
                            .document
                            .revision,
                        engine.saved_revision
                    );
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
                assert_eq!(
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .revision,
                    old.revision + 1
                );
                assert_eq!(
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .bit_depth,
                    depth
                );
                assert_eq!(
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .document
                        .layers[0]
                        .pixels
                        .get16(2, 1),
                    [53 * 257, 97 * 257, 173 * 257, 65535]
                );
                assert_eq!(
                    engine
                        .resident_state()
                        .expect("resident fixture")
                        .undo
                        .len(),
                    before["undo"].as_array().unwrap().len() + 1
                );
                assert_eq!(expected["undo"][0], before["undo"][0]);
                assert!(engine
                    .resident_state()
                    .expect("resident fixture")
                    .redo
                    .is_empty());
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
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .pixels
                    .rgba16()
            );
            assert_eq!(
                recovered.export_png().unwrap(),
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .export_png()
                    .unwrap()
            );
            assert_eq!(
                recovered.layers[0].pixels.get16(0, 0),
                engine
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .layers[0]
                    .pixels
                    .get16(0, 0)
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
