//! Session-admitted source I/O. Live resident rasters are not evicted by this service.
use crate::large_document::{
    root::{Directory, PersistentRootContext, RootOperation},
    BlockRef, CommitRef, DiskBudget, FreshFragment, Limits as StoreLimits, Reservation,
    ResidentBudget, Snapshot, Store, WeakStore,
};
use ring::digest::SHA256;
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
#[path = "source_runtime/segments.rs"]
mod segments;
#[path = "source_runtime/staging.rs"]
mod staging;
pub(crate) use segments::{SessionJxlOutput, SessionSegmentLog, SessionSegments};
pub(crate) use staging::{SessionBytes, SessionTemporary};
#[path = "source_runtime/recovery_marker.rs"]
mod recovery_marker;
pub(crate) use recovery_marker::NativeMarker;

pub(crate) struct Limits {
    pub resident_bytes: usize,
    pub disk_bytes: u64,
    pub max_files: usize,
    pub store: StoreLimits,
}
struct HeldFile {
    path: PathBuf,
    file: File,
    identity: (u64, u64),
    bytes: u64,
    modified: std::time::SystemTime,
}
impl HeldFile {
    fn new(path: PathBuf, file: File) -> Result<Self, String> {
        let metadata = file
            .metadata()
            .map_err(|_| "Source inventory metadata unavailable")?;
        let held = Self {
            path,
            identity: crate::file_safety::identity(&file)?,
            bytes: metadata.len(),
            modified: metadata
                .modified()
                .map_err(|_| "Source inventory timestamp unavailable")?,
            file,
        };
        held.check()?;
        Ok(held)
    }
    fn check(&self) -> Result<(), String> {
        let metadata =
            fs::symlink_metadata(&self.path).map_err(|_| "Source inventory file disappeared")?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err("Source inventory requires a regular owned file".into());
        }
        let named = crate::file_safety::open_native_identity(&self.path)
            .map_err(|_| "Source inventory name unavailable")?;
        let actual = self
            .file
            .metadata()
            .map_err(|_| "Source inventory metadata unavailable")?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || crate::file_safety::identity(&named)? != self.identity
            || crate::file_safety::identity(&self.file)? != self.identity
            || crate::file_safety::link_count(&named)? != 1
            || crate::file_safety::link_count(&self.file)? != 1
            || actual.len() != self.bytes
            || actual.modified().ok() != Some(self.modified)
        {
            return Err("Source inventory ownership changed; files retained".into());
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 {
                return Err("Source inventory alias refused".into());
            }
        }
        Ok(())
    }
}
struct Location {
    path: PathBuf,
    directory_identity: (u64, u64),
    file_identity: (u64, u64),
    weak: Option<WeakStore>,
    selected: Option<CommitRef>,
}
struct Inventory {
    directory: Directory,
    // The namespace owner's exact file holds the exclusive session inventory lock.
    files: Vec<HeldFile>,
    locations: BTreeMap<[u8; 16], Location>,
    lineages: BTreeMap<[u8; 16], [u8; 16]>,
    staging: BTreeMap<[u8; 16], staging::Location>,
    directories: usize,
    // Payload/index/path allocations above must drop before their accounting.
    _charges: Vec<Reservation>,
}
pub(crate) struct SourceRuntime {
    resident: ResidentBudget,
    disk: DiskBudget,
    limits: Limits,
    inventory: Mutex<Option<Inventory>>,
    tainted: AtomicBool,
    // Close inventory handles before releasing the actual default profile lease.
    context: Arc<PersistentRootContext>,
}
impl SourceRuntime {
    /// In-memory admission only. Blank/readonly health sessions create no files.
    pub(crate) fn new(
        context: Arc<PersistentRootContext>,
        limits: Limits,
    ) -> Result<Arc<Self>, String> {
        let resident = ResidentBudget::new(limits.resident_bytes).map_err(|e| e.to_string())?;
        let disk = DiskBudget::with_resident(limits.disk_bytes, limits.max_files, resident.clone())
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            context,
            resident,
            disk,
            limits,
            inventory: Mutex::new(None),
            tainted: AtomicBool::new(false),
        }))
    }
    pub(crate) fn resident(&self) -> ResidentBudget {
        self.resident.clone()
    }
    pub(crate) fn disk(&self) -> DiskBudget {
        self.disk.clone()
    }
    pub(crate) fn begin(self: &Arc<Self>) -> Result<SourceSession, String> {
        let operation = self.context.begin()?;
        let mut state = self
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        if state.is_none() {
            *state = Some(self.inventory_owned(&operation)?);
        }
        let directory = state.as_ref().unwrap().directory.path().to_owned();
        drop(state);
        let session = SourceSession {
            runtime: self.clone(),
            operation: Arc::new(operation),
            directory,
        };
        session.check()?;
        Ok(session)
    }
    fn inventory_owned(&self, operation: &RootOperation) -> Result<Inventory, String> {
        let directory = operation.source_directory()?;
        let per_entry = directory
            .path()
            .as_os_str()
            .len()
            .checked_mul(8)
            .and_then(|n| n.checked_add(8192))
            .ok_or("Source inventory footprint overflow")?;
        let mut remaining = self
            .limits
            .max_files
            .checked_mul(per_entry)
            .and_then(|n| n.checked_add(per_entry))
            .ok_or("Source inventory footprint overflow")?;
        let mut charges = Vec::new();
        charges
            .try_reserve_exact(remaining.div_ceil(64 * 1024 * 1024))
            .map_err(|_| "Source inventory charge allocation refused")?;
        while remaining != 0 {
            let count = remaining.min(64 * 1024 * 1024);
            charges.push(
                self.resident
                    .reserve_bytes(count, count)
                    .map_err(|e| e.to_string())?,
            );
            remaining -= count;
        }
        let owner_path = directory.path().join("owner.pbr");
        let mut owner = match fs::symlink_metadata(&owner_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if fs::read_dir(directory.path())
                    .map_err(|_| "Cannot inspect fresh source namespace")?
                    .next()
                    .is_some()
                {
                    return Err("Nonempty source namespace has no owner; files retained".into());
                }
                // Reserve before creating. Any uncertain failure retains charge.
                let charge = self.disk.reserve(56).map_err(|e| e.to_string())?;
                let mut options = OpenOptions::new();
                options.read(true).write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options
                    .open(&owner_path)
                    .map_err(|_| "Cannot create exclusive source namespace owner")?;
                charge.bind(&file).map_err(|e| e.to_string())?;
                file.try_lock()
                    .map_err(|_| "Persistent source namespace is in use")?;
                let mut bytes = [0; 56];
                bytes[..8].copy_from_slice(b"PBROOT01");
                bytes[8..24].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
                let digest = ring::digest::digest(&SHA256, &bytes[..24]);
                bytes[24..].copy_from_slice(digest.as_ref());
                file.write_all(&bytes)
                    .map_err(|_| "Cannot write source namespace owner")?;
                file.sync_all()
                    .map_err(|_| "Cannot sync source namespace owner")?;
                crate::server::sync_parent(&owner_path)?;
                file
            }
            Ok(_) => {
                let file = open_read(&owner_path)?;
                file.try_lock()
                    .map_err(|_| "Persistent source namespace is in use")?;
                file
            }
            Err(_) => return Err("Cannot admit persistent source namespace".into()),
        };
        let mut bytes = [0; 56];
        use std::io::{Seek, SeekFrom};
        owner
            .seek(SeekFrom::Start(0))
            .map_err(|_| "Cannot read source namespace owner")?;
        if owner
            .metadata()
            .map_err(|_| "Source namespace owner unavailable")?
            .len()
            != 56
        {
            return Err("Incomplete source namespace owner retained".into());
        }
        owner
            .read_exact(&mut bytes)
            .map_err(|_| "Cannot read source namespace owner")?;
        if &bytes[..8] != b"PBROOT01"
            || bytes[8..24] == [0; 16]
            || ring::digest::digest(&SHA256, &bytes[..24]).as_ref() != &bytes[24..56]
        {
            return Err(
                "Persistent source owner is incomplete or unsupported; files retained".into(),
            );
        }
        let mut files = Vec::new();
        files
            .try_reserve_exact(self.limits.max_files)
            .map_err(|_| "Source inventory allocation refused")?;
        files.push(HeldFile::new(owner_path, owner)?);
        let mut inventory = Inventory {
            directory,
            files,
            locations: BTreeMap::new(),
            lineages: BTreeMap::new(),
            staging: BTreeMap::new(),
            directories: 0,
            _charges: charges,
        };
        for entry in fs::read_dir(inventory.directory.path())
            .map_err(|_| "Cannot inventory persistent sources")?
        {
            operation.check()?;
            let entry = entry.map_err(|_| "Cannot inspect persistent source entry")?;
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "Invalid source namespace name")?;
            if name == "owner.pbr" {
                continue;
            }
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|_| "Cannot inspect source namespace entry")?;
            if metadata.is_dir() {
                let (_, uuid) = name
                    .split_once('-')
                    .ok_or("Unknown source directory retained")?;
                if !["source", "planar", "scalar", "bytes", "segments"]
                    .iter()
                    .any(|prefix| name.starts_with(&format!("{prefix}-")))
                    || uuid::Uuid::parse_str(uuid).is_err()
                {
                    return Err("Unknown source directory retained".into());
                }
                let child = Directory::open(&path)?;
                inventory.directories += 1;
                if inventory.directories > self.limits.max_files {
                    return Err("Source directory inventory limit exceeded".into());
                }
                for item in
                    fs::read_dir(child.path()).map_err(|_| "Cannot inventory source directory")?
                {
                    let item = item.map_err(|_| "Cannot inspect source file")?;
                    let file_name = item.file_name();
                    let file_name = file_name.to_str().ok_or("Invalid source file name")?;
                    let expected = match name.split_once('-').unwrap().0 {
                        "source" => "blocks.pbt",
                        "planar" => "planes.pbp",
                        "scalar" => "field.pbf",
                        "bytes" => "bytes.pbb",
                        "segments" => "segments.pbj",
                        _ => unreachable!(),
                    };
                    if file_name != expected {
                        return Err("Unknown source file retained".into());
                    }
                    let file = open_read(&item.path())?;
                    if file_name == "blocks.pbt" {
                        let mut header = [0; 24];
                        (&file)
                            .read_exact(&mut header)
                            .map_err(|_| "Incomplete source header retained")?;
                        if &header[..8] != b"PBTILES1" || header[8..] == [0; 16] {
                            return Err("Unsupported source header retained".into());
                        }
                        let id = header[8..].try_into().unwrap();
                        if inventory
                            .locations
                            .insert(
                                id,
                                Location {
                                    path: path.clone(),
                                    directory_identity: child.identity(),
                                    file_identity: crate::file_safety::identity(&file)?,
                                    weak: None,
                                    selected: None,
                                },
                            )
                            .is_some()
                        {
                            return Err("Duplicate source identity retained".into());
                        }
                    }
                    push_file(&mut inventory, item.path(), file, self.limits.max_files)?;
                }
                child.check()?;
            } else if valid_marker_name(&name) || valid_pending_name(&name) {
                push_file(
                    &mut inventory,
                    path.clone(),
                    open_read(&path)?,
                    self.limits.max_files,
                )?;
            } else {
                return Err("Unknown persistent source entry retained".into());
            }
        }
        // Exact logical-file admission joins the SAME pool already exposed to surfaces.
        for held in &inventory.files {
            operation.check()?;
            held.check()?;
            self.disk
                .retain_file(&held.path, &held.file)
                .map_err(|e| e.to_string())?;
        }
        operation.check()?;
        inventory.directory.check()?;
        // No callback after this final inventory-wide observation pass.
        for held in &inventory.files {
            held.check()?;
        }
        Ok(inventory)
    }
}

pub(crate) struct SourceSession {
    runtime: Arc<SourceRuntime>,
    operation: Arc<RootOperation>,
    directory: PathBuf,
}
/// Nominal lifetime only. The worker owner parks this with an uncertain child
/// until known exit; it grants no source I/O or publication capability.
#[must_use]
pub(crate) struct SessionRetention {
    // Close retained inventory handles before the final operation/profile owner.
    _runtime: Arc<SourceRuntime>,
    _operation: Arc<RootOperation>,
}
impl SourceSession {
    pub(crate) fn retain_for_child(&self) -> Result<SessionRetention, String> {
        self.check()?;
        Ok(SessionRetention {
            _runtime: self.runtime.clone(),
            _operation: self.operation.clone(),
        })
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        self.operation.check()?;
        self.check_inventory()
    }
    /// Existing permits remain held. This admits only explicit proven cleanup,
    /// never a new writer, publication, checkpoint or source allocation.
    pub(crate) fn cleanup_check(&self) -> Result<(), String> {
        self.operation.check_retirement()?;
        self.check_inventory()
    }
    fn check_inventory(&self) -> Result<(), String> {
        if self.runtime.tainted.load(Ordering::Acquire) {
            return Err(
                "Source inventory ownership changed; fresh admission required and bytes retained"
                    .into(),
            );
        }
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_ref().ok_or("Source inventory is unavailable")?;
        inventory.directory.check()?;
        for held in &inventory.files {
            held.check()?;
        }
        Ok(())
    }
    pub(crate) fn resident(&self) -> ResidentBudget {
        self.runtime.resident()
    }
    pub(crate) fn disk(&self) -> DiskBudget {
        self.runtime.disk()
    }
    pub(crate) fn fresh_fragment(&self) -> Result<FreshFragment, String> {
        self.check()?;
        self.reserve_directory()?;
        let path = self
            .directory
            .join(format!("source-{}", uuid::Uuid::new_v4()));
        let fragment = FreshFragment::create(
            &path,
            self.resident(),
            self.disk(),
            self.runtime.limits.store,
        )
        .map_err(|e| e.to_string())?;
        self.register(fragment.store(), &path)?;
        self.check()?;
        Ok(fragment)
    }
    pub(crate) fn abort_fragment(
        &self,
        fragment: FreshFragment,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        self.cleanup_check()?;
        cleanup()?;
        let identity = fragment.store().identity();
        let path = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let location = state
                .as_ref()
                .and_then(|inventory| inventory.locations.get(&identity))
                .ok_or("Fragment is outside the owned inventory")?;
            if !location
                .weak
                .as_ref()
                .and_then(WeakStore::upgrade)
                .is_some_and(|store| store.same_owner(fragment.store()))
            {
                return Err("Fragment differs from its registered owner".into());
            }
            location.path.clone()
        };
        let directory = Directory::open(&path)?;
        fragment
            .abort(&mut || {
                cleanup().map_err(|_| crate::large_document::Error::Cancelled)?;
                self.cleanup_check()
                    .map_err(|_| crate::large_document::Error::Changed)
            })
            .map_err(|e| e.to_string())?;
        if let Err(error) = directory.retire_empty(&mut || {
            cleanup()?;
            self.cleanup_check()
        }) {
            self.runtime.tainted.store(true, Ordering::Release);
            return Err(error);
        }
        self.cleanup_check()?;
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_mut().ok_or("Source inventory is unavailable")?;
        let location = inventory
            .locations
            .get(&identity)
            .ok_or("Retired fragment inventory changed")?;
        if location.path != path
            || location
                .weak
                .as_ref()
                .and_then(WeakStore::upgrade)
                .is_some()
        {
            return Err("Retired fragment gained an unexpected owner".into());
        }
        inventory.locations.remove(&identity);
        inventory.lineages.retain(|_, store| *store != identity);
        inventory.directories = inventory
            .directories
            .checked_sub(1)
            .ok_or("Retired fragment directory count differs")?;
        Ok(())
    }
    fn reserve_directory(&self) -> Result<(), String> {
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_mut().ok_or("Source inventory is unavailable")?;
        if inventory.directories >= self.runtime.limits.max_files {
            return Err("Source directory inventory limit exceeded".into());
        }
        inventory.directories += 1;
        Ok(())
    }
    fn register(&self, store: &Store, path: &Path) -> Result<(), String> {
        let held_path = path.join("blocks.pbt");
        let file = open_read(&held_path)?;
        let held = HeldFile::new(held_path, file)?;
        held.check()?;
        let directory_identity = Directory::open(path)?.identity();
        if store.filesystem_identity() != (directory_identity, held.identity) {
            return Err("New source differs from its inventoried filesystem owner".into());
        }
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let inventory = state.as_mut().ok_or("Source inventory is unavailable")?;
        if inventory.locations.contains_key(&store.identity()) {
            return Err("Duplicate source identity retained".into());
        }
        if inventory.locations.len() >= self.runtime.limits.max_files {
            return Err("Source inventory limit exceeded".into());
        }
        // The Store holds its own exact file. Do not retain another physical
        // handle which would keep an aborted file allocated after quota refund.
        inventory.locations.insert(
            store.identity(),
            Location {
                path: path.to_owned(),
                directory_identity,
                file_identity: held.identity,
                weak: Some(store.downgrade()),
                selected: None,
            },
        );
        Ok(())
    }
    pub(crate) fn store_for_lineage(&self, lineage: [u8; 16]) -> Result<Store, String> {
        if lineage == [0; 16] {
            return Err("Source lineage is empty".into());
        }
        self.check()?;
        let existing = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_ref().ok_or("Source inventory is unavailable")?;
            inventory
                .lineages
                .get(&lineage)
                .and_then(|id| inventory.locations.get(id))
                .map(|location| {
                    (
                        location.weak.as_ref().and_then(WeakStore::upgrade),
                        location.selected,
                    )
                })
        };
        if let Some((Some(store), _)) = existing {
            return Ok(store);
        }
        // A dropped unselected mirror has no authority to infer a newest commit.
        // Its bytes stay charged; a fresh mirror is admitted independently.
        self.reserve_directory()?;
        let path = self
            .directory
            .join(format!("source-{}", uuid::Uuid::new_v4()));
        let store = Store::create_with_disk_budget(
            &path,
            self.resident(),
            self.disk(),
            self.runtime.limits.store,
        )
        .map_err(|e| e.to_string())?;
        self.register(&store, &path)?;
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        state
            .as_mut()
            .unwrap()
            .lineages
            .insert(lineage, store.identity());
        Ok(store)
    }
    pub(crate) fn seal_store(
        &self,
        store: &Store,
        blocks: &[BlockRef],
        metadata: &[u8],
    ) -> Result<Snapshot, String> {
        self.check()?;
        if !store.uses_disk_budget(&self.disk())
            || !store.resident_budget().same_pool(&self.resident())
        {
            return Err("Source uses a foreign admission pool".into());
        }
        {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let registered = state
                .as_ref()
                .and_then(|inventory| inventory.locations.get(&store.identity()))
                .and_then(|location| location.weak.as_ref())
                .and_then(WeakStore::upgrade)
                .is_some_and(|registered| registered.same_owner(store));
            if !registered {
                return Err("Source is outside the owned inventory".into());
            }
        }
        let head = store.checkpoint_reference().map_err(|e| e.to_string())?;
        let source = head.map_or(store.identity(), |head| head.source());
        let revision = head
            .map_or(Some(1), |head| head.revision().checked_add(1))
            .ok_or("Source checkpoint sequence exhausted")?;
        let snapshot = store
            .commit(source, revision, head, metadata, blocks, &mut || {
                self.check()
                    .map_err(|_| crate::large_document::Error::Cancelled)
            })
            .map_err(|e| e.to_string())?;
        self.check()?;
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let location = state
            .as_mut()
            .and_then(|inventory| inventory.locations.get_mut(&store.identity()))
            .ok_or("Source is outside the owned inventory")?;
        location.selected = Some(snapshot.reference());
        Ok(snapshot)
    }
    pub(crate) fn resolve_store(
        &self,
        identity: [u8; 16],
        selected: CommitRef,
    ) -> Result<Snapshot, String> {
        self.check()?;
        let (held, path, directory_identity, file_identity) = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let location = state
                .as_ref()
                .and_then(|inventory| inventory.locations.get(&identity))
                .ok_or("Recovery source is outside the owned inventory")?;
            (
                location.weak.as_ref().and_then(WeakStore::upgrade),
                location.path.clone(),
                location.directory_identity,
                location.file_identity,
            )
        };
        let directory = Directory::open(&path).map_err(|error| {
            self.runtime.tainted.store(true, Ordering::Release);
            error
        })?;
        let named =
            crate::file_safety::open_native_identity(&path.join("blocks.pbt")).map_err(|_| {
                self.runtime.tainted.store(true, Ordering::Release);
                "Owned recovery source is unavailable"
            })?;
        if directory.identity() != directory_identity
            || crate::file_safety::identity(&named)? != file_identity
        {
            self.runtime.tainted.store(true, Ordering::Release);
            return Err(
                "Recovery source filesystem ownership changed; replacement retained".into(),
            );
        }
        let mut check = || {
            self.check()
                .map_err(|_| crate::large_document::Error::Cancelled)
        };
        let snapshot = match held {
            Some(store) => store.snapshot(selected, &mut check),
            None => Store::recover_with_disk_budget(
                &path,
                selected,
                self.resident(),
                self.disk(),
                self.runtime.limits.store,
                &mut check,
            ),
        }
        .map_err(|e| e.to_string())?;
        if snapshot.store().identity() != identity {
            return Err("Recovery source identity differs".into());
        }
        directory.check().map_err(|error| {
            self.runtime.tainted.store(true, Ordering::Release);
            error
        })?;
        let final_named = crate::file_safety::open_native_identity(&path.join("blocks.pbt"))
            .map_err(|_| {
                self.runtime.tainted.store(true, Ordering::Release);
                "Owned recovery source disappeared during admission"
            })?;
        if snapshot.store().filesystem_identity() != (directory_identity, file_identity)
            || crate::file_safety::identity(&named)? != file_identity
            || crate::file_safety::identity(&final_named)? != file_identity
        {
            self.runtime.tainted.store(true, Ordering::Release);
            return Err("Recovery source filesystem identity changed during admission".into());
        }
        self.check()?;
        let mut state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?;
        let location = state
            .as_mut()
            .unwrap()
            .locations
            .get_mut(&identity)
            .unwrap();
        location.weak = Some(snapshot.store().downgrade());
        location.selected = Some(selected);
        // Transfer the startup observation to the actual Store's held handle.
        // A duplicate inventory handle must not survive later owned retirement.
        state
            .as_mut()
            .unwrap()
            .files
            .retain(|held| held.path != path.join("blocks.pbt"));
        Ok(snapshot)
    }
}

fn open_read(path: &Path) -> Result<File, String> {
    crate::file_safety::open_native_read(path)
        .map_err(|_| "Cannot open owned source inventory file".into())
}
fn push_file(
    inventory: &mut Inventory,
    path: PathBuf,
    file: File,
    maximum: usize,
) -> Result<(), String> {
    if inventory.files.len() >= maximum {
        return Err("Persistent source inventory limit exceeded".into());
    }
    let held = HeldFile::new(path, file)?;
    held.check()?;
    inventory.files.push(held);
    Ok(())
}
fn valid_marker_name(name: &str) -> bool {
    name.strip_prefix("recovery-v3-")
        .and_then(|n| n.strip_suffix(".pbr").or_else(|| n.strip_suffix(".json")))
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}
fn valid_pending_name(name: &str) -> bool {
    name.strip_prefix("pending-")
        .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        base: PathBuf,
        held: Option<Directory>,
    }
    impl Fixture {
        fn new() -> Self {
            let base = std::env::temp_dir()
                .join(format!("peerbrush-source-runtime-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&base).unwrap();
            let base = fs::canonicalize(base).unwrap();
            let held = Some(Directory::open(&base).unwrap());
            fs::create_dir(base.join("legacy")).unwrap();
            Self { base, held }
        }
        fn locations(&self) -> crate::settings::Locations {
            crate::settings::Locations {
                config: self.base.join("config"),
                data: self.base.join("data"),
                cache: self.base.join("cache"),
            }
        }
        fn connection(&self) -> (crate::server::Shared, crate::server::Connection) {
            let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
            let connection =
                crate::server::start(shared.clone(), self.base.join("runtime")).unwrap();
            (shared, connection)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let temp = fs::canonicalize(std::env::temp_dir()).unwrap();
            if self.base.starts_with(temp)
                && self
                    .base
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("peerbrush-source-runtime-"))
                && self.held.as_ref().is_some_and(|held| held.check().is_ok())
                && fs::canonicalize(&self.base).is_ok_and(|p| p == self.base)
            {
                drop(self.held.take());
                let _ = fs::remove_dir_all(&self.base);
            }
        }
    }
    fn limits() -> Limits {
        Limits {
            resident_bytes: 8 * 1024 * 1024,
            disk_bytes: 8 * 1024 * 1024,
            max_files: 16,
            store: StoreLimits {
                disk_bytes: 4 * 1024 * 1024,
                max_blocks: 64,
                max_resident_blocks: 16,
                manifest_bytes: 1024 * 1024,
            },
        }
    }
    #[test]
    fn actual_default_profile_is_lazy_persistent_and_keeps_the_exact_lease() {
        let f = Fixture::new();
        let profile =
            crate::settings::Profile::open(f.locations(), &f.base.join("legacy")).unwrap();
        let (shared, connection) = f.connection();
        let context =
            PersistentRootContext::from_profile(&connection, profile.source_profile().unwrap())
                .unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        assert!(!f.locations().data.exists());
        assert_eq!(runtime.disk().used().unwrap(), 0);
        assert_eq!(runtime.resident().used(), 0);
        drop(profile);
        assert!(crate::settings::Profile::open(f.locations(), &f.base.join("legacy")).is_err());
        let session = runtime.begin().unwrap();
        assert!(f
            .locations()
            .data
            .join("native-sources-v1/owner.pbr")
            .is_file());
        assert!(!connection.state_dir.join("native-sources-v1").exists());
        assert_eq!(runtime.disk().used().unwrap(), 56);
        let workspace = crate::workspace::attach(&shared);
        assert!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(std::sync::atomic::Ordering::Acquire)
                > 0
        );
        let fragment = session.fresh_fragment().unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 80);
        fragment
            .abort(&mut || {
                session
                    .check()
                    .map_err(|_| crate::large_document::Error::Cancelled)
            })
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56);
        drop(session);
        drop(runtime);
        drop(context);
        drop(connection);
        assert!(crate::settings::Profile::open(f.locations(), &f.base.join("legacy")).is_ok());
    }
    #[test]
    fn explicit_source_inventory_rebuilds_one_pool_and_resolves_only_selected_owned_identity() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let store = session.store_for_lineage([6; 16]).unwrap();
        let spec = crate::large_document::TileSpec::new(2, 2, 16).unwrap();
        let block = store
            .put(spec, &[9; 32], &mut || {
                session
                    .check()
                    .map_err(|_| crate::large_document::Error::Cancelled)
            })
            .unwrap();
        let snapshot = session
            .seal_store(&store, &[block], b"source descriptor")
            .unwrap();
        let selected = snapshot.reference();
        let identity = store.identity();
        let charged = runtime.disk().used().unwrap();
        assert!(charged > 56 + 24 + 32);
        assert!(session.resolve_store([99; 16], selected).is_err());
        let restored = session.resolve_store(identity, selected).unwrap();
        assert_eq!(
            restored.store().pin(block, &mut || Ok(())).unwrap().bytes(),
            &[9; 32]
        );
        drop(restored);
        drop(snapshot);
        drop(store);
        drop(session);
        drop(runtime);
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 0);
        let session = runtime.begin().unwrap();
        assert_eq!(runtime.disk().used().unwrap(), charged);
        let restored = session.resolve_store(identity, selected).unwrap();
        assert!(restored
            .store()
            .resident_budget()
            .same_pool(&runtime.resident()));
        assert!(restored.store().uses_disk_budget(&runtime.disk()));
        assert_eq!(
            restored.store().pin(block, &mut || Ok(())).unwrap().bytes(),
            &[9; 32]
        );
        drop(restored);
        drop(session);
        drop(runtime);
        drop(context);
        drop(connection);
    }
    #[test]
    fn source_namespace_cannot_adopt_unowned_nonempty_or_competing_inventory() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let competing = SourceRuntime::new(context.clone(), limits()).unwrap();
        assert!(competing.begin().is_err());
        assert_eq!(runtime.disk().used().unwrap(), 56);
        drop(session);
        drop(runtime);
        drop(competing);
        drop(context);
        drop(connection);
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let operation = context.begin().unwrap();
        let directory = operation.source_directory().unwrap();
        fs::write(directory.path().join("foreign"), b"preserve").unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        assert!(runtime.begin().is_err());
        assert_eq!(runtime.disk().used().unwrap(), 0);
        assert_eq!(
            fs::read(directory.path().join("foreign")).unwrap(),
            b"preserve"
        );
        assert!(!directory.path().join("owner.pbr").exists());
        drop(operation);
        drop(runtime);
        drop(context);
        drop(connection);
    }
    #[test]
    fn late_namespace_replacement_refuses_source_writes_and_preserves_foreign_data() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let path = connection.state_dir.join("native-sources-v1");
        let renamed = fs::rename(&path, connection.state_dir.join("held-sources"));
        #[cfg(windows)]
        if let Err(error) = &renamed {
            // Windows may prevent moving the directory containing our held,
            // locked owner file. Assert that native prevention preserved it.
            assert!(matches!(error.raw_os_error(), Some(5 | 32)));
            assert!(!connection.state_dir.join("held-sources").exists());
            session.check().unwrap();
            assert_eq!(fs::metadata(path.join("owner.pbr")).unwrap().len(), 56);
            assert_eq!(runtime.disk().used().unwrap(), 56);
            return;
        }
        renamed.unwrap();
        fs::create_dir(&path).unwrap();
        fs::write(path.join("foreign"), b"preserve").unwrap();
        assert!(session.check().is_err());
        assert!(session.fresh_fragment().is_err());
        assert_eq!(fs::read(path.join("foreign")).unwrap(), b"preserve");
        assert_eq!(runtime.disk().used().unwrap(), 56);
        drop(session);
        drop(runtime);
        drop(context);
        drop(connection);
    }

    #[test]
    fn connection_factory_refuses_a_different_instances_directory() {
        let f = Fixture::new();
        let (_shared, mut connection) = f.connection();
        let other = f.base.join("other-runtime");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("instance.lock"), b"foreign instance").unwrap();
        connection.state_dir = other.clone();
        assert!(PersistentRootContext::from_explicit_state(&connection).is_err());
        assert_eq!(
            fs::read(other.join("instance.lock")).unwrap(),
            b"foreign instance"
        );
        assert!(!other.join("native-sources-v1").exists());
        drop(connection);
    }
    #[test]
    fn clean_registered_fragment_abort_reclaims_directory_slots_beyond_the_inventory_limit() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        for _ in 0..32 {
            let fragment = session.fresh_fragment().unwrap();
            session.abort_fragment(fragment, &mut || Ok(())).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), 56);
        }
        assert_eq!(
            fs::read_dir(connection.state_dir.join("native-sources-v1"))
                .unwrap()
                .count(),
            1
        );
        drop(session);
        drop(runtime);
        drop(context);
        drop(connection);
    }
    #[test]
    fn refused_fragment_abort_keeps_its_inventory_and_source_bytes() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let fragment = session.fresh_fragment().unwrap();
        let identity = fragment.store().identity();
        let held = fragment.store().clone();
        assert!(session.abort_fragment(fragment, &mut || Ok(())).is_err());
        assert_eq!(runtime.disk().used().unwrap(), 80);
        assert!(runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .locations
            .contains_key(&identity));
        assert_eq!(held.checkpoint_reference().unwrap(), None);
        drop(held);
        drop(session);
        drop(runtime);
        drop(context);
        drop(connection);
    }
    #[test]
    fn foreign_directory_after_file_abort_is_preserved_and_slot_remains_conservative() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let fragment = session.fresh_fragment().unwrap();
        let identity = fragment.store().identity();
        let path = runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .locations[&identity]
            .path
            .clone();
        let mut changed = false;
        let result = session.abort_fragment(fragment, &mut || {
            if !changed && !path.join("blocks.pbt").exists() {
                let renamed = fs::rename(
                    &path,
                    path.with_file_name(format!("retained-{}", uuid::Uuid::new_v4())),
                );
                #[cfg(windows)]
                if let Err(error) = &renamed {
                    assert!(matches!(error.raw_os_error(), Some(5 | 32)));
                    return Err("Native directory replacement prevented".into());
                }
                renamed.unwrap();
                fs::create_dir(&path).unwrap();
                fs::write(path.join("foreign"), b"preserve").unwrap();
                changed = true;
            }
            Ok(())
        });
        assert!(result.is_err());
        assert_eq!(runtime.disk().used().unwrap(), 56);
        if changed {
            assert_eq!(fs::read(path.join("foreign")).unwrap(), b"preserve");
        }
        assert!(runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .locations
            .contains_key(&identity));
        drop(session);
        drop(runtime);
        drop(context);
        drop(connection);
    }
    #[test]
    #[cfg(unix)]
    fn native_inventory_fifo_replacement_refuses_without_stalling_retirement() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt, sync::mpsc, time::Duration};
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let owner = session.directory.join("owner.pbr");
        fs::rename(&owner, owner.with_file_name("retained-owner.pbr")).unwrap();
        let name = CString::new(owner.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let (sender, done) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            assert!(session.check().is_err());
            assert!(session.cleanup_check().is_err());
            assert!(open_read(&owner).is_err());
            sender.send(()).unwrap();
        });
        done.recv_timeout(Duration::from_secs(2))
            .expect("Native inventory FIFO stalled its existing admission");
        worker.join().unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56);
    }
    #[test]
    fn native_marker_publication_is_exclusive_held_and_startup_inventory_only() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let snapshot = *uuid::Uuid::new_v4().as_bytes();
        let bytes = b"small opaque framing fixture";
        let hash = ring::digest::digest(&SHA256, bytes)
            .as_ref()
            .try_into()
            .unwrap();
        session
            .publish_recovery_marker(
                snapshot,
                bytes.len() as u64,
                hash,
                &mut |out| out.write_all(bytes).map_err(|e| e.to_string()),
                &|| Ok(()),
            )
            .unwrap();
        let mut marker = session.open_recovery_marker(snapshot).unwrap();
        assert_eq!(marker.snapshot(), snapshot);
        assert_eq!(marker.len(), bytes.len() as u64);
        let mut read = Vec::new();
        marker.read_to_end(&mut read).unwrap();
        assert_eq!(read, bytes);
        assert!(session
            .publish_recovery_marker(
                snapshot,
                bytes.len() as u64,
                hash,
                &mut |out| out.write_all(bytes).map_err(|e| e.to_string()),
                &|| Ok(())
            )
            .is_err());
        marker.check().unwrap();
        drop(marker);
        drop(session);
        drop(runtime);
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let mut marker = session.open_recovery_marker(snapshot).unwrap();
        read.clear();
        marker.read_to_end(&mut read).unwrap();
        assert_eq!(read, bytes);
        assert!(runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .locations
            .is_empty());
    }
    #[test]
    fn native_marker_stale_or_short_producers_never_select_a_partial_marker() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let snapshot = *uuid::Uuid::new_v4().as_bytes();
        assert!(session
            .publish_recovery_marker(
                snapshot,
                32,
                [1; 32],
                &mut |out| out.write_all(b"short").map_err(|e| e.to_string()),
                &|| Ok(())
            )
            .is_err());
        assert!(session.open_recovery_marker(snapshot).is_err());
        assert_eq!(runtime.disk().used().unwrap(), 56 + 32);
        let stale = std::cell::Cell::new(false);
        let other = *uuid::Uuid::new_v4().as_bytes();
        let bytes = b"stale";
        let hash = ring::digest::digest(&SHA256, bytes)
            .as_ref()
            .try_into()
            .unwrap();
        assert!(session
            .publish_recovery_marker(
                other,
                bytes.len() as u64,
                hash,
                &mut |out| {
                    out.write_all(bytes).map_err(|e| e.to_string())?;
                    stale.set(true);
                    Ok(())
                },
                &|| if stale.get() {
                    Err("Source changed before durable publication".into())
                } else {
                    Ok(())
                }
            )
            .is_err());
        assert!(session.open_recovery_marker(other).is_err());
        assert!(session
            .directory
            .join(format!("pending-{}", uuid::Uuid::from_bytes(other)))
            .is_file());
        assert!(!session
            .directory
            .join(format!("recovery-v3-{}.pbr", uuid::Uuid::from_bytes(other)))
            .exists());
    }
    #[test]
    fn admitted_provisional_retirement_finishes_during_connection_drop_before_instance_unlock() {
        use std::{
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };
        let f = Fixture::new();
        let (shared, connection) = f.connection();
        let state_dir = connection.state_dir.clone();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let fragment = session.fresh_fragment().unwrap();
        fragment
            .store()
            .put(
                crate::large_document::TileSpec::new(1, 1, 8).unwrap(),
                &[2; 4],
                &mut || Ok(()),
            )
            .unwrap();
        let mut partial = session
            .byte_temporary(128, crate::large_document::ByteRole::SourceBulk)
            .unwrap();
        partial
            .temporary()
            .append(&[3; 16], &mut || Ok(()))
            .unwrap();
        let completed = session
            .byte_temporary(0, crate::large_document::ByteRole::LayerData)
            .unwrap()
            .finish([4; 32], &mut || Ok(()))
            .unwrap();
        let (finished, done) = mpsc::channel();
        let dropper = thread::spawn(move || {
            drop(connection);
            finished.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.check().is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(session.check().is_err());
        assert!(runtime.begin().is_err());
        assert!(session.fresh_fragment().is_err());
        assert!(session
            .byte_temporary(0, crate::large_document::ByteRole::LayerData)
            .is_err());
        let workspace = crate::workspace::attach(&shared);
        let mut cleanup = || {
            assert!(session.check().is_err());
            session.cleanup_check()?;
            assert!(
                workspace
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire)
                    > 0
            );
            assert!(matches!(done.try_recv(), Err(mpsc::TryRecvError::Empty)));
            Ok(())
        };
        partial.abort(&mut cleanup).unwrap();
        completed.retire(&mut cleanup).unwrap();
        session.abort_fragment(fragment, &mut cleanup).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56);
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .directories,
            0
        );
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(session);
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        dropper.join().unwrap();
        let next = OpenOptions::new()
            .read(true)
            .write(true)
            .open(state_dir.join("instance.lock"))
            .unwrap();
        next.try_lock().unwrap();
    }
    #[test]
    fn session_byte_spools_register_before_append_and_reclaim_only_after_verified_retirement() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        for i in 0..32u8 {
            let mut staged = session
                .byte_temporary(4, crate::large_document::ByteRole::LayerData)
                .unwrap();
            staged.check().unwrap();
            assert_eq!(
                runtime
                    .inventory
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .staging
                    .len(),
                1
            );
            staged.temporary().append(&[i; 4], &mut || Ok(())).unwrap();
            let completed = staged.finish([i + 1; 32], &mut || Ok(())).unwrap();
            completed.check().unwrap();
            assert!(completed
                .bytes()
                .uses_budgets(&session.resident(), &session.disk()));
            let mut output =
                crate::large_document::ScratchBytes::new(&session.resident(), 4, 4).unwrap();
            completed
                .bytes()
                .read_range_into(0, &mut output, &mut || Ok(()))
                .unwrap();
            assert_eq!(&*output, &[i; 4]);
            completed.retire(&mut || Ok(())).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), 56);
            let state = runtime.inventory.lock().unwrap();
            assert_eq!(state.as_ref().unwrap().directories, 0);
            assert!(state.as_ref().unwrap().staging.is_empty());
        }
        let mut staged = session
            .byte_temporary(1024, crate::large_document::ByteRole::SourceBulk)
            .unwrap();
        staged.temporary().append(&[7; 64], &mut || Ok(())).unwrap();
        staged.abort(&mut || Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56);
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .directories,
            0
        );
    }
    #[test]
    fn escaped_byte_owner_keeps_inventory_and_quota_when_retirement_is_busy() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let staged = session
            .byte_temporary(0, crate::large_document::ByteRole::PhotoshopContainer)
            .unwrap();
        let completed = staged.finish([1; 32], &mut || Ok(())).unwrap();
        let escaped = completed.bytes().clone();
        let charged = session.disk().used().unwrap();
        assert!(completed.retire(&mut || Ok(())).is_err());
        assert_eq!(session.disk().used().unwrap(), charged);
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .directories,
            1
        );
        drop(escaped);
        assert_eq!(session.disk().used().unwrap(), charged);
    }
    #[test]
    fn orphan_byte_spool_startup_counts_actual_bytes_without_inventing_source_authority() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let mut staged = session
            .byte_temporary(4, crate::large_document::ByteRole::SourceBulk)
            .unwrap();
        staged.temporary().append(&[4; 4], &mut || Ok(())).unwrap();
        let completed = staged.finish([3; 32], &mut || Ok(())).unwrap();
        let charged = runtime.disk().used().unwrap();
        drop(completed);
        drop(session);
        drop(runtime);
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 0);
        let session = runtime.begin().unwrap();
        assert_eq!(runtime.disk().used().unwrap(), charged);
        let inventory = runtime.inventory.lock().unwrap();
        let inventory = inventory.as_ref().unwrap();
        assert_eq!(inventory.directories, 1);
        assert!(inventory.locations.is_empty());
        assert!(inventory.staging.is_empty());
        assert_eq!(inventory.files.len(), 2);
        session.operation.check().unwrap();
    }
    #[test]
    fn byte_wrapper_preserves_specific_callback_failure_and_retains_uncertain_debt() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let staged = session
            .byte_temporary(0, crate::large_document::ByteRole::LayerData)
            .unwrap();
        let charged = runtime.disk().used().unwrap();
        let error = staged.finish([1; 32], &mut || {
            Err("Captured source revision changed".into())
        });
        assert!(matches!(error, Err(ref message) if message == "Captured source revision changed"));
        assert_eq!(runtime.disk().used().unwrap(), charged);
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .staging
                .len(),
            1
        );
    }
    #[test]
    fn canceled_and_foreign_byte_spools_keep_unknown_bytes_and_registration() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let mut staged = session
            .byte_temporary(1024, crate::large_document::ByteRole::CompositeData)
            .unwrap();
        staged.temporary().append(&[3; 64], &mut || Ok(())).unwrap();
        let path = runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .staging
            .values()
            .next()
            .unwrap()
            .path
            .join("bytes.pbb");
        let old = path.with_file_name("retained.pbb");
        fs::rename(&path, &old).unwrap();
        fs::write(&path, b"foreign").unwrap();
        let charged = runtime.disk().used().unwrap();
        assert!(staged.abort(&mut || Ok(())).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"foreign");
        assert!(old.exists());
        assert_eq!(runtime.disk().used().unwrap(), charged);
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .staging
                .len(),
            1
        );
    }
    #[test]
    fn warm_runtime_refuses_a_same_bytes_foreign_source_after_the_last_store_drops() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let store = session.store_for_lineage([8; 16]).unwrap();
        let snapshot = session.seal_store(&store, &[], b"selected source").unwrap();
        let selected = snapshot.reference();
        let identity = store.identity();
        let path = runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .locations[&identity]
            .path
            .join("blocks.pbt");
        drop(snapshot);
        drop(store);
        let original = fs::read(&path).unwrap();
        let charged = runtime.disk().used().unwrap();
        fs::rename(&path, path.with_file_name("retained.pbt")).unwrap();
        fs::write(&path, &original).unwrap();
        assert!(session.resolve_store(identity, selected).is_err());
        assert!(session.fresh_fragment().is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(
            fs::read(path.with_file_name("retained.pbt")).unwrap(),
            original
        );
        assert_eq!(runtime.disk().used().unwrap(), charged);
        drop(session);
        drop(runtime);
        drop(context);
        drop(connection);
    }
    #[test]
    fn compatible_jxl_log_keeps_actual_session_and_canonical_inventory_until_retirement() {
        use crate::large_document::{
            segments::{SegmentEvent, SegmentLimits},
            ScratchBytes,
        };
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let spec = SegmentLimits {
            output_bytes: 1024,
            protocol_log_bytes: 65536,
            records: 32,
            events: 128,
        };
        let directories = runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .directories;
        assert!(session.segment_log(spec, [0; 16], [8; 32]).is_err());
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .directories,
            directories
        );
        let mut log = session.segment_log(spec, [7; 16], [8; 32]).unwrap();
        log.append(
            0,
            SegmentEvent::Observed { wire_kind: 2 },
            None,
            &mut || Ok(()),
        )
        .unwrap();
        let mut bytes = ScratchBytes::new(&session.resident(), 8, 65536).unwrap();
        bytes.copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let sha = ring::digest::digest(&SHA256, &bytes)
            .as_ref()
            .try_into()
            .unwrap();
        log.append(
            1,
            SegmentEvent::Data {
                offset: 0,
                sha256: sha,
            },
            Some((&bytes, bytes.len())),
            &mut || Ok(()),
        )
        .unwrap();
        log.append(
            2,
            SegmentEvent::Finalized { position: 8 },
            None,
            &mut || Ok(()),
        )
        .unwrap();
        log.append(
            3,
            SegmentEvent::Done {
                bytes: 8,
                records: 1,
            },
            None,
            &mut || Ok(()),
        )
        .unwrap();
        let log = log.finish(&mut || Ok(())).unwrap();
        let canonical = log.canonical(&mut || Ok(())).unwrap();
        assert_eq!(
            canonical.bytes().role(),
            crate::large_document::ByteRole::CompatibleJxlMode1
        );
        assert_eq!(canonical.bytes().payload_sha256(), sha);
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .staging
                .len(),
            2
        );
        canonical.check_session(&session).unwrap();
        let foreign = runtime.begin().unwrap();
        assert!(canonical.check_session(&foreign).is_err());
        drop(foreign);
        canonical.retire(&mut || session.cleanup_check()).unwrap();
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .staging
                .len(),
            0
        );
        assert_eq!(
            runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .directories,
            0
        );
        assert_eq!(runtime.disk().used().unwrap(), 56);
        drop(bytes);
        drop(session);
        drop(runtime);
        drop(connection);
    }
    #[test]
    fn unsealed_jxl_log_restart_accounts_actual_bytes_without_resuming_encoder() {
        use crate::large_document::segments::{SegmentEvent, SegmentLimits};
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let mut log = session
            .segment_log(
                SegmentLimits {
                    output_bytes: 1024,
                    protocol_log_bytes: 65536,
                    records: 32,
                    events: 128,
                },
                [7; 16],
                [8; 32],
            )
            .unwrap();
        log.append(
            0,
            SegmentEvent::Observed { wire_kind: 2 },
            None,
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56 + 256 + 96);
        drop(log);
        drop(session);
        drop(runtime);
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56 + 128 + 96);
        {
            let state = runtime.inventory.lock().unwrap();
            let inventory = state.as_ref().unwrap();
            assert_eq!(inventory.directories, 1);
            assert_eq!(inventory.files.len(), 2);
            assert!(inventory.locations.is_empty());
            assert!(inventory.staging.is_empty());
        }
        drop(session);
        drop(runtime);
        drop(connection);
    }
    #[test]
    fn connection_drop_waits_for_jxl_abort_without_authorizing_new_output_after_stop() {
        use crate::large_document::segments::{SegmentEvent, SegmentLimits};
        use std::{
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let spec = SegmentLimits {
            output_bytes: 1024,
            protocol_log_bytes: 65536,
            records: 32,
            events: 128,
        };
        let mut log = session.segment_log(spec, [7; 16], [8; 32]).unwrap();
        log.append(
            0,
            SegmentEvent::Observed { wire_kind: 2 },
            None,
            &mut || Ok(()),
        )
        .unwrap();
        let (finished, done) = mpsc::channel();
        let closer = thread::spawn(move || {
            drop(connection);
            finished.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while session.check().is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert!(session.check().is_err());
        assert!(done.try_recv().is_err());
        assert!(session.segment_log(spec, [7; 16], [8; 32]).is_err());
        log.abort(&mut || session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 56);
        assert!(done.try_recv().is_err());
        drop(session);
        done.recv_timeout(Duration::from_secs(2)).unwrap();
        closer.join().unwrap();
        drop(runtime);
    }
    #[test]
    fn nominal_child_retention_is_owned_send_static_without_granting_io() {
        fn owned_send<T: Send + 'static>() {}
        owned_send::<SessionRetention>();
    }
    #[test]
    fn uncertain_child_retention_preserves_profile_instance_and_file_operation_until_drop() {
        use std::{
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };
        let f = Fixture::new();
        let profile =
            crate::settings::Profile::open(f.locations(), &f.base.join("legacy")).unwrap();
        let (shared, connection) = f.connection();
        let state_dir = connection.state_dir.clone();
        let context =
            PersistentRootContext::from_profile(&connection, profile.source_profile().unwrap())
                .unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let workspace = crate::workspace::attach(&shared);
        let before = workspace
            .lock()
            .unwrap()
            .file_operations
            .load(Ordering::Acquire);
        assert_eq!(before, 1);
        let retained = session.retain_for_child().unwrap();
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            before
        );
        drop(profile);
        drop(context);
        let (finished, done) = mpsc::channel();
        let closer = thread::spawn(move || {
            drop(connection);
            finished.send(()).unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while session.check().is_ok() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(session.check().is_err());
        assert!(session.retain_for_child().is_err());
        drop(session);
        drop(runtime);
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            before
        );
        assert!(crate::settings::Profile::open(f.locations(), &f.base.join("legacy")).is_err());
        let instance = OpenOptions::new()
            .read(true)
            .write(true)
            .open(state_dir.join("instance.lock"))
            .unwrap();
        assert!(instance.try_lock().is_err());
        assert!(matches!(
            done.recv_timeout(Duration::from_millis(20)),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
        drop(retained);
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        closer.join().unwrap();
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            0
        );
        instance.try_lock().unwrap();
        assert!(crate::settings::Profile::open(f.locations(), &f.base.join("legacy")).is_ok());
    }
}
