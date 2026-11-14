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
        Arc, Mutex, TryLockError,
    },
};
#[path = "source_runtime/scalars.rs"]
mod scalars;
#[path = "source_runtime/segments.rs"]
mod segments;
#[path = "source_runtime/staging.rs"]
mod staging;
pub(crate) use scalars::{SessionScalar, SessionScalarScratch};
pub(crate) use segments::{SessionJxlOutput, SessionSegmentLog, SessionSegments};
pub(crate) use staging::{SessionBytes, SessionTemporary};
#[path = "source_runtime/collection.rs"]
mod collection;
#[path = "source_runtime/maintenance.rs"]
mod maintenance;
mod read_session;
#[cfg(test)]
pub(crate) use collection::CollectionFault;
pub(crate) use collection::{Collection, SourceRetirement};
pub(crate) use read_session::{ReadChecks, SourceReadSession};
#[path = "source_runtime/destination.rs"]
mod destination;
pub(crate) use destination::SourceDestination;
#[path = "source_runtime/checkpoints.rs"]
mod checkpoints;
#[path = "source_runtime/recovery_marker.rs"]
mod recovery_marker;
#[cfg(test)]
pub(crate) use checkpoints::RetirementFault;
pub(crate) use checkpoints::{CheckpointCensus, RetiredMarker};
pub(crate) use recovery_marker::{NativeMarker, NativeMarkerIds, RetiredPendingMarker};
#[path = "source_runtime/publication.rs"]
mod publication;
pub(crate) use publication::{
    ArmedPublication, CommittedPublication, PublicationIntent, PublicationPlan, RefusedPublication,
    RegisteredCreation,
};

pub(crate) const INVENTORY_CHECK_BUSY: &str = "Source inventory validation is busy";

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
        let expected = (self.identity, 1);
        if crate::file_safety::file_identity_and_links(&named)? != expected
            || crate::file_safety::file_identity_and_links(&self.file)? != expected
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
    owner: [u8; 16],
    // The namespace owner's exact file holds the exclusive session inventory lock.
    files: Vec<HeldFile>,
    locations: BTreeMap<[u8; 16], Location>,
    lineages: BTreeMap<[u8; 16], [u8; 16]>,
    staging: BTreeMap<[u8; 16], staging::Location>,
    checkpoints: checkpoints::Registry,
    publications: publication::Registry,
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
    access: Arc<crate::large_document::SourceAccess>,
    maintenance: maintenance::Control,
    foreground: Arc<crate::large_document::SourceAccess>,
    checkpoints: maintenance::Control,
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
            access: Arc::new(Default::default()),
            maintenance: Default::default(),
            foreground: Arc::new(Default::default()),
            checkpoints: Default::default(),
        }))
    }
    pub(crate) fn resident(&self) -> ResidentBudget {
        self.resident.clone()
    }
    pub(crate) fn disk(&self) -> DiskBudget {
        self.disk.clone()
    }
    pub(crate) fn begin(self: &Arc<Self>) -> Result<SourceSession, String> {
        self.begin_mode(false)
    }
    /// Called off-lock by active source work that can yield background checkpoints.
    pub(crate) fn begin_foreground(self: &Arc<Self>) -> Result<SourceSession, String> {
        let foreground = self.foreground_activity()?;
        let mut session = self.begin()?;
        session.foreground = Some(foreground);
        Ok(session)
    }
    pub(crate) fn foreground_activity(&self) -> Result<crate::large_document::SourceRead, String> {
        self.checkpoints
            .read(&self.foreground)
            .map_err(|e| e.to_string())
    }
    pub(crate) fn try_checkpoint(&self) -> Result<Option<CheckpointScope<'_>>, String> {
        match self.checkpoints.start_exclusive(&self.foreground) {
            Ok(operation) => Ok(Some(CheckpointScope(operation))),
            Err(crate::large_document::Error::Busy) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
    pub(crate) fn begin_existing(self: &Arc<Self>) -> Result<SourceSession, String> {
        self.begin_mode(true)
    }
    pub(crate) fn begin_existing_if_present(
        self: &Arc<Self>,
    ) -> Result<Option<SourceSession>, String> {
        let _observation = self
            .maintenance
            .read(&self.access)
            .map_err(|e| e.to_string())?;
        let present = self
            .inventory
            .try_lock()
            .map_err(|_| "Source inventory is busy")?
            .is_some();
        if present {
            return self.begin_existing().map(Some);
        }
        let Some(operation) = self.context.begin_existing_sources()? else {
            return Ok(None);
        };
        self.begin_operation(operation, true).map(Some)
    }
    fn begin_mode(self: &Arc<Self>, existing_only: bool) -> Result<SourceSession, String> {
        let operation = if existing_only {
            self.context.begin_existing()?
        } else {
            self.context.begin()?
        };
        self.begin_operation(operation, existing_only)
    }
    fn begin_operation(
        self: &Arc<Self>,
        operation: RootOperation,
        existing_only: bool,
    ) -> Result<SourceSession, String> {
        let access = Arc::new(
            self.maintenance
                .read(&self.access)
                .map_err(|e| e.to_string())?,
        );
        let mut state = self
            .inventory
            .lock()
            .map_err(|_| "Source inventory is unavailable")?;
        if state.is_none() {
            *state = Some(self.inventory_owned(&operation, existing_only)?);
        }
        // Admission runs off-lock. Serialize the complete inventory validation
        // so readers woken together cannot lose requests to transient contention.
        operation.check()?;
        self.validate_inventory(state.as_ref().unwrap())?;
        let directory = state.as_ref().unwrap().directory.path().to_owned();
        operation.check()?;
        drop(state);
        let session = SourceSession {
            runtime: self.clone(),
            operation: Arc::new(operation),
            directory,
            id: *uuid::Uuid::new_v4().as_bytes(),
            inventory_check: Mutex::new(()),
            access: Some(access),
            foreground: None,
        };
        Ok(session)
    }
    fn validate_inventory(&self, inventory: &Inventory) -> Result<(), String> {
        if self.tainted.load(Ordering::Acquire) {
            return Err(
                "Source inventory ownership changed; fresh admission required and bytes retained"
                    .into(),
            );
        }
        inventory.directory.check()?;
        for held in &inventory.files {
            held.check()?;
        }
        Ok(())
    }
    fn inventory_owned(
        &self,
        operation: &RootOperation,
        existing_only: bool,
    ) -> Result<Inventory, String> {
        let directory = if existing_only {
            operation.existing_source_directory()?
        } else {
            operation.source_directory()?
        };
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
        let checkpoints = checkpoints::Registry::new(self.limits.max_files)?;
        let owner_path = directory.path().join("owner.pbr");
        let mut owner = match fs::symlink_metadata(&owner_path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if existing_only {
                    return Err(
                        "Existing native source namespace has no owner; recovery retained".into(),
                    );
                }
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
            owner: bytes[8..24].try_into().unwrap(),
            files,
            locations: BTreeMap::new(),
            lineages: BTreeMap::new(),
            staging: BTreeMap::new(),
            checkpoints,
            publications: publication::Registry::new(self.limits.max_files)?,
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
            } else if valid_marker_name(&name)
                || valid_pending_name(&name)
                || publication::recognized_name(&name)
            {
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
        publication::restore(self, operation, &mut inventory)?;
        operation.check()?;
        inventory.directory.check()?;
        // No callback after this final inventory-wide observation pass.
        for held in &inventory.files {
            held.check()?;
        }
        Ok(inventory)
    }
}

pub(crate) struct CheckpointScope<'a>(maintenance::ExclusiveOperation<'a>);
impl CheckpointScope<'_> {
    pub(crate) fn interrupted(&self) -> bool {
        self.0.interrupted()
    }
}

pub(crate) struct SourceSession {
    runtime: Arc<SourceRuntime>,
    operation: Arc<RootOperation>,
    directory: PathBuf,
    id: [u8; 16],
    // A codec supervisor and its pump share a session. Their read checks
    // must not reject each other as a competing inventory operation.
    inventory_check: Mutex<()>,
    access: Option<Arc<crate::large_document::SourceRead>>,
    foreground: Option<crate::large_document::SourceRead>,
}
#[must_use = "Retain the returned fragment for retry, or report unreconciled disk debt"]
pub(crate) struct RefusedFragmentRetirement {
    pub(crate) reason: String,
    pub(crate) fragment: Option<FreshFragment>,
}
/// Nominal lifetime only. The worker owner parks this with an uncertain child
/// until known exit; it grants no source I/O or publication capability.
#[must_use]
pub(crate) struct SessionRetention {
    // Close retained inventory handles before the final operation/profile owner.
    _runtime: Arc<SourceRuntime>,
    _operation: Arc<RootOperation>,
    _access: Arc<crate::large_document::SourceRead>,
}
impl SourceSession {
    pub(crate) fn park_activity(session: &mut Arc<Self>) -> Result<(), String> {
        let session =
            Arc::get_mut(session).ok_or("Proposal preparation still has active session owners")?;
        let operation = Arc::get_mut(&mut session.operation)
            .ok_or("Proposal preparation still has active child work")?;
        operation.park_activity();
        drop(session.foreground.take());
        Ok(())
    }
    pub(crate) fn activity(&self) -> Option<crate::server::FileOperation> {
        self.operation.activity()
    }
    /// Retained proposals may reacquire activity off-lock for a live operation.
    pub(crate) fn foreground_activity(&self) -> Result<crate::large_document::SourceRead, String> {
        self.runtime.foreground_activity()
    }
    pub(crate) fn uses_runtime(&self, runtime: &Arc<SourceRuntime>) -> bool {
        Arc::ptr_eq(&self.runtime, runtime)
    }
    #[cfg(test)]
    pub(crate) fn owned_file_bytes_for_test(&self, path: &Path) -> Result<Vec<u8>, String> {
        use std::io::{Seek, SeekFrom};
        self.check()?;
        let (store, held) = {
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let inventory = state.as_ref().ok_or("Source inventory unavailable")?;
            let store = inventory
                .locations
                .values()
                .find(|location| location.path.join("blocks.pbt") == path)
                .and_then(|location| location.weak.as_ref())
                .map(WeakStore::resume)
                .transpose()
                .map_err(|e| e.to_string())?;
            let held = inventory
                .files
                .iter()
                .find(|file| file.path == path)
                .map(|held| held.file.try_clone().map_err(|e| e.to_string()))
                .transpose()?;
            (store, held)
        };
        if let Some(store) = store {
            return store.file_bytes_for_test().map_err(|e| e.to_string());
        }
        let mut held = held.ok_or("Test read is outside the actual source inventory")?;
        if held.metadata().map_err(|e| e.to_string())?.len() > 64 * 1024 * 1024 {
            return Err("Test read exceeds fixture bound".into());
        }
        held.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        held.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        self.check()?;
        Ok(bytes)
    }
    pub(crate) fn with_live_fence<T>(&self, commit: impl FnOnce() -> T) -> Result<T, String> {
        self.operation.with_live_fence(commit)
    }
    pub(crate) fn retain_for_child(&self) -> Result<SessionRetention, String> {
        self.check()?;
        Ok(SessionRetention {
            _runtime: self.runtime.clone(),
            _operation: self.operation.clone(),
            _access: self
                .access
                .clone()
                .ok_or("Source session is in exclusive cleanup")?,
        })
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        self.operation.check()?;
        self.check_inventory()
    }
    /// Only for memory work over already pinned pixels or immutable metadata.
    /// Source I/O and publication still need the full inventory check.
    pub(crate) fn check_pinned_work(&self) -> Result<(), String> {
        self.operation.check_active()?;
        if self.runtime.tainted.load(Ordering::Acquire) {
            return Err("Source inventory ownership changed; fresh admission required and bytes retained".into());
        }
        Ok(())
    }
    /// Existing permits remain held. This admits only explicit proven cleanup,
    /// never a new writer, publication, checkpoint or source allocation.
    pub(crate) fn cleanup_check(&self) -> Result<(), String> {
        self.operation.check_retirement()?;
        self.check_inventory()
    }
    fn serialize_inventory_access(&self) -> Result<std::sync::MutexGuard<'_, ()>, String> {
        self.inventory_check
            .lock()
            .map_err(|_| "Source inventory check unavailable".into())
    }
    fn check_inventory(&self) -> Result<(), String> {
        if self.runtime.tainted.load(Ordering::Acquire) {
            return Err(
                "Source inventory ownership changed; fresh admission required and bytes retained"
                    .into(),
            );
        }
        let _check = self.serialize_inventory_access()?;
        let state = self
            .runtime
            .inventory
            .try_lock()
            .map_err(|error| match error {
                TryLockError::WouldBlock => INVENTORY_CHECK_BUSY,
                TryLockError::Poisoned(_) => "Source inventory is unavailable",
            })?;
        let inventory = state.as_ref().ok_or("Source inventory is unavailable")?;
        self.runtime.validate_inventory(inventory)
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
        self.try_abort_fragment(fragment, cleanup)
            .map_err(|refused| refused.reason)
    }
    pub(crate) fn try_abort_fragment(
        &self,
        fragment: FreshFragment,
        cleanup: &mut dyn FnMut() -> Result<(), String>,
    ) -> std::result::Result<(), RefusedFragmentRetirement> {
        let identity = fragment.store().identity();
        let filesystem = fragment.store().filesystem_identity();
        let mut prepare = || -> Result<_, String> {
            self.cleanup_check()?;
            cleanup()?;
            let state = self
                .runtime
                .inventory
                .try_lock()
                .map_err(|_| "Source inventory is busy")?;
            let location = state
                .as_ref()
                .and_then(|inventory| inventory.locations.get(&identity))
                .ok_or("Fragment is outside the owned inventory")?;
            if (location.directory_identity, location.file_identity) != filesystem {
                return Err(
                    "Fragment filesystem owner differs from its registered inventory".into(),
                );
            }
            if !location
                .weak
                .as_ref()
                .and_then(WeakStore::upgrade)
                .is_some_and(|store| store.same_owner(fragment.store()))
            {
                return Err("Fragment differs from its registered owner".into());
            }
            let path = location.path.clone();
            drop(state);
            let directory = Directory::open(&path)?;
            if directory.identity() != filesystem.0 {
                return Err("Fragment directory differs from its registered owner".into());
            }
            Ok((path, directory))
        };
        let (path, directory) = match prepare() {
            Ok(prepared) => prepared,
            Err(reason) => {
                return Err(RefusedFragmentRetirement {
                    reason,
                    fragment: Some(fragment),
                })
            }
        };
        let mut failure = None;
        fragment
            .try_abort(&mut || {
                cleanup()
                    .and_then(|_| self.cleanup_check())
                    .map_err(|reason| {
                        failure = Some(reason);
                        crate::large_document::Error::Cancelled
                    })
            })
            .map_err(
                |refused: crate::large_document::RefusedFragmentRetirement| {
                    RefusedFragmentRetirement {
                        reason: failure.unwrap_or_else(|| refused.error.to_string()),
                        fragment: refused.fragment,
                    }
                },
            )?;
        let finish = || -> Result<(), String> {
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
                || (location.directory_identity, location.file_identity) != filesystem
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
        };
        finish().map_err(|reason| RefusedFragmentRetirement {
            reason,
            fragment: None,
        })
    }
    fn reserve_directory(&self) -> Result<(), String> {
        let _check = self.serialize_inventory_access()?;
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
        let weak = store.downgrade().map_err(|e| e.to_string())?;
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
                weak: Some(weak),
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
                .and_then(|location| location.weak.as_ref())
                .map(WeakStore::resume)
                .transpose()
                .map_err(|e| e.to_string())?
        };
        if let Some(store) = existing {
            return Ok(store);
        }
        // Only a new lineage creates a file. Retained custody above resumes the
        // actual known state without inferring a newest checkpoint from disk.
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
        self.try_seal_store(store, blocks, metadata)?
            .ok_or_else(|| {
                "Readonly source prefix does not cover every required reference; bytes retained"
                    .into()
            })
    }
    /// None requires a new physical copy; it grants no selected-prefix authority.
    pub(crate) fn try_seal_store(
        &self,
        store: &Store,
        blocks: &[BlockRef],
        metadata: &[u8],
    ) -> Result<Option<Snapshot>, String> {
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
        if let Some(snapshot) = store
            .checkpoint_snapshot(&mut || {
                self.check()
                    .map_err(|_| crate::large_document::Error::Changed)
            })
            .map_err(|e| e.to_string())?
        {
            if Some(snapshot.reference()) != head {
                return Err("Source checkpoint changed during coverage admission".into());
            }
            let bytes = blocks
                .len()
                .checked_mul(std::mem::size_of::<([u8; 32], bool)>())
                .and_then(|n| n.checked_mul(2))
                .and_then(|n| n.checked_add(128))
                .ok_or("Source reference coverage footprint overflow")?;
            let _coverage = self
                .resident()
                .reserve_bytes(bytes, 64 * 1024 * 1024)
                .map_err(|e| e.to_string())?;
            let mut needed = Vec::new();
            needed
                .try_reserve_exact(blocks.len())
                .map_err(|_| "Source coverage allocation refused")?;
            if needed.capacity() > blocks.len().saturating_mul(2) {
                return Err("Source coverage exceeds admitted capacity".into());
            }
            needed.extend(
                blocks
                    .iter()
                    .map(|reference| (reference.fingerprint(), false)),
            );
            needed.sort_unstable_by_key(|(key, _)| *key);
            needed.dedup_by_key(|(key, _)| *key);
            snapshot
                .visit_blocks(
                    &mut || {
                        self.check()
                            .map_err(|_| crate::large_document::Error::Changed)
                    },
                    |reference| {
                        if let Ok(at) =
                            needed.binary_search_by_key(&reference.fingerprint(), |(key, _)| *key)
                        {
                            needed[at].1 = true;
                        }
                        Ok(())
                    },
                )
                .map_err(|e| e.to_string())?;
            if needed.iter().all(|(_, covered)| *covered) {
                self.record_selected(store, snapshot.reference())?;
                return Ok(Some(snapshot));
            }
            if snapshot.has_unconfirmed_tail().map_err(|e| e.to_string())? {
                if let Some(retained) = self.retained_checkpoint_source(store, &mut needed)? {
                    self.record_selected(store, retained.reference())?;
                    return Ok(Some(retained));
                }
                return Ok(None);
            }
        }
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
        self.record_selected(store, snapshot.reference())?;
        Ok(Some(snapshot))
    }
    fn record_selected(&self, store: &Store, selected: CommitRef) -> Result<(), String> {
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
        if !location
            .weak
            .as_ref()
            .and_then(WeakStore::upgrade)
            .is_some_and(|owner| owner.same_owner(store))
        {
            return Err("Source checkpoint owner changed before registration".into());
        }
        location.selected = Some(selected);
        drop(state);
        self.check()
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
                location
                    .weak
                    .as_ref()
                    .map(WeakStore::resume)
                    .transpose()
                    .map_err(|e| e.to_string())?,
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
        let weak = snapshot.store().downgrade().map_err(|e| e.to_string())?;
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
        location.weak = Some(weak);
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
    pub(super) struct Fixture {
        base: PathBuf,
        held: Option<Directory>,
    }
    impl Fixture {
        pub(super) fn new() -> Self {
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
        pub(super) fn connection(&self) -> (crate::server::Shared, crate::server::Connection) {
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
    pub(super) fn limits() -> Limits {
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
    fn three_zero_native_read_frames_keep_default_data_absent_and_source_permits_bounded() {
        use crate::engine::{
            native::NativeDocument,
            scene::{SceneIdentity, SceneSnapshot},
        };
        let f = Fixture::new();
        let profile =
            crate::settings::Profile::open(f.locations(), &f.base.join("legacy")).unwrap();
        let (shared, connection) = f.connection();
        let context =
            PersistentRootContext::from_profile(&connection, profile.source_profile().unwrap())
                .unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let document =
            NativeDocument::new_blank(7, 5, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let scene = Arc::new(
            SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: shared.lock().unwrap().project_id.clone(),
                    document_id: document.metadata().id().into(),
                    revision: document.metadata().revision(),
                },
                document.clone(),
                Vec::new(),
                None,
                None,
            )
            .unwrap(),
        );
        let workspace = crate::workspace::attach(&shared);
        let baseline = runtime.resident().used();
        assert!(!f.locations().data.exists());
        let guard = runtime.begin_read(&scene).unwrap();
        let other =
            NativeDocument::new_blank(7, 5, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let other_scene = SceneSnapshot::from_native(
            SceneIdentity {
                project_id: scene.identity().project_id.clone(),
                document_id: other.metadata().id().into(),
                revision: other.metadata().revision(),
            },
            other.clone(),
            Vec::new(),
            None,
            None,
        )
        .unwrap();
        assert!(guard.check_scene(&other_scene).is_err());
        drop((guard, other_scene, other));
        assert_eq!(runtime.resident().used(), baseline);
        let refused = runtime
            .resident()
            .reserve_bytes(
                runtime.resident().limit() - baseline - 512,
                runtime.resident().limit(),
            )
            .unwrap();
        assert!(runtime.begin_read(&scene).is_err());
        drop(refused);
        assert!(!f.locations().data.exists());
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            0
        );
        for _ in 0..3 {
            let guard = runtime.begin_read(&scene).unwrap();
            assert!(
                workspace
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire)
                    > 0
            );
            assert_eq!(runtime.resident().used(), baseline + 1024);
            let request = crate::render::native_preview::Request::capture(
                scene.clone(),
                guard,
                None,
                7,
                None,
                false,
            )
            .unwrap();
            let frame = request.render(&mut || Ok(())).unwrap();
            assert!(frame
                .stripes()
                .iter()
                .all(|stripe| stripe.bytes().iter().all(|byte| *byte == 0)));
            drop((request, frame));
            assert_eq!(runtime.resident().used(), baseline);
            assert_eq!(
                workspace
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire),
                0
            );
            assert!(!f.locations().data.exists());
        }
        drop((scene, document));
        assert_eq!(runtime.disk().used().unwrap(), 0);
        assert_eq!(runtime.resident().used(), 0);
        assert!(runtime.inventory.lock().unwrap().is_none());
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            0
        );
        assert!(!f.locations().data.exists());
        drop((runtime, context, profile, connection));
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
    fn inventory_validation_reports_contention_until_the_lock_is_released() {
        let fixture = Fixture::new();
        let (_shared, connection) = fixture.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = SourceReadSession::from(Arc::new(runtime.begin().unwrap()));
        let held = runtime.inventory.lock().unwrap();
        session.check_pinned_work().unwrap();
        assert_eq!(session.check().unwrap_err(), INVENTORY_CHECK_BUSY);
        drop(held);
        session.check().unwrap();
        runtime.tainted.store(true, Ordering::Release);
        assert!(session.check_pinned_work().is_err());
    }

    #[test]
    fn fragment_retirement_refusal_returns_owned_tokens_for_retry_before_unlink() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        for mode in ["busy", "reader", "cancel", "inventory"] {
            let fragment = session.fresh_fragment().unwrap();
            let identity = fragment.store().identity();
            let filesystem = fragment.store().filesystem_identity();
            let reader = (mode == "reader").then(|| fragment.store().clone());
            let busy = (mode == "busy").then(|| runtime.inventory.lock().unwrap());
            if mode == "inventory" {
                runtime
                    .inventory
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .locations
                    .get_mut(&identity)
                    .unwrap()
                    .file_identity
                    .0 ^= 1;
            }
            let before = runtime.disk().used().unwrap();
            let refused = session
                .try_abort_fragment(fragment, &mut || {
                    if mode == "cancel" {
                        Err("Stop proposal cleanup".into())
                    } else {
                        Ok(())
                    }
                })
                .err()
                .unwrap();
            if mode == "cancel" {
                assert_eq!(refused.reason, "Stop proposal cleanup");
            }
            let fragment = refused.fragment.expect("No unlink was attempted");
            assert_eq!(fragment.store().identity(), identity);
            assert_eq!(fragment.store().filesystem_identity(), filesystem);
            assert_eq!(runtime.disk().used().unwrap(), before);
            drop((busy, reader));
            if mode == "inventory" {
                runtime
                    .inventory
                    .lock()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .locations
                    .get_mut(&identity)
                    .unwrap()
                    .file_identity = filesystem.1;
            }
            session
                .try_abort_fragment(fragment, &mut || Ok(()))
                .unwrap_or_else(|refused| panic!("{}", refused.reason));
            assert!(!runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .locations
                .contains_key(&identity));
            assert_eq!(runtime.disk().used().unwrap(), 56);
        }
    }
    #[test]
    fn namespace_retirement_failure_after_unlink_does_not_return_a_retry_token() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
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
        let refused = session
            .try_abort_fragment(fragment, &mut || {
                if path.join("blocks.pbt").exists() {
                    Ok(())
                } else {
                    Err("Stop after file unlink".into())
                }
            })
            .err()
            .unwrap();
        assert_eq!(refused.reason, "Stop after file unlink");
        assert!(refused.fragment.is_none());
        assert!(runtime.tainted.load(Ordering::Acquire));
        assert!(!path.join("blocks.pbt").exists());
        assert!(path.is_dir());
        assert!(runtime
            .inventory
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .locations
            .contains_key(&identity));
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
    fn held_inventory_file_refuses_extra_links_replaced_names_and_changed_bytes() {
        let f = Fixture::new();
        let path = f.base.join("owned.pbr");
        let moved = f.base.join("retained.pbr");
        fs::write(&path, b"source").unwrap();
        let held = HeldFile::new(path.clone(), open_read(&path).unwrap()).unwrap();
        held.check().unwrap();

        fs::hard_link(&path, &moved).unwrap();
        assert!(held.check().is_err());
        fs::remove_file(&moved).unwrap();
        held.check().unwrap();

        fs::rename(&path, &moved).unwrap();
        fs::write(&path, b"source").unwrap();
        assert!(held.check().is_err());
        fs::remove_file(&path).unwrap();
        fs::rename(&moved, &path).unwrap();
        held.check().unwrap();

        fs::write(&path, b"changed source").unwrap();
        assert!(held.check().is_err());
        assert_eq!(fs::read(&path).unwrap(), b"changed source");
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
    fn warm_lineage_resumes_exact_custody_without_another_file_or_checkpoint() {
        for depth in [8, 16] {
            let f = Fixture::new();
            let (_shared, connection) = f.connection();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let runtime = SourceRuntime::new(context, limits()).unwrap();
            let session = runtime.begin().unwrap();
            let store = session.store_for_lineage([0x71; 16]).unwrap();
            let spec = crate::large_document::TileSpec::new(2, 2, depth).unwrap();
            let bytes = vec![47; spec.bytes()];
            let reference = store.put(spec, &bytes, &mut || Ok(())).unwrap();
            let snapshot = session
                .seal_store(&store, &[reference], b"retained lineage")
                .unwrap();
            let selected = snapshot.reference();
            let identity = store.identity();
            let filesystem = store.filesystem_identity();
            let original = store.file_bytes_for_test().unwrap();
            let charged = runtime.disk().used().unwrap();
            let (locations, directories) = {
                let state = runtime.inventory.lock().unwrap();
                let inventory = state.as_ref().unwrap();
                (inventory.locations.len(), inventory.directories)
            };
            drop((snapshot, store));
            for _ in 0..3 {
                let store = session.store_for_lineage([0x71; 16]).unwrap();
                assert_eq!(store.identity(), identity);
                assert_eq!(store.filesystem_identity(), filesystem);
                assert_eq!(store.checkpoint_reference().unwrap(), Some(selected));
                assert_eq!(store.put(spec, &bytes, &mut || Ok(())).unwrap(), reference);
                let snapshot = session
                    .seal_store(&store, &[reference], b"different capture metadata")
                    .unwrap();
                assert_eq!(snapshot.reference(), selected);
                assert_eq!(store.pin(reference, &mut || Ok(())).unwrap().bytes(), bytes);
                assert_eq!(store.file_bytes_for_test().unwrap(), original);
                assert_eq!(runtime.disk().used().unwrap(), charged);
                let state = runtime.inventory.lock().unwrap();
                let inventory = state.as_ref().unwrap();
                assert_eq!(
                    (inventory.locations.len(), inventory.directories),
                    (locations, directories)
                );
                drop(state);
                drop((snapshot, store));
            }
        }
    }
    #[test]
    fn readonly_selected_source_reuses_only_a_complete_covering_original_prefix() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let store = session.store_for_lineage([3; 16]).unwrap();
        let spec = crate::large_document::TileSpec::new(1, 1, 8).unwrap();
        let first = store.put(spec, &[1; 4], &mut || Ok(())).unwrap();
        let second = store.put(spec, &[2; 4], &mut || Ok(())).unwrap();
        let original = session
            .seal_store(&store, &[first], b"original physical prefix")
            .unwrap();
        let selected = original.reference();
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
        drop(original);
        drop(store);
        drop(session);
        drop(runtime);
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"tail").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin_existing().unwrap();
        let recovered = session.resolve_store(identity, selected).unwrap();
        assert!(recovered.has_unconfirmed_tail().unwrap());
        let source = recovered.store();
        let before = fs::metadata(&path).unwrap().len();
        let reused = session
            .seal_store(
                &source,
                &[first, first],
                b"new document metadata is separate",
            )
            .unwrap();
        assert_eq!(reused.reference(), selected);
        assert_eq!(
            &*reused.metadata(&mut || Ok(())).unwrap(),
            b"original physical prefix"
        );
        assert_eq!(fs::metadata(&path).unwrap().len(), before);
        assert!(session
            .seal_store(&source, &[second], b"missing from selected table")
            .is_err());
        let foreign = session.store_for_lineage([4; 16]).unwrap();
        let copied = foreign.put(spec, &[1; 4], &mut || Ok(())).unwrap();
        assert!(session
            .seal_store(&source, &[copied], b"foreign reference")
            .is_err());
        assert_eq!(fs::metadata(&path).unwrap().len(), before);
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
        let ids = session.recovery_marker_ids().unwrap();
        assert_eq!(ids.iter().collect::<Vec<_>>(), [snapshot]);
        drop(ids);
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
        assert_eq!(
            runtime.disk().used().unwrap(),
            if cfg!(windows) { 56 } else { 56 + 32 }
        );
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
        assert_eq!(
            session
                .directory
                .join(format!("pending-{}", uuid::Uuid::from_bytes(other)))
                .is_file(),
            !cfg!(windows)
        );
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
        let scalar_spec = crate::large_document::ScalarSpec::new(1, 1).unwrap();
        let partial_scalar = session.scalar_temporary(scalar_spec).unwrap();
        partial_scalar.check().unwrap();
        let mut scalar = session.scalar_temporary(scalar_spec).unwrap();
        scalar
            .append_rows(0, &0.37f64.to_be_bytes(), &mut || Ok(()))
            .unwrap();
        let scalar = scalar.finish([5; 32], &mut || Ok(())).unwrap();
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
        assert!(session.scalar_temporary(scalar_spec).is_err());
        assert!(scalar.check().is_err());
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
        partial_scalar.abort(&mut cleanup).unwrap();
        scalar.retire(&mut cleanup).unwrap();
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
    fn captured_global_style_dependencies_match_native_layers_and_preview_before_retirement() {
        use crate::{
            compositor::native::{Native16, Native8, Plan, Stored, Topology},
            effects::Effect,
            engine::scene::{CapturedScene, LegacyTileCache},
            raster::backed::Region,
            render::native_preview::Request,
        };
        for depth in [8, 16] {
            for kind in [
                "drop_shadow",
                "inner_shadow",
                "outer_glow",
                "inner_glow",
                "satin",
                "bevel_emboss",
                "gradient_overlay",
                "liquify",
                "liquify_after_style",
            ] {
                let f = Fixture::new();
                let (_shared, connection) = f.connection();
                let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
                let mut limits = limits();
                limits.resident_bytes = 128 * 1024 * 1024;
                limits.disk_bytes = 128 * 1024 * 1024;
                limits.max_files = 64;
                let runtime = SourceRuntime::new(context, limits).unwrap();
                let mut doc = crate::compositor::native::tests::doc(depth, false, "screen");
                // Editable metadata supplements this captured current raster;
                // global effects must not replace it with a fresh vector render.
                doc.layers[0].source = Some(Arc::new(crate::source::Source {
                    width: doc.layers[0].pixels.width,
                    height: doc.layers[0].pixels.height,
                    matrix: [1., 0., 0., 1., 0., 0.],
                    content: crate::source::Content::Shape {
                        shape: "rectangle".into(),
                        bounds: [0., 0., 3., 2.],
                        points: vec![],
                        closed: true,
                        fill: [12347, 23459, 34571, 45679],
                        stroke: [0; 4],
                        stroke_width: 0.,
                    },
                }));
                let effect = |kind: &str, weight| Effect {
                    id: crate::engine::id(),
                    kind: kind.into(),
                    enabled: true,
                    weight,
                    settings: crate::effects::defaults(kind).into(),
                };
                doc.layers[0].effects.push(effect("blur", 0.57));
                let after_style = kind == "liquify_after_style";
                if after_style {
                    doc.layers[0].effects.push(effect("stroke", 0.53));
                }
                let mut style = effect(if after_style { "liquify" } else { kind }, 0.67);
                if kind == "gradient_overlay" {
                    let settings = Arc::make_mut(&mut style.settings);
                    settings["pixel_grid"] = serde_json::json!(true);
                    settings["align_layer"] = serde_json::json!(true);
                    settings["angle"] = serde_json::json!(37.);
                }
                if style.kind == "liquify" {
                    let mut mask = crate::raster::Raster::new(11, 9);
                    for y in 0..9 {
                        for x in 0..11 {
                            mask.set(x, y, [((x * 37 + y * 51) % 256) as u8, 255, 173, 0]);
                        }
                    }
                    let coverage = crate::selection::Coverage {
                        mask,
                        bounds: [1, 2, 3, 4],
                        origin: [-2, 1],
                        contours: vec![],
                    };
                    let mut settings = serde_json::json!({"radius":2.75,"strength":0.83,"strokes":[
                        {"mode":"push","points":[[-1.25,1.5],[4.5,3.25],[8.5,5.5]],"selection":[0,0,9,7],"polygon":[[0.,0.],[9.,0.],[7.5,7.],[1.5,7.]],"coverage":coverage},
                        {"mode":"expand","radius":3.25,"points":[[4.25,3.5]],"coverage":coverage},
                        {"mode":"restore","radius":1.75,"points":[[4.25,3.5]]}
                    ]});
                    if !after_style {
                        settings["strokes"][2]["selection"] = serde_json::json!([0, 0, 9, 7]);
                    }
                    style.settings = settings.into();
                }
                doc.layers[0].effects.push(style);
                if kind.starts_with("liquify") {
                    doc.layers[0].effects.push(effect("stroke", 0.59));
                }
                doc.layers[0].effects.push(effect("blur", 0.63));
                doc.layers[1].effects.push(effect("outer_glow", 0.47));
                crate::adjustment::add(&mut doc, &serde_json::json!({"kind":"invert"})).unwrap();
                doc.layers[0].effects.push(effect("inner_shadow", 0.61));
                doc.layers[0].opacity = 0.37;
                doc.filters.push(effect("blur", 0.43));
                let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
                let staged = CapturedScene::from_parts(
                    doc.clone(),
                    "stored-style-test".into(),
                    vec![],
                    None,
                    None,
                )
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
                let scene = Arc::new(staged.scene);
                let session = Arc::new(staged.session.unwrap());
                let baseline = runtime.disk().used().unwrap();
                let topology = Topology::new(&scene).unwrap();
                assert!(Stored::needed(&topology));
                let stored = Stored::prepare(topology.clone(), &session, &mut || Ok(())).unwrap();
                let region = Region::new([-2, -1, 11, 9]).unwrap();
                {
                    let window = stored.window(region, &mut || Ok(())).unwrap();
                    if depth == 8 {
                        let masks: Vec<_> = doc
                            .layers
                            .iter()
                            .map(|layer| {
                                layer.mask.as_ref().and_then(|mask| {
                                    mask.prepare(layer.pixels.width, layer.pixels.height)
                                })
                            })
                            .collect();
                        let colors = crate::effects::prepare(&doc, &masks).unwrap();
                        let legacy = crate::compositor::Plan::new(&doc, &masks, &colors);
                        let native =
                            Plan::<Native8, _>::with_topology(topology.clone(), &window, region)
                                .unwrap();
                        for y in -1..9 {
                            for x in -2..11 {
                                assert_eq!(
                                    native.pixel(x, y).unwrap(),
                                    legacy.sample(legacy.group(None), x, y),
                                    "{depth} {kind} {x},{y}"
                                );
                                for i in 0..doc.layers.len() {
                                    assert_eq!(
                                        native.layer(i, x, y).unwrap(),
                                        legacy.layer(i, x, y),
                                        "{depth} {kind} layer{i} {x},{y}"
                                    );
                                }
                            }
                        }
                    } else {
                        let legacy = crate::depth16::Plan16::new(&doc).unwrap();
                        let native =
                            Plan::<Native16, _>::with_topology(topology.clone(), &window, region)
                                .unwrap();
                        for y in -1..9 {
                            for x in -2..11 {
                                assert_eq!(
                                    native.pixel(x, y).unwrap(),
                                    legacy.pixel(x, y),
                                    "{depth} {kind} {x},{y}"
                                );
                                for i in 0..doc.layers.len() {
                                    assert_eq!(
                                        native.layer(i, x, y).unwrap(),
                                        legacy.layer(i, x, y),
                                        "{depth} {kind} layer{i} {x},{y}"
                                    );
                                }
                            }
                        }
                    }
                }
                {
                    use crate::{
                        production_export::{native_rows, Format, Output, Resampling},
                        psd::{
                            native_rows36::Rows36,
                            standard36::{NativeRows, RowRequest, RowSource},
                        },
                    };
                    let filtered8 = if depth == 8 {
                        Some(crate::filters::prepare8(&doc).unwrap())
                    } else {
                        None
                    };
                    let filtered16 = if depth == 16 {
                        Some(crate::filters::prepare16(&doc).unwrap())
                    } else {
                        None
                    };
                    let expected = |x, y| {
                        if let Some(image) = &filtered16 {
                            image.get(x, y)
                        } else {
                            filtered8.as_ref().unwrap().get(x, y).map(u16::from)
                        }
                    };
                    let output = Output {
                        target: None,
                        target_name: String::new(),
                        variant: String::new(),
                        filename: "style.png".into(),
                        document_rect: [0, 0, 9, 7],
                        width: 9,
                        height: 7,
                        format: Format::Png,
                        native_source_depth: depth,
                        bit_depth: depth,
                        scale_xy: [1., 1.],
                    };
                    let rows =
                        native_rows::Rows::with_prepared(&scene, None, &stored, &|| Ok(()))
                            .unwrap();
                    let mut bytes = vec![0; 9 * 7 * usize::from(depth / 2)];
                    rows.fill(
                        &output,
                        depth,
                        Resampling::Nearest,
                        0,
                        7,
                        &mut bytes,
                        &|| Ok(()),
                    )
                    .unwrap();
                    for y in 0..7 {
                        for x in 0..9 {
                            let stride = usize::from(depth / 2);
                            let at = (y * 9 + x) as usize * stride;
                            let actual: Vec<u16> = if depth == 8 {
                                bytes[at..at + 4].iter().map(|v| u16::from(*v)).collect()
                            } else {
                                bytes[at..at + 8]
                                    .chunks_exact(2)
                                    .map(|v| u16::from_be_bytes(v.try_into().unwrap()))
                                    .collect()
                            };
                            assert_eq!(actual, expected(x, y), "{depth} {kind} export{x},{y}");
                        }
                    }
                    let legacy8 = if depth == 8 {
                        let masks: Vec<_> = doc
                            .layers
                            .iter()
                            .map(|layer| {
                                layer.mask.as_ref().and_then(|mask| {
                                    mask.prepare(layer.pixels.width, layer.pixels.height)
                                })
                            })
                            .collect();
                        Some(crate::effects::prepare(&doc, &masks).unwrap())
                    } else {
                        None
                    };
                    let legacy16 = if depth == 16 {
                        Some(crate::depth16::prepare(&doc).unwrap())
                    } else {
                        None
                    };
                    let mut psd = Rows36::with_prepared(&scene, &stored).unwrap();
                    for source in [RowSource::Layer(1), RowSource::BakedComposite] {
                        for channel in [-1, 0, 1, 2] {
                            let request = RowRequest {
                                provenance: psd.provenance(),
                                source,
                                channel,
                                bounds: [0, 0, 9, 7],
                                first_row: 0,
                                rows: 7,
                                depth,
                            };
                            let mut bytes = vec![0; 9 * 7 * usize::from(depth / 8)];
                            psd.fill(request, &mut bytes, &|| Ok(())).unwrap();
                            let c = if channel == -1 { 3 } else { channel as usize };
                            for y in 0..7 {
                                for x in 0..9 {
                                    let at = (y * 9 + x) as usize * usize::from(depth / 8);
                                    let actual = if depth == 8 {
                                        u16::from(bytes[at])
                                    } else {
                                        u16::from_be_bytes(bytes[at..at + 2].try_into().unwrap())
                                    };
                                    let wanted = if source == RowSource::BakedComposite {
                                        expected(x, y)[c]
                                    } else if let Some(colors) = &legacy16 {
                                        colors[1].as_ref().unwrap().get(x, y)[c]
                                    } else {
                                        u16::from(
                                            legacy8.as_ref().unwrap()[1]
                                                .as_ref()
                                                .unwrap()
                                                .get(x, y)[c],
                                        )
                                    };
                                    assert_eq!(
                                        actual, wanted,
                                        "{depth} {kind} PSD {source:?} channel{channel} {x},{y}"
                                    );
                                }
                            }
                        }
                    }
                }
                stored.retire(&mut || Ok(())).unwrap();
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                for target in [
                    None,
                    Some(doc.layers[0].id.as_str()),
                    Some(doc.layers[1].id.as_str()),
                ] {
                    for mask in [false, true] {
                        let request = Request::capture(
                            scene.clone(),
                            session.clone(),
                            Some([1, 1, 8, 6]),
                            5,
                            target,
                            mask,
                        )
                        .unwrap();
                        let frame = request.render(&mut || Ok(())).unwrap();
                        assert_eq!(
                            &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                            &doc.preview(Some([1, 1, 8, 6]), 5, target, mask).unwrap().2,
                            "{depth} {kind} {target:?} mask{mask}"
                        );
                        assert_eq!(runtime.disk().used().unwrap(), baseline);
                    }
                }
                let request =
                    Request::capture(scene.clone(), session.clone(), None, 48, None, false)
                        .unwrap();
                let mut checks = 0;
                assert!(request
                    .render(&mut || {
                        checks += 1;
                        if checks > 1000 {
                            Err("stored preview cancelled".into())
                        } else {
                            Ok(())
                        }
                    })
                    .is_err());
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                assert!(runtime
                    .inventory
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .staging
                    .is_empty());
            }
        }
    }
    #[test]
    fn stored_weighted_liquify_keeps_native_samples_across_tiles_and_output_bands() {
        use crate::{
            effects::{regional, stored, Effect},
            raster::backed::Region,
        };
        for depth in [8, 16] {
            let f = Fixture::new();
            let (_shared, connection) = f.connection();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let mut limits = limits();
            limits.resident_bytes = 96 * 1024 * 1024;
            limits.disk_bytes = 32 * 1024 * 1024;
            let runtime = SourceRuntime::new(context, limits).unwrap();
            let session = runtime.begin().unwrap();
            let baseline = (runtime.resident().used(), runtime.disk().used().unwrap());
            let size = [769, 521];
            let effect = Effect {
                id: crate::engine::id(),
                kind: "liquify".into(),
                enabled: true,
                weight: 0.67,
                settings: serde_json::json!({"radius":47.25,"strength":0.85,"strokes":[
                    {"mode":"push","points":[[231.25,158.5],[282.75,185.25],[517.5,348.75]]},
                    {"mode":"pinch","radius":61.5,"points":[[511.5,341.25]]}
                ]})
                .into(),
            };
            let pixel = |x: i32, y: i32| {
                let value = [
                    ((x * 739 + y * 811 + 12347) % 65536) as u16,
                    ((x * 509 + y * 317 + 33559) % 65536) as u16,
                    51237,
                    if (x + y) % 9 == 0 {
                        0
                    } else {
                        ((x * 137 + y * 797 + 719) % 65536) as u16
                    },
                ];
                if depth == 8 {
                    value.map(|v| u16::from(crate::raster::project16(v)) * 257)
                } else {
                    value
                }
            };
            let mappings = std::cell::Cell::new(0);
            let prepare = |crop| stored::stack(
                &session,
                [81; 32],
                stored::Frame {
                    size,
                    origin: [-3, 2],
                },
                size,
                depth,
                std::slice::from_ref(&effect),
                crop,
                |index, frame, check| {
                    assert_eq!(index, 0);
                    mappings.set(mappings.get() + 1);
                    crate::liquify::regional::Plan::new(
                        frame,
                        &effect.settings,
                        &[None; 2],
                        &runtime.resident(),
                        check,
                    )?
                    .prepare(check, |_, _| {
                        panic!("Unclipped fixture must not request a clip")
                    })
                },
                &mut || Ok(()),
                |region, check| {
                    let bytes = crate::render::region::native(
                        region,
                        depth,
                        &runtime.resident(),
                        check,
                        |x, y| Ok(pixel(x, y)),
                    )?;
                    regional::RegionalImage::from_native(region, depth, bytes)
                },
            );
            let image = prepare(None).unwrap();
            assert_eq!(mappings.get(), 1);
            assert_eq!(
                image.frame(),
                stored::Frame {
                    size,
                    origin: [-3, 2]
                }
            );
            let mut eight = crate::effects::Image {
                width: size[0],
                height: size[1],
                bytes: Vec::new(),
            };
            let mut sixteen = crate::depth16::Image16 {
                width: size[0],
                height: size[1],
                words: Vec::new(),
            };
            for y in 0..size[1] as i32 {
                for x in 0..size[0] as i32 {
                    if depth == 8 {
                        eight
                            .bytes
                            .extend(pixel(x, y).map(crate::raster::project16));
                    } else {
                        sixteen.words.extend(pixel(x, y));
                    }
                }
            }
            if depth == 8 {
                crate::effects::apply_cpu(&mut eight, "liquify", &effect.settings).unwrap();
            } else {
                crate::depth16::color::apply_cpu(&mut sixteen, "liquify", &effect.settings)
                    .unwrap();
            }
            for bounds in [[249, 165, 521, 349], [0, 0, 1, 521], [512, 510, 769, 521]] {
                let actual = image
                    .read(&session, Region::new(bounds).unwrap(), &mut || Ok(()))
                    .unwrap();
                let crop = [bounds[0] - 3, bounds[1] + 2, bounds[2] - 3, bounds[3] + 2];
                let cropped = prepare(Some(crop)).unwrap();
                assert_eq!(cropped.frame().origin, [crop[0], crop[1]]);
                assert_eq!(cropped.frame().size, [(crop[2] - crop[0]) as u32, (crop[3] - crop[1]) as u32]);
                let window = cropped.read_document(&session, Region::new(crop).unwrap(), &mut || Ok(())).unwrap();
                assert_eq!(window.bytes(), actual.bytes());
                drop(window);
                cropped.retire(&mut || Ok(())).unwrap();
                for y in bounds[1]..bounds[3] {
                    for x in bounds[0]..bounds[2] {
                        let expected = if depth == 16 {
                            crate::effects::weighted_pixel(
                                pixel(x, y),
                                sixteen.get(x, y),
                                effect.weight,
                            )
                        } else {
                            crate::effects::weighted_pixel(
                                pixel(x, y).map(|v| u16::from(crate::raster::project16(v))),
                                eight.get(x, y).map(u16::from),
                                effect.weight,
                            )
                            .map(|v| v * 257)
                        };
                        assert_eq!(actual.pixel16(x, y).unwrap(), expected, "{depth} {x},{y}");
                    }
                }
            }
            image.retire(&mut || Ok(())).unwrap();
            assert_eq!(
                (runtime.resident().used(), runtime.disk().used().unwrap()),
                baseline
            );
            assert!(runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .staging
                .is_empty());
        }
    }
    #[test]
    fn session_style_fields_match_full_origin_and_read_only_the_needed_prefix() {
        use crate::{
            effects::{field, regional::RegionalImage},
            large_document::ScratchBytes,
            raster::backed::{Region, MAX_REGION_PIXELS},
        };
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let foreign = runtime.begin().unwrap();
        let frame = [129, 1031];
        let initial_disk = runtime.disk().used().unwrap();
        let initial_memory = runtime.resident().used();
        for depth in [8, 16] {
            let source = |region: Region| -> Result<RegionalImage, String> {
                let length = region.pixels() * usize::from(depth / 2);
                let mut bytes = ScratchBytes::new(&runtime.resident(), length, length)
                    .map_err(|e| e.to_string())?;
                let [l, t, r, b] = region.bounds();
                let mut at = 0;
                for y in t..b {
                    for x in l..r {
                        let alpha = if (x == 11 && y == 9) || (x == 119 && y == 1027) {
                            1u16
                        } else if (96..118).contains(&x) && (596..632).contains(&y) {
                            let range = if depth == 16 { 65535 } else { 255 };
                            ((x * 739 + y * 811) % range + 1) as u16
                        } else {
                            0
                        };
                        if depth == 16 {
                            for value in [12347, 33559, 51237, alpha] {
                                bytes[at..at + 2].copy_from_slice(&value.to_be_bytes());
                                at += 2;
                            }
                        } else {
                            bytes[at..at + 4].copy_from_slice(&[37, 91, 173, alpha as u8]);
                            at += 4;
                        }
                    }
                }
                RegionalImage::from_native(region, depth, bytes.freeze())
            };
            let mut reads = 0;
            let prepared = field::prepare_session(
                &session,
                [73; 32],
                frame,
                [0, 0, frame[0] as i32, frame[1] as i32],
                &[3, 3, 3],
                depth,
                &mut || Ok(()),
                |region, _| {
                    reads += 1;
                    source(region)
                },
            )
            .unwrap();
            let full_disk = runtime.disk().used().unwrap();
            assert_eq!(
                reads,
                (frame[1] as usize).div_ceil(MAX_REGION_PIXELS / frame[0] as usize)
            );
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
            for bounds in [[0, 0, 29, 31], [100, 1000, 129, 1031], [11, 9, 120, 10]] {
                let actual = prepared
                    .tile(&session, [73; 32], bounds, &mut || Ok(()))
                    .unwrap();
                let expected = field::prepare(
                    frame,
                    bounds,
                    &[3, 3, 3],
                    depth,
                    &runtime.resident(),
                    &mut || Ok(()),
                    source,
                )
                .unwrap();
                for y in bounds[1]..bounds[3] {
                    for x in bounds[0]..bounds[2] {
                        assert_eq!(
                            actual.value(x, y).unwrap().to_bits(),
                            expected.value(x, y).unwrap().to_bits()
                        );
                    }
                }
            }
            assert!(prepared
                .tile(&foreign, [73; 32], [0, 0, 1, 1], &mut || Ok(()))
                .is_err());
            assert!(prepared
                .tile(&session, [74; 32], [0, 0, 1, 1], &mut || Ok(()))
                .is_err());
            assert_eq!(
                prepared
                    .tile(&session, [73; 32], [0, 0, 1, 1], &mut || Err(
                        "style read cancelled".into()
                    ))
                    .err()
                    .unwrap(),
                "style read cancelled"
            );
            let crop = [97, 598, 119, 631];
            let mut read_pixels = 0;
            let cropped = field::prepare_session(
                &session, [75; 32], frame, crop, &[3, 3, 3], depth,
                &mut || Ok(()), |region, _| {
                    let [left, top, right, bottom] = region.bounds();
                    assert_eq!([left, right], [0, 128]);
                    assert!(top >= 0 && bottom <= 640);
                    read_pixels += region.pixels();
                    source(region)
                },
            ).unwrap();
            assert_eq!(read_pixels, 128 * 640);
            let cropped_bytes = runtime.disk().used().unwrap() - full_disk;
            assert!(cropped_bytes > 0 && cropped_bytes < full_disk - initial_disk);
            let bounds = [100, 600, 118, 631];
            let actual = cropped.tile(&session, [75; 32], bounds, &mut || Ok(())).unwrap();
            let expected = prepared.tile(&session, [73; 32], bounds, &mut || Ok(())).unwrap();
            assert_eq!(actual.frame(), frame);
            assert_eq!(actual.bounds(), bounds);
            for y in bounds[1]..bounds[3] {
                for x in bounds[0]..bounds[2] {
                    assert_eq!(actual.value(x, y).unwrap().to_bits(), expected.value(x, y).unwrap().to_bits());
                }
            }
            assert!(actual.value(108, 617).unwrap() > 0.);
            assert!(cropped.tile(&session, [75; 32], [96, 598, 99, 600], &mut || Ok(())).is_err());
            drop((actual, expected));
            cropped.retire(&mut || Ok(())).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), full_disk);
            prepared.retire(&mut || Ok(())).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), initial_disk);
            assert_eq!(runtime.resident().used(), initial_memory);
            let failed = field::prepare_session(
                &session,
                [73; 32],
                frame,
                crop,
                &[3, 3, 3],
                depth,
                &mut || Ok(()),
                |_, _| Err("style source cancelled".into()),
            );
            assert_eq!(failed.err().unwrap(), "style source cancelled");
            assert_eq!(runtime.disk().used().unwrap(), initial_disk);
            assert_eq!(runtime.resident().used(), initial_memory);
            let state = runtime.inventory.lock().unwrap();
            assert_eq!(state.as_ref().unwrap().directories, 0);
            assert!(state.as_ref().unwrap().staging.is_empty());
        }
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
    fn byte_wrapper_preserves_specific_callback_failure_and_retires_verified_spool() {
        let f = Fixture::new();
        let (_shared, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let baseline = runtime.disk().used().unwrap();
        let staged = session
            .byte_temporary(0, crate::large_document::ByteRole::LayerData)
            .unwrap();
        let error = staged.finish([1; 32], &mut || {
            Err("Captured source revision changed".into())
        });
        assert!(matches!(error, Err(ref message) if message == "Captured source revision changed"));
        assert_eq!(runtime.disk().used().unwrap(), baseline);
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
        let original = store.file_bytes_for_test().unwrap();
        drop(snapshot);
        drop(store);
        let charged = runtime.disk().used().unwrap();
        fs::rename(&path, path.with_file_name("retained.pbt")).unwrap();
        fs::write(&path, &original).unwrap();
        assert!(session.resolve_store(identity, selected).is_err());
        assert!(session.fresh_fragment().is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
        assert_eq!(runtime.disk().used().unwrap(), charged);
        drop(session);
        drop(runtime);
        assert_eq!(
            fs::read(path.with_file_name("retained.pbt")).unwrap(),
            original
        );
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
    #[test]
    #[ignore = "Internal bounded pipe-wait child, launched only by the retained-worker lifecycle test"]
    fn retained_codec_pipe_child_waits_for_owned_stdin_close() {
        use std::io::Read;
        let mut byte = [0];
        let result = std::io::stdin().read_exact(&mut byte);
        assert!(matches!(result,Err(error) if error.kind()==std::io::ErrorKind::UnexpectedEof));
    }
    #[test]
    fn autonomous_retained_codec_reaper_releases_actual_profile_and_instance_without_future_request(
    ) {
        use std::{
            process::Command,
            sync::mpsc,
            thread,
            time::{Duration, Instant},
        };
        crate::jpeg_xl::prepare_retained_test_reaper().unwrap();
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
        let baseline = workspace
            .lock()
            .unwrap()
            .file_operations
            .load(Ordering::Acquire);
        assert_eq!(baseline, 1);
        let retained = session.retain_for_child().unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--ignored",
            "--exact",
            "workspace::source_runtime::tests::retained_codec_pipe_child_waits_for_owned_stdin_close",
            "--nocapture",
            "--test-threads=1",
        ]);
        let mut child = crate::jpeg_xl::park_retained_test_child(command, retained).unwrap();
        assert!(!child.reaped());
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
        assert!(!child.reaped());
        assert_eq!(
            workspace
                .lock()
                .unwrap()
                .file_operations
                .load(Ordering::Acquire),
            baseline
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
        // No codec request follows this close. The autonomous reaper must perform
        // actual child/job/source-owner drops and wake the waiting Connection.
        child.close_child();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !child.reaped() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(2));
        }
        assert!(child.reaped());
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
