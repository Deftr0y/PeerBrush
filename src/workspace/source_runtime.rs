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
    sync::{Arc, Mutex},
};

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
        let named = crate::file_safety::open_for_identity(&self.path)
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
    weak: Option<WeakStore>,
    selected: Option<CommitRef>,
}
struct Inventory {
    directory: Directory,
    // The namespace owner's exact file holds the exclusive session inventory lock.
    files: Vec<HeldFile>,
    locations: BTreeMap<[u8; 16], Location>,
    lineages: BTreeMap<[u8; 16], [u8; 16]>,
    directories: usize,
    // Payload/index/path allocations above must drop before their accounting.
    _charges: Vec<Reservation>,
}
pub(crate) struct SourceRuntime {
    resident: ResidentBudget,
    disk: DiskBudget,
    limits: Limits,
    inventory: Mutex<Option<Inventory>>,
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
        let disk =
            DiskBudget::new(limits.disk_bytes, limits.max_files).map_err(|e| e.to_string())?;
        Ok(Arc::new(Self {
            context,
            resident,
            disk,
            limits,
            inventory: Mutex::new(None),
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
            operation,
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
                if !["source", "planar", "scalar", "bytes"]
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
    operation: RootOperation,
    directory: PathBuf,
}
impl SourceSession {
    pub(crate) fn check(&self) -> Result<(), String> {
        self.operation.check()?;
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
        let (held, path) = {
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
            )
        };
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
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(7).custom_flags(0x00200000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
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
        .and_then(|n| n.strip_suffix(".json"))
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
}
