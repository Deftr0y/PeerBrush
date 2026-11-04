use super::*;
use crate::{
    engine::{
        scene::{
            Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster, Retained36,
            SceneIdentity, StageSession, Warp36,
        },
        Document,
    },
    large_document::{BlockRef, DiskBudget, FreshFragment, Limits, ResidentBudget, Snapshot},
    psd::memory36::{Admitted, MetadataCharge},
    raster::Raster,
};
use std::{cell::Cell, path::PathBuf};

struct Collector {
    sources: Vec<Surface>,
    store: Store,
    pool: ResidentBudget,
}
impl RasterCollector for Collector {
    fn resident(&mut self, r: &Raster) -> Result<RasterRef36, String> {
        let size = (r.tiles.len() + r.samples16.len()) * 512 + 4096;
        let index = self
            .pool
            .reserve_bytes(size, size)
            .map_err(|e| e.to_string())?;
        let surface = Surface::stage_current_pixels(r, self.store.clone(), &mut || Ok(()))
            .map_err(|e| e.to_string())?;
        drop(index);
        self.sources.push(surface);
        let id = self.sources.len() as u32;
        let retained = r
            .retained
            .as_ref()
            .map(|o| {
                Ok::<_, String>(Box::new(Retained36 {
                    pixels: self.resident(&o.pixels)?,
                    matrix: o.matrix,
                    projective: o.projective,
                    warps: o.warps.iter().map(Warp36::capture).collect(),
                }))
            })
            .transpose()?;
        Ok(RasterRef36 {
            id,
            width: r.width,
            height: r.height,
            depth: r.depth,
            retained,
        })
    }
    fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
        Err("Unexpected clipboard fixture Surface".into())
    }
}
fn limits() -> Limits {
    Limits {
        disk_bytes: 32 * 1024 * 1024,
        max_blocks: 256,
        max_resident_blocks: 16,
        manifest_bytes: 1024 * 1024,
    }
}
struct TestSession {
    pool: ResidentBudget,
    disk: DiskBudget,
    parent: PathBuf,
    created: Cell<usize>,
}
impl StageSession for TestSession {
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
    fn cleanup_check(&self) -> Result<(), String> {
        Ok(())
    }
    fn store_for_lineage(&self, _: [u8; 16]) -> Result<Store, String> {
        Err("No fixture lineage resolver".into())
    }
    fn seal_store(
        &self,
        store: &Store,
        refs: &[BlockRef],
        metadata: &[u8],
    ) -> Result<Snapshot, String> {
        let parent = store.checkpoint_reference().map_err(|e| e.to_string())?;
        store
            .commit(
                parent.map_or(store.identity(), |p| p.source()),
                parent.map_or(1, |p| p.revision() + 1),
                parent,
                metadata,
                refs,
                &mut || Ok(()),
            )
            .map_err(|e| e.to_string())
    }
    fn abort_fragment(
        &self,
        fragment: FreshFragment,
        cleanup: &dyn Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        fragment
            .abort(&mut || cleanup().map_err(|_| Error::Cancelled))
            .map_err(|e| e.to_string())
    }
}
impl Session for TestSession {
    fn resident(&self) -> ResidentBudget {
        self.pool.clone()
    }
    fn disk(&self) -> DiskBudget {
        self.disk.clone()
    }
    fn fresh(&self) -> Result<FreshFragment, String> {
        let n = self.created.get() + 1;
        self.created.set(n);
        FreshFragment::create(
            &self
                .parent
                .join(format!("copy-{n}-{}", crate::engine::id())),
            self.pool.clone(),
            self.disk.clone(),
            limits(),
        )
        .map_err(|e| e.to_string())
    }
}
struct Fixture {
    pool: ResidentBudget,
    disk: DiskBudget,
    parent: PathBuf,
    input: FreshFragment,
    document: NativeDocument,
    scene: SceneSnapshot,
    legacy: Document,
}
impl Fixture {
    fn new(legacy: Document) -> Self {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let disk = DiskBudget::new(64 * 1024 * 1024, 32).unwrap();
        let parent = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "peerbrush-native-clipboard-{}",
                crate::engine::id()
            ));
        std::fs::create_dir(&parent).unwrap();
        let input =
            FreshFragment::create(&parent.join("input"), pool.clone(), disk.clone(), limits())
                .unwrap();
        let ticket = MetadataCharge::reserve(&pool, 2 * 1024 * 1024, &|| Ok(())).unwrap();
        let mut collector = Collector {
            sources: vec![],
            store: input.store().clone(),
            pool: pool.clone(),
        };
        let metadata = Document36::capture(&legacy, &mut collector).unwrap();
        let document = NativeDocument::from_pending(
            PendingScene::new(
                Admitted::from_parts(Arc::new(metadata), ticket),
                RasterRoster::new(collector.sources).unwrap(),
                pool.clone(),
                disk.clone(),
            )
            .unwrap(),
        )
        .unwrap();
        let scene = SceneSnapshot::from_native(
            SceneIdentity {
                project_id: "clipboard-component".into(),
                document_id: legacy.id.clone(),
                revision: legacy.revision,
            },
            document.clone(),
            vec![],
            None,
            None,
        )
        .unwrap();
        Self {
            pool,
            disk,
            parent,
            input,
            document,
            scene,
            legacy,
        }
    }
    fn session(&self) -> TestSession {
        TestSession {
            pool: self.pool.clone(),
            disk: self.disk.clone(),
            parent: self.parent.clone(),
            created: Cell::new(0),
        }
    }
    fn source_hash(&self) -> Vec<u8> {
        // Read through the held authority: Windows deliberately locks the Store
        // against independent named-file reads. This binds every actual native
        // payload, including retained sources, hidden channels and tile padding.
        let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
        self.document
            .metadata()
            .visit_rasters(|raster| {
                let source = self.document.source(raster.slot())?;
                hash.update(&source.key().bytes());
                for tile in source.physical_tiles() {
                    hash.update(&tile.coordinate().0.to_be_bytes());
                    hash.update(&tile.coordinate().1.to_be_bytes());
                    hash.update(&tile.reference().fingerprint());
                    let bytes = source
                        .prepare_tile(tile.coordinate(), &mut || Ok(()))
                        .map_err(|e| e.to_string())?
                        .copy_to_scratch(&self.pool)
                        .map_err(|e| e.to_string())?;
                    hash.update(&bytes);
                }
                Ok(())
            })
            .unwrap();
        hash.finish().as_ref().to_vec()
    }
    fn finish(self) {
        let Self {
            pool,
            disk,
            parent,
            input,
            document,
            scene,
            legacy,
        } = self;
        drop(legacy);
        drop(scene);
        drop(document);
        input.abort(&mut || Ok(())).unwrap();
        assert_eq!(pool.used(), 0);
        assert_eq!(disk.used().unwrap(), 0);
        std::fs::remove_dir_all(parent).unwrap();
    }
}
fn doc(depth: u16, selection: u8) -> Document {
    let mut doc = Document::new_depth(17, 19, depth).unwrap();
    let target = &mut doc.layers[0];
    target.x = -2;
    target.y = 3;
    target.opacity = 0.37;
    target.pixels.set16(2, 3, [12345, 23456, 34567, 65003]);
    target.pixels.set16(5, 7, [12001, 23003, 34007, 21987]);
    target.pixels.set16(8, 1, [1001, 3007, 8009, 0]);
    let mut original = crate::retained::Original::from(&target.pixels);
    original.pixels.set16(12, 13, [65003, 127, 509, 65535]);
    target.pixels.retained = Some(Box::new(original));
    let mut behind = crate::engine::Layer::new("Behind", "fill", 17, 19);
    behind.pixels = Raster::new_depth(17, 19, depth);
    behind.color = [17, 83, 219, 127];
    behind.x = 1;
    behind.y = -1;
    behind.opacity = 0.43;
    behind.blend = "screen".into();
    doc.layers.push(behind);
    if selection > 0 {
        doc.selection = Some([-4, -3, 13, 15]);
    }
    if selection == 2 {
        doc.selection_polygon = Some(vec![[-4., -3.], [13., -3.], [2., 15.]]);
    }
    if selection == 3 {
        let mut mask = Raster::new(17, 19);
        for y in 0..19 {
            for x in 0..17 {
                let v = [0, 1, 64, 127, 128, 254, 255][(x + y * 3) as usize % 7];
                mask.set(x, y, [v, 7, 11, 255]);
            }
        }
        doc.selection_coverage = Some(crate::selection::Coverage {
            mask,
            bounds: doc.selection.unwrap(),
            origin: [1, -2],
            contours: vec![],
        });
    }
    doc
}
fn assert_copy(copy: &StagedCopy, expected: &crate::clipboard::Image, pool: &ResidentBudget) {
    assert_eq!(copy.origin(), expected.origin.unwrap());
    let source = copy.output.surface();
    assert_eq!(
        [source.width(), source.height()],
        [expected.width, expected.height]
    );
    assert_eq!(
        source.depth(),
        if expected.samples16.is_some() { 16 } else { 8 }
    );
    for ty in 0..source.height().div_ceil(TILE) {
        for tx in 0..source.width().div_ceil(TILE) {
            let view = source
                .prepare_tile((tx, ty), &mut || Ok(()))
                .unwrap()
                .copy_to_scratch(pool)
                .unwrap();
            for yy in 0..TILE {
                for xx in 0..TILE {
                    let (x, y) = (tx * TILE + xx, ty * TILE + yy);
                    let at = (yy * TILE + xx) as usize * usize::from(source.depth() / 8) * 4;
                    if x >= source.width() || y >= source.height() {
                        assert!(view[at..at + usize::from(source.depth() / 8) * 4]
                            .iter()
                            .all(|b| *b == 0));
                        continue;
                    }
                    let pixel = ((y * source.width() + x) * 4) as usize;
                    if let Some(words) = &expected.samples16 {
                        for channel in 0..4 {
                            assert_eq!(
                                u16::from_be_bytes([
                                    view[at + channel * 2],
                                    view[at + channel * 2 + 1]
                                ]),
                                words[pixel + channel],
                                "native16({x},{y})c{channel}"
                            );
                        }
                    } else {
                        assert_eq!(
                            &view[at..at + 4],
                            &expected.bytes[pixel..pixel + 4],
                            "native8({x},{y})"
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn actual_raw_copy_matches_legacy_target_merged_mask_and_current_selection_native8_and16() {
    let mut cases = 0;
    for depth in [8, 16] {
        for selected in 0..4 {
            for merged in [false, true] {
                for mask in [false, true] {
                    let fixture = Fixture::new(doc(depth, selected));
                    let source = fixture.source_hash();
                    let baseline = fixture.disk.used().unwrap();
                    let session = fixture.session();
                    let request = Request {
                        target: &fixture.legacy.layers[0].id,
                        mask,
                        merged,
                    };
                    let expected = crate::clipboard::Image::copy(
                        &fixture.legacy,
                        request.target,
                        mask,
                        merged,
                    )
                    .unwrap();
                    let copy = stage_copy(
                        &fixture.document,
                        &fixture.scene,
                        request,
                        &session,
                        &mut Work::new(1_000_000).unwrap(),
                        &|| Ok(()),
                    )
                    .unwrap();
                    assert_eq!(copy.source(), fixture.document.key());
                    assert_copy(&copy, &expected, &fixture.pool);
                    copy.abort(&session).unwrap();
                    assert_eq!(fixture.disk.used().unwrap(), baseline);
                    assert_eq!(fixture.source_hash(), source);
                    drop(session);
                    fixture.finish();
                    cases += 1;
                }
            }
        }
    }
    assert_eq!(cases, 32);
}
#[path = "extra_tests.rs"]
mod extra;
#[path = "policy_tests.rs"]
mod policy;
