use super::stage_regional_copy as stage_copy;
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
fn selected_merged_copy_keeps_wide_empty_gaps_without_sampling_them() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(8192, 260, depth).unwrap();
        let first = [12347, 23459, 34571, 65535];
        let second = [50001, 42003, 33007, 65535];
        doc.layers[0].pixels.set16(6, 2, first);
        doc.layers[0].pixels.set16(8187, 2, second);
        doc.selection = Some([4, 1, 8188, 260]);
        let fixture = Fixture::new(doc);
        let source = fixture.source_hash();
        let disk = fixture.disk.used().unwrap();
        let session = fixture.session();
        let copy = stage_copy(
            &fixture.document,
            &fixture.scene,
            Request { target: &fixture.legacy.layers[0].id, mask: false, merged: true },
            &session,
            &mut Work::new(300_000).unwrap(),
            &|| Ok(()),
        ).unwrap();
        assert_eq!(copy.origin(), [4, 1]);
        assert_eq!(copy.bounds(), [4, 1, 8188, 260]);
        let output = copy.output.surface();
        assert_eq!(output.depth(), depth);
        assert_eq!(output.physical_tiles().len(), 2);
        for (x, y, expected) in [(2, 1, first), (8183, 1, second), (4092, 1, [0; 4]), (2, 258, [0; 4])] {
            let region = Region::new([x, y, x + 1, y + 1]).unwrap();
            let view = output.prepare_region(region, &mut || Ok(())).unwrap();
            let expected = if depth == 8 {
                expected.map(|word| u16::from(crate::raster::project16(word)) * 257)
            } else {
                expected
            };
            assert_eq!(view.pixel16(x, y).unwrap(), expected);
        }
        copy.abort(&session).unwrap();
        assert_eq!(fixture.disk.used().unwrap(), disk);
        assert_eq!(fixture.source_hash(), source);
        drop(session);
        fixture.finish();
    }
}

#[test]
fn selected_copy_skips_dense_artwork_under_empty_mask_tiles() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(4096, 35, depth).unwrap();
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = [43, 127, 219, 211];
        let mut mask = Raster::new(4089, 34);
        mask.set(2, 1, [91; 4]);
        mask.set(4073, 1, [173; 4]);
        let bounds = [9, 2, 4081, 3];
        doc.selection = Some(bounds);
        doc.selection_coverage = Some(crate::selection::Coverage {
            mask, bounds, origin: [7, 1], contours: vec![],
        });
        let fixture = Fixture::new(doc);
        let baseline = fixture.disk.used().unwrap();
        let session = fixture.session();
        for merged in [false, true] {
            let target = &fixture.legacy.layers[0].id;
            let request = Request { target, mask: false, merged };
            let expected = crate::clipboard::Image::copy(&fixture.legacy, target, false, merged).unwrap();
            let copy = stage_copy(
                &fixture.document, &fixture.scene, request, &session,
                &mut Work::new(1100).unwrap(), &|| Ok(()),
            ).unwrap();
            assert_copy(&copy, &expected, &fixture.pool);
            assert_eq!(copy.bounds(), bounds);
            assert_eq!(copy.output.surface().physical_tiles().len(), 2);
            copy.abort(&session).unwrap();
            assert_eq!(fixture.disk.used().unwrap(), baseline);
        }
        drop(session);
        fixture.finish();
    }
}

#[test]
fn sparse_mask_cut_keeps_raw_hidden_rgb_and_skips_empty_spans() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(4096, 3, depth).unwrap();
        let layer = &mut doc.layers[0];
        layer.x = 5;
        let target = layer.id.clone();
        let mut raw = Raster::new_depth(4096, 3, depth);
        raw.set16(5, 1, [12347, 23459, 34571, 49153]);
        raw.set16(2043, 1, [23459, 34571, 45673, 65535]);
        raw.set16(4075, 1, [45673, 34571, 23459, 0]);
        layer.mask = Some(crate::engine::Mask {
            enabled: true, linked: true, cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: "cut-step".into(), kind: "paint".into(), enabled: false,
                weight: 1., value: 0., pixels: raw, settings: serde_json::Value::Null,
            }],
        });
        let bounds = [10, 1, 4081, 2];
        let mut mask = Raster::new(4081, 3);
        mask.set(10, 1, [91; 4]);
        mask.set(4080, 1, [173; 4]);
        doc.selection = Some(bounds);
        doc.selection_coverage = Some(crate::selection::Coverage {
            mask, bounds, origin: [0, 0], contours: vec![],
        });
        let expected = crate::clipboard::Image::mask_cut(&doc, &target, Some("cut-step")).unwrap();
        let fixture = Fixture::new(doc);
        let before = fixture.source_hash();
        let baseline = fixture.disk.used().unwrap();
        let session = fixture.session();
        let command = serde_json::json!({"op":"paint.clear_selection","layer":target,"mask":true,"step":"cut-step"});
        let copy = stage_mask_cut(
            &fixture.document, &fixture.scene, &command, &session,
            &mut Work::new(1100).unwrap(), &|| Ok(()),
        ).unwrap();
        assert_copy(&copy, &expected, &fixture.pool);
        assert_eq!(copy.bounds(), bounds);
        assert_eq!(copy.output.surface().physical_tiles().len(), 2);
        copy.abort(&session).unwrap();
        assert_eq!(fixture.disk.used().unwrap(), baseline);
        assert_eq!(fixture.source_hash(), before);
        drop(session);
        fixture.finish();
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
                    let image = copy
                        .retire_into_transport(&session, &fixture.pool, &|| Ok(()))
                        .unwrap();
                    assert_eq!(image.bytes, expected.bytes);
                    assert_eq!(image.samples16, expected.samples16);
                    assert_eq!(image.origin, expected.origin);
                    assert_eq!(fixture.disk.used().unwrap(), baseline);
                    assert_eq!(fixture.source_hash(), source);
                    let retained = image.bytes.len()
                        + image.samples16.as_ref().map_or(0, |words| words.len() * 2)
                        + 4096;
                    let used = fixture.pool.used();
                    drop(image);
                    assert_eq!(fixture.pool.used(), used - retained);
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
#[path = "pixel_transport_tests.rs"]
mod pixel_transport;
#[path = "policy_tests.rs"]
mod policy;
#[path = "transport_tests.rs"]
mod transport;

#[test]
fn selected_layer_and_mask_copy_fit_without_unrelated_color_stages() {
    use crate::engine::{
        native::worker::tests::Fixture as RuntimeFixture,
        scene::{CapturedScene, LegacyTileCache}, Layer, Mask, MaskStep,
    };

    for depth in [8, 16] {
        for mask in [false, true] {
            let fixture = RuntimeFixture::new();
            let (runtime, _connection) = fixture.runtime();
            let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let mut doc = Document::new_depth(4097, 513, depth).unwrap();
            let shadow = || crate::effects::Effect {
                id: crate::engine::id(), kind: "drop_shadow".into(), enabled: true, weight: 1.,
                settings: crate::effects::defaults("drop_shadow").into(),
            };
            let mut base = Layer::new("Clipping base", "paint", 4097, 513);
            base.pixels = Raster::new_depth(4097, 513, depth);
            base.effects.push(shadow());
            let layer = &mut doc.layers[0];
            layer.x = 5;
            layer.y = 1;
            layer.visible = false;
            layer.clip_to = Some(base.id.clone());
            let target = layer.id.clone();
            layer.pixels = Raster::new_depth(6, 3, depth);
            let mut paint = Raster::new_depth(6, 3, depth);
            for y in 0..3 {
                for x in 0..6 {
                    layer.pixels.set16(x, y, [12347, 31119, 52973, 65535]);
                    paint.set16(x, y, [22119, 0, 0, 65535]);
                }
            }
            let expected = if mask {
                let gray = paint.get16(0, 0)[0];
                layer.effects.push(shadow());
                layer.mask = Some(Mask {
                    enabled: false, linked: false, cache_key: crate::engine::id(),
                    steps: vec![MaskStep { id: crate::engine::id(), kind: "paint".into(), enabled: true,
                        weight: 1., value: 0., pixels: paint, settings: serde_json::Value::Null }],
                });
                [gray, gray, gray, 65535]
            } else {
                layer.pixels.get16(0, 0)
            };
            doc.layers.push(base);
            doc.selection = Some([5, 1, 11, 4]);
            let staged = CapturedScene::from_parts(doc, "scoped-copy".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
            let scene = SceneSnapshot::from_native(
                SceneIdentity { project_id: "scoped-copy".into(), document_id: native.metadata().id().into(), revision: 0 },
                native.clone(), vec![], None, None,
            ).unwrap();
            let session = staged.session.as_ref().unwrap();
            let disk = runtime.disk();
            let baseline = disk.used().unwrap();
            let held = disk.reserve(disk.limit() - disk.admitted().unwrap() - 4 * 1024 * 1024).unwrap();
            let admitted = disk.used().unwrap();
            let copy = super::stage_copy(&native, &scene, Request { target: &target, mask, merged: false },
                session, &mut Work::new(1024).unwrap(), &|| Ok(())).unwrap();
            assert_eq!(copy.bounds(), [5, 1, 11, 4]);
            assert_eq!(copy.output.surface().depth(), depth);
            let pixels = copy.output.surface().prepare_region(Region::new([0, 0, 6, 3]).unwrap(), &mut || Ok(())).unwrap();
            for y in 0..3 {
                for x in 0..6 { assert_eq!(pixels.pixel16(x, y).unwrap(), expected); }
            }
            drop(pixels);
            copy.abort(session).unwrap();
            assert_eq!(disk.used().unwrap(), admitted);

            // The same budget cannot hold a full-canvas shadow stage.
            let full = Stored::prepare(Topology::new(&scene).unwrap(), session, &mut || Ok(()));
            assert!(full.err().unwrap().contains("budget exceeded"));
            assert_eq!(disk.used().unwrap(), admitted);
            let cropped = super::stage_copy(&native, &scene,
                Request { target: &target, mask: false, merged: !mask },
                session, &mut Work::new(1024).unwrap(), &|| Ok(())).unwrap();
            assert_eq!(cropped.bounds(), [5, 1, 11, 4]);
            cropped.abort(session).unwrap();
            assert_eq!(disk.used().unwrap(), admitted);
            drop(held);
            assert_eq!(disk.used().unwrap(), baseline);
        }
    }
}

#[test]
fn rendered_copy_matches_masks_styles_clipping_filters_and_profiles_at_source_precision() {
    use crate::{
        effects::Effect,
        engine::native::worker::tests::Fixture as RuntimeFixture,
        engine::{
            scene::{CapturedScene, LegacyTileCache},
            Layer, Mask, MaskStep,
        },
    };
    let effect = |kind: &str| Effect {
        id: crate::engine::id(),
        kind: kind.into(),
        enabled: true,
        weight: 0.63,
        settings: serde_json::json!({}).into(),
    };
    for depth in [8, 16] {
        for profiled in [false, true] {
            let fixture = RuntimeFixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(512 * 1024 * 1024, 128 * 1024 * 1024);
            let mut legacy = doc(depth, 0);
            legacy.width = 273;
            legacy.selection = Some([0, 0, 273, 19]);
            let mut coverage = Raster::new(273, 19);
            for y in 0..19 {
                for x in 0..273 {
                    let v = [0, 1, 64, 127, 128, 254, 255][(x + y * 3) as usize % 7];
                    coverage.set(x, y, [v, v, v, 255]);
                }
            }
            legacy.selection_coverage = Some(crate::selection::Coverage {
                mask: coverage,
                bounds: legacy.selection.unwrap(),
                origin: [0, 0],
                contours: vec![],
            });
            legacy.layers[0].pixels = Raster::new_depth(267, 19, depth);
            for y in 0..19 {
                for x in 0..267 {
                    legacy.layers[0].pixels.set16(
                        x,
                        y,
                        [12347 + (x as u16) * 101, 23459, 34571, 49153],
                    );
                }
            }
            legacy.layers[0].effects = vec![effect("invert"), effect("drop_shadow")];
            legacy.layers[0].mask = Some(Mask {
                enabled: profiled,
                linked: false,
                cache_key: crate::engine::id(),
                steps: vec![MaskStep {
                    id: crate::engine::id(),
                    kind: "fill".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.63,
                    pixels: Raster::new_depth(267, 19, depth),
                    settings: serde_json::json!({}),
                }],
            });
            let mut group = Layer::new("Folder", "group", 273, 19);
            group.pixels = Raster::new_depth(273, 19, depth);
            group.opacity = 0.71;
            let mut adjustment = Layer::new("Invert", "adjustment", 273, 19);
            adjustment.pixels = Raster::new_depth(273, 19, depth);
            adjustment.effects = vec![effect("invert")];
            adjustment.parent = Some(group.id.clone());
            legacy.layers[0].clip_to = Some(legacy.layers[1].id.clone());
            for layer in &mut legacy.layers {
                layer.parent = Some(group.id.clone());
            }
            legacy.layers.insert(0, adjustment);
            legacy.layers.insert(0, group);
            legacy.filters = vec![effect("invert")];
            if profiled {
                legacy.icc_profile = Some(
                    moxcms::ColorProfile::new_display_p3()
                        .encode()
                        .unwrap()
                        .into(),
                );
            }
            let cache = LegacyTileCache::new(&runtime.resident(), 16).unwrap();
            let mut staged = CapturedScene::from_parts(
                legacy.clone(),
                "rendered-copy".into(),
                vec![],
                None,
                None,
            )
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let session = staged.session.take().unwrap();
            let scene = SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: "rendered-copy".into(),
                    document_id: legacy.id.clone(),
                    revision: legacy.revision,
                },
                native.clone(),
                vec![],
                None,
                None,
            )
            .unwrap();
            let disk = runtime.disk().used().unwrap();
            for target in &legacy.layers {
                for (mask, merged) in [(false, false), (true, false), (false, true), (true, true)] {
                    let expected =
                        crate::clipboard::Image::copy(&legacy, &target.id, mask, merged).unwrap();
                    if mask && merged {
                        let color = crate::clipboard::Image::copy(&legacy, &target.id, false, true)
                            .unwrap();
                        assert_eq!(expected.bytes, color.bytes);
                        assert_eq!(expected.samples16, color.samples16);
                        assert_eq!(expected.origin, color.origin);
                    }
                    let copy = super::stage_copy(
                        &native,
                        &scene,
                        Request {
                            target: &target.id,
                            mask,
                            merged,
                        },
                        &session,
                        &mut Work::new(4_000_000).unwrap(),
                        &|| Ok(()),
                    )
                    .unwrap();
                    assert_copy(&copy, &expected, &runtime.resident());
                    let image = copy
                        .retire_into_transport(&session, native.budget(), &|| session.check())
                        .unwrap();
                    assert_eq!(image.bytes, expected.bytes);
                    assert_eq!(image.samples16, expected.samples16);
                    assert_eq!(image.origin, expected.origin);
                    drop(image);
                    assert_eq!(runtime.disk().used().unwrap(), disk);
                }
            }
            let refused = super::stage_copy(
                &native,
                &scene,
                Request {
                    target: &legacy.layers[2].id,
                    mask: false,
                    merged: false,
                },
                &session,
                &mut Work::new(4_000_000).unwrap(),
                &|| {
                    if runtime.disk().used().unwrap() > disk {
                        Err("rendered copy interrupted".into())
                    } else {
                        Ok(())
                    }
                },
            )
            .err()
            .unwrap();
            assert!(refused.contains("rendered copy interrupted"));
            assert_eq!(runtime.disk().used().unwrap(), disk);
            drop((
                scene,
                native,
                session,
                staged.seals,
                cache,
                runtime,
                connection,
            ));
        }
    }
}
