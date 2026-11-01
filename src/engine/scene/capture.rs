//! Current-limit renderer bridge. Resident Raster remains live authority; this
//! captured/staged scene is derived and grants no editable backing admission.
use super::*;
use crate::large_document::{BlockRef, ScratchBytes, Snapshot, Store, TileSpec};
use crate::raster::surface::TilePatch;
use std::{
    collections::BTreeMap,
    sync::{Mutex, Weak},
};

pub(crate) trait StageSession {
    fn check(&self) -> Result<()>;
    /// Existing implementations remain conservative until the concrete service
    /// supplies its retained-instance cleanup-only admission check.
    fn cleanup_check(&self) -> Result<()> {
        self.check()
    }
    fn abort_fragment(
        &self,
        _fragment: crate::large_document::FreshFragment,
        _cleanup: &dyn Fn() -> Result<()>,
    ) -> Result<()> {
        Err("Source service retirement is unavailable; source bytes remain charged".into())
    }
    fn store_for_lineage(&self, lineage: [u8; 16]) -> Result<Store>;
    fn seal_store(&self, store: &Store, blocks: &[BlockRef], metadata: &[u8]) -> Result<Snapshot>;
}
pub(crate) trait StageRuntime {
    type Session: StageSession;
    fn resident(&self) -> ResidentBudget;
    fn disk(&self) -> DiskBudget;
    fn begin(&self) -> Result<Self::Session>;
}

/// The renderer keeps the optional session until its LAST source I/O finishes;
/// it must not put that operation into an indefinitely retained scene cache.
pub(crate) struct StagedScene<S> {
    pub scene: SceneSnapshot,
    // Keep each exact committed prefix alive through the last source access.
    pub seals: Vec<Snapshot>,
    _seal_charge: Option<crate::large_document::Reservation>,
    pub session: Option<S>,
}

pub struct CapturedScene {
    document: Document,
    identity: SceneIdentity,
    leases: Vec<super::super::Lease>,
    file_version: Option<crate::file_safety::Version>,
    file_ownership: Option<Arc<crate::file_ownership::Ownership>>,
}
impl CapturedScene {
    pub(crate) fn from_parts(
        document: Document,
        project_id: String,
        leases: Vec<super::super::Lease>,
        file_version: Option<crate::file_safety::Version>,
        file_ownership: Option<Arc<crate::file_ownership::Ownership>>,
    ) -> Self {
        let identity = SceneIdentity {
            project_id,
            document_id: document.id.clone(),
            revision: document.revision,
        };
        Self {
            document,
            identity,
            leases,
            file_version,
            file_ownership,
        }
    }
    pub fn identity(&self) -> &SceneIdentity {
        &self.identity
    }

    /// Call only after releasing engine/root/registry locks. The supplied runtime
    /// owns the actual persistent-root and retained-connection admission policy.
    pub(crate) fn stage<R: StageRuntime>(
        self,
        runtime: &R,
        cache: &LegacyTileCache,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<StagedScene<R::Session>> {
        check()?;
        let resident = runtime.resident();
        let disk = runtime.disk();
        if !cache.budget.same_pool(&resident) {
            return Err("Scene cache uses a different resident pool".into());
        }
        // Conservative temporary/current-limit metadata admission. Native payloads
        // are never serialized by Document36. This is not total-app RAM accounting.
        let owner = MetadataCharge::reserve(&resident, 64 * 1024 * 1024, check)?;
        let mut capture = Collector {
            planes: vec![],
            resident: &resident,
            decoded: vec![],
        };
        let metadata = Document36::capture(&self.document, &mut capture)?;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(capture.planes.len())
            .map_err(|_| "Scene source roster allocation refused")?;
        let mut session = None;
        let mut lineage = [0; 16];
        let digest =
            ring::digest::digest(&ring::digest::SHA256, self.identity.project_id.as_bytes());
        lineage.copy_from_slice(&digest.as_ref()[..16]);
        for source in &capture.planes {
            check()?;
            let surface = match source {
                PlaneInput::Stored(surface) => {
                    if !surface.uses_budget(&resident) || !surface.uses_disk_budget(&disk) {
                        return Err("Scene source uses another admission pool".into());
                    }
                    if !surface.is_known_zero() && session.is_none() {
                        session = Some(runtime.begin()?);
                    }
                    surface.clone()
                }
                PlaneInput::Resident(raster) => {
                    if raster.tiles.is_empty() && raster.samples16.is_empty() {
                        Surface::zero(raster.width, raster.height, raster.depth)
                            .map_err(|e| e.to_string())?
                    } else {
                        if session.is_none() {
                            session = Some(runtime.begin()?);
                        }
                        cache.stage(raster, session.as_ref().unwrap(), lineage, &disk, check)?
                    }
                }
            };
            sources.push(surface);
        }
        capture
            .decoded
            .try_reserve(1)
            .map_err(|_| "Captured metadata owner allocation refused")?;
        capture.decoded.push(owner);
        // Transfer every existing metadata/coverage admission before temporary
        // native payloads drop. Contour DTOs keep their charge through snapshots.
        let owner = MetadataCharge::combine(&capture.decoded, check)?;
        drop(capture);
        if let Some(session) = &session {
            session.check()?;
        }
        check()?;
        let metadata = Admitted::from_parts(Arc::new(metadata), owner);
        let pending = PendingScene::new(metadata, RasterRoster::new(sources)?, resident, disk)?;
        let mut scene = SceneSnapshot::new(
            self.identity,
            pending,
            self.leases,
            self.file_version,
            self.file_ownership,
        )?;
        scene.runtime_views = RuntimeViews {
            structure: self.document.source_structure,
            patterns: self.document.source_patterns,
        };
        let (seals, seal_charge) = if let Some(session) = &session {
            let (seals, charge) = seal_sources(&scene, session, check)?;
            (seals, Some(charge))
        } else {
            (vec![], None)
        };
        check()?;
        Ok(StagedScene {
            scene,
            seals,
            _seal_charge: seal_charge,
            session,
        })
    }
}

fn seal_sources<S: StageSession>(
    scene: &SceneSnapshot,
    session: &S,
    check: &dyn Fn() -> Result<()>,
) -> Result<(Vec<Snapshot>, crate::large_document::Reservation)> {
    // Current-limit derived bridge: all descriptor copies are admitted before
    // building the per-store tables. This does not evict resident live authority.
    let count = scene.rasters().iter().try_fold(0usize, |n, (_, surface)| {
        n.checked_add(surface.physical_tiles().count())
            .filter(|n| *n <= 65536)
            .ok_or_else(|| "Derived scene has too many physical tile references".to_string())
    })?;
    let bytes = count
        .checked_mul(std::mem::size_of::<BlockRef>() + 256)
        .and_then(|n| n.checked_add(128))
        .ok_or("Derived seal metadata overflow")?;
    let charge = scene
        .budget()
        .reserve_bytes(bytes, bytes)
        .map_err(|e| e.to_string())?;
    let mut stores: BTreeMap<[u8; 16], (Store, Vec<BlockRef>)> = BTreeMap::new();
    for (_, surface) in scene.rasters().iter() {
        for tile in surface.physical_tiles() {
            check()?;
            session.check()?;
            let entry = stores
                .entry(tile.owner().identity())
                .or_insert_with(|| (tile.owner().clone(), vec![]));
            entry
                .1
                .try_reserve_exact(1)
                .map_err(|_| "Derived seal reference allocation refused")?;
            entry.1.push(tile.reference());
        }
    }
    let mut seals = Vec::new();
    seals
        .try_reserve_exact(stores.len())
        .map_err(|_| "Derived checkpoint allocation refused")?;
    let key = scene.render_key()?;
    for (_, (store, mut blocks)) in stores {
        session.check()?;
        check()?;
        blocks.sort_unstable_by_key(|b| b.digest());
        blocks.dedup();
        seals.push(session.seal_store(&store, &blocks, &key)?);
    }
    session.check()?;
    check()?;
    Ok((seals, charge))
}

enum PlaneInput {
    Resident(Raster),
    Stored(Surface),
}
struct Collector<'a> {
    planes: Vec<PlaneInput>,
    resident: &'a ResidentBudget,
    decoded: Vec<Arc<MetadataCharge>>,
}
impl Collector<'_> {
    fn next(&self) -> Result<u32> {
        if self.planes.len() >= MAX_RASTER_LOCATIONS {
            return Err("Too many native source locations".into());
        }
        Ok(self.planes.len() as u32 + 1)
    }
}
impl RasterCollector for Collector<'_> {
    fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
        raster.validate_layout()?;
        let id = self.next()?;
        self.planes
            .try_reserve(1)
            .map_err(|_| "Scene source handle allocation refused")?;
        self.planes.push(PlaneInput::Resident(Raster {
            width: raster.width,
            height: raster.height,
            depth: raster.depth,
            tiles: raster.tiles.clone(),
            samples16: raster.samples16.clone(),
            retained: None,
        }));
        let retained = raster
            .retained
            .as_ref()
            .map(|original| -> Result<Box<Retained36>> {
                Ok(Box::new(Retained36 {
                    pixels: self.resident(&original.pixels)?,
                    matrix: original.matrix,
                    projective: original.projective,
                    warps: original.warps.iter().map(Warp36::capture).collect(),
                }))
            })
            .transpose()?;
        Ok(RasterRef36 {
            id,
            width: raster.width,
            height: raster.height,
            depth: raster.depth,
            retained,
        })
    }
    fn surface(&mut self, surface: &Surface) -> Result<RasterRef36> {
        let id = self.next()?;
        self.planes
            .try_reserve(1)
            .map_err(|_| "Scene source handle allocation refused")?;
        self.planes.push(PlaneInput::Stored(surface.clone()));
        // Uniform OriginalSurface support is added with the actual field migration.
        Ok(RasterRef36 {
            id,
            width: surface.width(),
            height: surface.height(),
            depth: surface.depth(),
            retained: None,
        })
    }
    fn embedded_coverage(&mut self, value: &Value) -> Result<Coverage36> {
        let estimate = legacy_coverage_bytes(value)?;
        let owner = MetadataCharge::reserve(self.resident, estimate, &|| Ok(()))?;
        let coverage = crate::selection::Coverage::deserialize(value)
            .map_err(|_| "Invalid captured Liquify coverage")?;
        coverage.validate()?;
        // Keep the decoded buffers' owner until all captured Arc payloads drop.
        self.decoded.push(owner);
        Coverage36::from_capture(&coverage, self)
    }
}

fn legacy_coverage_bytes(value: &Value) -> Result<usize> {
    let object = value
        .as_object()
        .ok_or("Invalid captured coverage metadata")?;
    if object
        .keys()
        .any(|k| !matches!(k.as_str(), "mask" | "bounds" | "origin" | "contours"))
    {
        return Err("Unsupported captured coverage fields".into());
    }
    fn raster_bytes(value: &Value, nested: bool) -> Result<usize> {
        let width = value["width"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("Invalid legacy coverage width")?;
        let height = value["height"]
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or("Invalid legacy coverage height")?;
        crate::raster::check_size(width, height)?;
        if value.get("depth").and_then(Value::as_u64).unwrap_or(8) != 8
            || value
                .get("samples16")
                .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err("Captured coverage must retain its native scalar depth".into());
        }
        let count = value["tiles"]
            .as_array()
            .filter(|a| a.len() <= 4096)
            .ok_or("Invalid bounded legacy coverage tiles")?
            .len();
        let mut bytes = count
            .checked_mul(256 * 256 * 4 + 1024)
            .ok_or("Legacy coverage allocation overflow")?;
        if let Some(original) = value.get("retained").filter(|v| !v.is_null()) {
            if nested {
                return Err("Nested captured originals are unsupported".into());
            }
            bytes = bytes
                .checked_add(raster_bytes(&original["pixels"], true)?)
                .ok_or("Legacy coverage allocation overflow")?;
        }
        Ok(bytes)
    }
    let contours = value["contours"]
        .as_array()
        .filter(|a| a.len() <= 500_000)
        .ok_or("Invalid bounded captured contours")?;
    let points = contours.iter().try_fold(0usize, |sum, row| {
        sum.checked_add(row.as_array().ok_or("Invalid captured contour")?.len())
            .ok_or("Captured contour count overflow")
    })?;
    if points > 500_000 {
        return Err("Captured contour vertices exceed their bound".into());
    }
    raster_bytes(&value["mask"], false)?
        .checked_add(points * 16 + contours.len() * 48 + 1024 * 1024)
        .ok_or_else(|| "Captured coverage allocation overflow".into())
}

enum Life {
    Eight(Weak<Vec<u8>>),
    Sixteen(Weak<Vec<u16>>),
}
struct CachedTile {
    life: Life,
    owner: Store,
    reference: BlockRef,
}
pub struct LegacyTileCache {
    entries: Mutex<BTreeMap<(u16, usize), CachedTile>>,
    _charge: crate::large_document::Reservation,
    budget: ResidentBudget,
    maximum: usize,
}
impl LegacyTileCache {
    pub fn new(budget: &ResidentBudget, maximum: usize) -> Result<Self> {
        if maximum == 0 || maximum > 65536 {
            return Err("Invalid legacy source cache bound".into());
        }
        let bytes = maximum
            .checked_mul(512)
            .ok_or("Legacy cache metadata overflow")?;
        let charge = budget
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        Ok(Self {
            entries: Mutex::new(BTreeMap::new()),
            _charge: charge,
            budget: budget.clone(),
            maximum,
        })
    }
    fn stage<S: StageSession>(
        &self,
        raster: &Raster,
        session: &S,
        lineage: [u8; 16],
        disk: &DiskBudget,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<Surface> {
        session.check()?;
        check()?;
        let spec = TileSpec::new(256, 256, raster.depth).map_err(|e| e.to_string())?;
        let count = if raster.depth == 16 {
            raster.samples16.len()
        } else {
            raster.tiles.len()
        };
        let bytes = count
            .checked_mul(std::mem::size_of::<TilePatch>() + 128)
            .ok_or("Legacy patch metadata overflow")?;
        let _provisional = self
            .budget
            .reserve_bytes(bytes, bytes)
            .map_err(|e| e.to_string())?;
        let mut patches = Vec::new();
        patches
            .try_reserve_exact(count)
            .map_err(|_| "Legacy patch metadata allocation refused")?;
        let mut store = None;
        macro_rules! tiles {
            ($map:expr,$variant:ident,$depth:expr) => {
                for (&coordinate, payload) in $map {
                    session.check()?;
                    check()?;
                    let key = ($depth, Arc::as_ptr(payload) as usize);
                    let cached = self.entries.try_lock().ok().and_then(|entries| {
                        entries.get(&key).and_then(|entry| match &entry.life {
                            Life::$variant(weak)
                                if weak
                                    .upgrade()
                                    .is_some_and(|held| Arc::ptr_eq(&held, payload)) =>
                            {
                                Some((entry.owner.clone(), entry.reference))
                            }
                            _ => None,
                        })
                    });
                    let (owner, reference) = if let Some(cached) = cached {
                        cached
                    } else {
                        if store.is_none() {
                            store = Some(session.store_for_lineage(lineage)?);
                        }
                        let target = store.as_ref().unwrap();
                        if !target.resident_budget().same_pool(&self.budget)
                            || !target.uses_disk_budget(disk)
                        {
                            return Err(
                                "Derived source store uses a different admission pool".into()
                            );
                        }
                        let reference = self.put_payload(target, spec, payload, session, check)?;
                        if let Ok(mut entries) = self.entries.try_lock() {
                            if entries.len() >= self.maximum && !entries.contains_key(&key) {
                                if let Some(first) = entries.keys().next().copied() {
                                    entries.remove(&first);
                                }
                            }
                            entries.insert(
                                key,
                                CachedTile {
                                    life: Life::$variant(Arc::downgrade(payload)),
                                    owner: target.clone(),
                                    reference,
                                },
                            );
                        }
                        (target.clone(), reference)
                    };
                    patches.push(TilePatch::stored(coordinate, owner, reference));
                }
            };
        }
        if raster.depth == 16 {
            tiles!(&raster.samples16, Sixteen, 16);
        } else {
            tiles!(&raster.tiles, Eight, 8);
        }
        session.check()?;
        check()?;
        Surface::zero(raster.width, raster.height, raster.depth)
            .map_err(|e| e.to_string())?
            .with_tile_patches(&patches, &self.budget, disk)
            .map_err(|e| e.to_string())
    }
    fn put_payload<T: NativePayload, S: StageSession>(
        &self,
        store: &Store,
        spec: TileSpec,
        payload: &Arc<Vec<T>>,
        session: &S,
        check: &dyn Fn() -> Result<()>,
    ) -> Result<BlockRef> {
        T::put(store, spec, payload, &self.budget, &mut || {
            session
                .check()
                .and_then(|_| check())
                .map_err(|_| crate::large_document::Error::Cancelled)
        })
        .map_err(|e| e.to_string())
    }
}
trait NativePayload: Sized {
    fn put(
        store: &Store,
        spec: TileSpec,
        data: &[Self],
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> crate::large_document::Result<BlockRef>;
}
impl NativePayload for u8 {
    fn put(
        store: &Store,
        spec: TileSpec,
        data: &[u8],
        _: &ResidentBudget,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> crate::large_document::Result<BlockRef> {
        store.put(spec, data, check)
    }
}
impl NativePayload for u16 {
    fn put(
        store: &Store,
        spec: TileSpec,
        data: &[u16],
        budget: &ResidentBudget,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> crate::large_document::Result<BlockRef> {
        let mut bytes = ScratchBytes::new(budget, spec.bytes(), spec.bytes())?;
        for (index, (&word, pair)) in data.iter().zip(bytes.chunks_exact_mut(2)).enumerate() {
            if index % 32768 == 0 {
                check()?;
            }
            pair.copy_from_slice(&word.to_be_bytes());
        }
        store.put(spec, &bytes, check)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Runtime(Arc<RuntimeInner>);
    struct RuntimeInner {
        root: std::path::PathBuf,
        resident: ResidentBudget,
        disk: DiskBudget,
        begins: AtomicUsize,
        sealed: AtomicUsize,
        stores: Mutex<BTreeMap<[u8; 16], (Store, Option<Snapshot>)>>,
    }
    struct Session(Arc<RuntimeInner>);
    impl Runtime {
        fn new() -> Self {
            let root = std::fs::canonicalize(std::env::temp_dir())
                .unwrap()
                .join(format!("peerbrush-captured-scene-{}", crate::engine::id()));
            std::fs::create_dir(&root).unwrap();
            Self(Arc::new(RuntimeInner {
                root,
                resident: ResidentBudget::new(192 * 1024 * 1024).unwrap(),
                disk: DiskBudget::new(64 * 1024 * 1024, 32).unwrap(),
                begins: AtomicUsize::new(0),
                sealed: AtomicUsize::new(0),
                stores: Mutex::new(BTreeMap::new()),
            }))
        }
    }
    impl Drop for RuntimeInner {
        fn drop(&mut self) {
            self.stores.get_mut().unwrap().clear();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
    impl StageRuntime for Runtime {
        type Session = Session;
        fn resident(&self) -> ResidentBudget {
            self.0.resident.clone()
        }
        fn disk(&self) -> DiskBudget {
            self.0.disk.clone()
        }
        fn begin(&self) -> Result<Session> {
            self.0.begins.fetch_add(1, Ordering::SeqCst);
            Ok(Session(self.0.clone()))
        }
    }
    impl StageSession for Session {
        fn check(&self) -> Result<()> {
            Ok(())
        }
        fn store_for_lineage(&self, lineage: [u8; 16]) -> Result<Store> {
            let mut stores = self.0.stores.lock().unwrap();
            if let Some((store, _)) = stores.get(&lineage) {
                return Ok(store.clone());
            }
            let store = Store::create_with_disk_budget(
                &self.0.root.join(crate::engine::id()),
                self.0.resident.clone(),
                self.0.disk.clone(),
                crate::large_document::Limits {
                    disk_bytes: 16 * 1024 * 1024,
                    max_blocks: 128,
                    max_resident_blocks: 16,
                    manifest_bytes: 1024 * 1024,
                },
            )
            .map_err(|e| e.to_string())?;
            stores.insert(lineage, (store.clone(), None));
            Ok(store)
        }
        fn seal_store(
            &self,
            store: &Store,
            blocks: &[BlockRef],
            metadata: &[u8],
        ) -> Result<Snapshot> {
            let mut stores = self.0.stores.lock().unwrap();
            let (_, head) = stores
                .values_mut()
                .find(|(s, _)| s.identity() == store.identity())
                .ok_or("Unregistered test store")?;
            let parent = head.as_ref().map(Snapshot::reference);
            let snapshot = store
                .commit(
                    store.identity(),
                    parent.map_or(1, |p| p.revision() + 1),
                    parent,
                    metadata,
                    blocks,
                    &mut || Ok(()),
                )
                .map_err(|e| e.to_string())?;
            *head = Some(snapshot.clone());
            self.0.sealed.fetch_add(1, Ordering::SeqCst);
            Ok(snapshot)
        }
    }
    fn capture(document: &Document) -> CapturedScene {
        CapturedScene::from_parts(
            document.clone(),
            "captured-test-project".into(),
            vec![],
            None,
            None,
        )
    }
    #[test]
    fn zero_only_capture_has_no_source_io_and_metadata_changes_change_render_identity() {
        let runtime = Runtime::new();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut doc = Document::new(8, 8).unwrap();
        let first = capture(&doc).stage(&runtime, &cache, &|| Ok(())).unwrap();
        assert!(first.session.is_none());
        assert!(first.seals.is_empty());
        assert_eq!(runtime.0.begins.load(Ordering::SeqCst), 0);
        let key = first.scene.render_key().unwrap();
        doc.layers[0].opacity = 0.5;
        let second = capture(&doc).stage(&runtime, &cache, &|| Ok(())).unwrap();
        assert_eq!(first.scene.identity(), second.scene.identity());
        assert_ne!(key, second.scene.render_key().unwrap());
        let first_metadata = first.scene.metadata();
        let first_layer = first_metadata.layers().next().unwrap();
        let first_slot = first_layer.pixels().slot();
        assert!(first.scene.raster(first_slot).unwrap().is_known_zero());
        assert!(second.scene.raster(first_slot).is_err());
        assert_eq!(runtime.0.disk.used().unwrap(), 0);
    }
    #[test]
    fn changed_cow_tiles_stage_once_preserve_old_scene_and_seal_every_physical_reference() {
        for depth in [8, 16] {
            let runtime = Runtime::new();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut doc = Document::new_depth(512, 256, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(0, 0, [12345, 23456, 34567, 65535]);
            doc.layers[0]
                .pixels
                .set16(256, 0, [23456, 34567, 45678, 65535]);
            let first = capture(&doc).stage(&runtime, &cache, &|| Ok(())).unwrap();
            let old = first.scene.rasters().iter().next().unwrap().1;
            let refs: Vec<_> = old.physical_tiles().map(|p| p.reference()).collect();
            doc.layers[0]
                .pixels
                .set16(256, 0, [34567, 45678, 56789, 65535]);
            let second = capture(&doc).stage(&runtime, &cache, &|| Ok(())).unwrap();
            let current = second.scene.rasters().iter().next().unwrap().1;
            let current_refs: Vec<_> = current.physical_tiles().map(|p| p.reference()).collect();
            assert_eq!(refs[0], current_refs[0]);
            assert_ne!(refs[1], current_refs[1]);
            assert_ne!(
                first.scene.render_key().unwrap(),
                second.scene.render_key().unwrap()
            );
            let mut committed = vec![];
            second.seals[0]
                .visit_blocks(&mut || Ok(()), |r| {
                    committed.push(r);
                    Ok(())
                })
                .unwrap();
            assert_eq!(committed.len(), 2);
            assert!(current_refs.iter().all(|r| committed.contains(r)));
            let before = old
                .prepare_region(
                    crate::raster::backed::Region::new([256, 0, 257, 1]).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            let after = current
                .prepare_region(
                    crate::raster::backed::Region::new([256, 0, 257, 1]).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            assert_ne!(
                before.pixel16(256, 0).unwrap(),
                after.pixel16(256, 0).unwrap()
            );
            drop(before);
            drop(after);
            drop(first);
            drop(second);
            drop(cache);
        }
    }
    #[test]
    fn retained_and_embedded_liquify_coverage_are_captured_without_inline_pixels() {
        let runtime = Runtime::new();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut doc = Document::new(8, 8).unwrap();
        doc.layers[0].pixels.set(1, 1, [5, 6, 7, 255]);
        doc.layers[0].pixels.retained = Some(Box::new(crate::retained::Original::from(
            &doc.layers[0].pixels,
        )));
        let mut mask = Raster::new(8, 8);
        mask.set(1, 1, [127; 4]);
        let coverage = crate::selection::Coverage {
            mask,
            bounds: [0, 0, 8, 8],
            origin: [0, 0],
            contours: vec![],
        };
        doc.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "liquify".into(),
            enabled: true,
            weight: 1.,
            settings: Arc::new(
                serde_json::json!({"strokes":[{"points":[[1,1],[2,2]],"coverage":coverage}]}),
            ),
        });
        let staged = capture(&doc).stage(&runtime, &cache, &|| Ok(())).unwrap();
        assert_eq!(staged.scene.rasters().len(), 3);
        assert!(staged
            .scene
            .metadata()
            .layers()
            .next()
            .unwrap()
            .pixels()
            .retained()
            .is_some());
        let text = serde_json::to_string(&staged.scene.metadata()).unwrap();
        assert!(!text.contains("\"tiles\""));
        assert!(!text.contains("samples16"));
        assert_eq!(
            staged
                .scene
                .metadata()
                .layers()
                .next()
                .unwrap()
                .effects()
                .next()
                .unwrap()
                .clips()
                .len(),
            1
        );
    }
    #[test]
    fn refusal_and_cancellation_never_return_a_partial_scene() {
        let runtime = Runtime::new();
        let foreign = ResidentBudget::new(1024 * 1024).unwrap();
        let cache = LegacyTileCache::new(&foreign, 4).unwrap();
        let doc = Document::new(8, 8).unwrap();
        assert!(capture(&doc).stage(&runtime, &cache, &|| Ok(())).is_err());
        let cache = LegacyTileCache::new(&runtime.resident(), 4).unwrap();
        assert!(capture(&doc)
            .stage(&runtime, &cache, &|| Err("cancelled".into()))
            .is_err());
        assert_eq!(runtime.0.begins.load(Ordering::SeqCst), 0);
    }
    #[test]
    fn cache_contention_and_wrong_weak_owner_do_not_substitute_or_block_source_bytes() {
        let runtime = Runtime::new();
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let mut doc = Document::new(8, 8).unwrap();
        doc.layers[0].pixels.set(0, 0, [11, 22, 33, 255]);
        let session = runtime.begin().unwrap();
        let store = session.store_for_lineage([9; 16]).unwrap();
        let impostor = Arc::new(vec![77u8; 256 * 256 * 4]);
        let reference = store
            .put(TileSpec::new(256, 256, 8).unwrap(), &impostor, &mut || {
                Ok(())
            })
            .unwrap();
        let payload = doc.layers[0].pixels.tiles.values().next().unwrap();
        cache.entries.lock().unwrap().insert(
            (8, Arc::as_ptr(payload) as usize),
            CachedTile {
                life: Life::Eight(Arc::downgrade(&impostor)),
                owner: store,
                reference,
            },
        );
        let first = capture(&doc).stage(&runtime, &cache, &|| Ok(())).unwrap();
        let surface = first.scene.rasters().iter().next().unwrap().1;
        let view = surface
            .prepare_region(
                crate::raster::backed::Region::new([0, 0, 1, 1]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(view.pixel(0, 0).unwrap(), [11, 22, 33, 255]);
        drop(view);
        let guard = cache.entries.lock().unwrap();
        let second = capture(&doc).stage(&runtime, &cache, &|| Ok(())).unwrap();
        assert_eq!(
            first.scene.render_key().unwrap(),
            second.scene.render_key().unwrap()
        );
        drop(guard);
    }
    #[test]
    fn late_cancellation_after_durable_seal_refuses_scene_publication() {
        let runtime = Runtime::new();
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let mut doc = Document::new(8, 8).unwrap();
        doc.layers[0].pixels.set(0, 0, [11, 22, 33, 255]);
        let result = capture(&doc).stage(&runtime, &cache, &|| {
            if runtime.0.sealed.load(Ordering::SeqCst) > 0 {
                Err("late cancellation".into())
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(runtime.0.sealed.load(Ordering::SeqCst), 1);
        assert!(runtime.disk().used().unwrap() > 0);
        // An unreturned candidate did not alter the captured resident authority.
        assert_eq!(doc.revision, 0);
        assert_eq!(doc.layers[0].pixels.get(0, 0), [11, 22, 33, 255]);
    }
}
