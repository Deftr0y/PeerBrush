use super::*;
use crate::{
    brush::input::LocalStroke,
    edit_bounds::output::{BuiltTiles, TileOutput},
    engine::{
        scene::{Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster},
        Document,
    },
    large_document::{
        root::{Directory, PersistentRootContext},
        FreshFragment,
    },
    psd::memory36::{Admitted, MetadataCharge},
    raster::Raster,
    workspace::source_runtime::{Limits, SourceRuntime, SourceSession},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};
struct Fixture {
    root: PathBuf,
    held: Option<Directory>,
}
impl Fixture {
    fn new() -> Self {
        let root = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pb-native-paint-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self {
            held: Some(Directory::open(&root).unwrap()),
            root,
        }
    }
    fn runtime(&self) -> (Arc<SourceRuntime>, crate::server::Connection) {
        let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
        let connection = crate::server::start(shared, self.root.join("state")).unwrap();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(
            context,
            Limits {
                resident_bytes: 128 * 1024 * 1024,
                disk_bytes: 32 * 1024 * 1024,
                max_files: 32,
                store: crate::large_document::Limits {
                    disk_bytes: 16 * 1024 * 1024,
                    max_blocks: 256,
                    max_resident_blocks: 16,
                    manifest_bytes: 2 * 1024 * 1024,
                },
            },
        )
        .unwrap();
        (runtime, connection)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        if self.root.parent() == Some(parent.as_path())
            && self
                .root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pb-native-paint-")
            && self.held.as_ref().is_some_and(|held| held.check().is_ok())
            && fs::canonicalize(&self.root).is_ok_and(|p| p == self.root)
        {
            drop(self.held.take());
            fs::remove_dir_all(&self.root).unwrap();
        }
    }
}
struct Collector<'a> {
    session: &'a SourceSession,
    surfaces: Vec<Surface>,
    fragments: Vec<FreshFragment>,
}
impl RasterCollector for Collector<'_> {
    fn resident(&mut self, raster: &Raster) -> Result<RasterRef36, String> {
        let fragment = self.session.fresh_fragment()?;
        let surface =
            Surface::stage_current_pixels(raster, fragment.store().clone(), &mut || Ok(()))
                .map_err(|e| e.to_string())?;
        let refs = surface
            .physical_tiles()
            .map(|p| p.reference())
            .collect::<Vec<_>>();
        self.session
            .seal_store(fragment.store(), &refs, b"Native paint component baseline")?;
        self.fragments.push(fragment);
        self.surfaces.push(surface);
        Ok(RasterRef36 {
            id: self.surfaces.len() as u32,
            width: raster.width,
            height: raster.height,
            depth: raster.depth,
            retained: None,
        })
    }
    fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
        Err("Unexpected native fixture source".into())
    }
}
fn native(source: &Document, session: &SourceSession) -> NativeDocument {
    // Known tiny fixture only; this is not an arbitrary metadata admission API.
    let metadata_owner =
        MetadataCharge::reserve(&session.resident(), 512 * 1024, &|| Ok(())).unwrap();
    let mut collector = Collector {
        session,
        surfaces: vec![],
        fragments: vec![],
    };
    let metadata = Document36::capture(source, &mut collector).unwrap();
    for fragment in collector.fragments {
        drop(fragment.publish());
    }
    let pending = PendingScene::new(
        Admitted::from_parts(Arc::new(metadata), metadata_owner),
        RasterRoster::new(collector.surfaces).unwrap(),
        session.resident(),
        session.disk(),
    )
    .unwrap();
    NativeDocument::from_pending(pending).unwrap()
}
fn source(depth: u16) -> Document {
    let mut source = Document::new_depth(9, 7, depth).unwrap();
    source.layers[0].x = -3;
    source.layers[0].y = 2;
    source.layers[0]
        .pixels
        .set16(0, 0, [12347, 23521, 43217, 49153]);
    source.layers[0].pixels.set16(8, 6, [7, 11, 13, 0]);
    source
}
#[test]
fn projected_tiles_are_generation_bound_native_and_preserve_physical_padding() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let source = source(depth);
        let id = &source.layers[0].id;
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, id).unwrap();
        let tile = input.prepare_tile((0, 0), &mut || Ok(())).unwrap();
        assert_eq!(tile.binding(), input.binding());
        assert_eq!(input.origin(), [-3, 2]);
        let bpp = if depth == 16 { 8 } else { 4 };
        assert!(tile.bytes()[9 * bpp..256 * bpp].iter().all(|v| *v == 0));
        let region = input.prepare_region([0, 0, 9, 7], &mut || Ok(())).unwrap();
        assert_eq!(
            region.pixel16(0, 0).unwrap(),
            source.layers[0].pixels.get16(0, 0)
        );
        assert!(region.pixel16(9, 0).is_err());
        let halo = input
            .prepare_halo([-2, -3, 12, 10], &mut || Ok(()))
            .unwrap();
        assert_eq!(
            halo.pixel16(0, 0).unwrap(),
            source.layers[0].pixels.get16(0, 0)
        );
        assert_eq!(halo.pixel16(-1, -1).unwrap(), [0; 4]);
        assert!(halo.pixel16(12, 0).is_err());
        let edited = document
            .capture_edit()
            .unwrap()
            .visibility(id, false, &|| Ok(()))
            .unwrap();
        let changed = document.adopt(edited).ok().unwrap();
        let changed_input = RasterInput::layer(&changed, id).unwrap();
        assert!(changed_input.validate(tile.binding()).is_err());
    }
}
#[test]
fn borrowed_selection_matches_legacy_coverage_priority_red_projection_and_geometry() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    let mut source = source(16);
    source.selection = Some([-2, 2, 4, 6]);
    source.selection_polygon = Some(vec![[-2., 2.], [4., 2.], [-2., 6.]]);
    let mut mask = Raster::new_depth(5, 4, 16);
    mask.set16(1, 1, [32769, 17, 19, 0]);
    source.selection_coverage = Some(crate::selection::Coverage {
        mask,
        bounds: source.selection.unwrap(),
        origin: [-3, 2],
        contours: vec![],
    });
    let document = native(&source, &session);
    let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
    let selection = input.selection().unwrap();
    assert!(selection.has_coverage());
    let prepared = selection.prepare([-3, 2, 5, 7], &mut || Ok(())).unwrap();
    for y in 2..7 {
        for x in -3..5 {
            assert_eq!(
                prepared.value(x, y).unwrap(),
                source.selection_coverage.as_ref().unwrap().value(x, y)
            );
        }
    }
    assert!(prepared.value(5, 2).is_err());
    source.selection_coverage.as_mut().unwrap().bounds = [0, 0, 1, 1];
    let stale = native(&source, &session);
    let geometric = SelectionInput::new(&stale).unwrap();
    assert!(!geometric.has_coverage());
    let prepared = geometric.prepare([-3, 2, 5, 7], &mut || Ok(())).unwrap();
    for y in 2..7 {
        for x in -3..5 {
            assert_eq!(
                prepared.value(x, y).unwrap() > 0.,
                crate::selection::contains_pixel(&source, x, y)
            );
        }
    }
}
#[test]
fn actual_session_brush_stages_exact_native_pixels_without_changing_document_or_history() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let source = source(depth);
        let id = &source.layers[0].id;
        let document = native(&source, &session);
        let key = document.key();
        let revision = document.metadata().revision();
        let input = RasterInput::layer(&document, id).unwrap();
        let points = [[1.25, 3.5], [4.5, 5.25]];
        let pressures = [0.35, 0.9];
        let settings = crate::brush::Settings::from_command(
            &serde_json::json!({"radius":2.3,"opacity":0.63,"flow":0.41}),
        )
        .unwrap();
        let local = LocalStroke::new(&input, &points, Some(&pressures), &mut || Ok(())).unwrap();
        let plan = local
            .brush(&input, settings, [17, 83, 219, 127], false, &mut || Ok(()))
            .unwrap();
        let before = input.prepare_tile((0, 0), &mut || Ok(())).unwrap();
        let output = plan
            .render_tile((0, 0), before.bytes(), &mut || Ok(()))
            .unwrap();
        let baseline = runtime.disk().used().unwrap();
        let mut generated = TileOutput::new(&input, &session).unwrap();
        generated
            .replace_tile(&before, output, &mut || Ok(()))
            .unwrap();
        let BuiltTiles::Draft(draft) = generated.finish(&mut || Ok(())).ok().unwrap() else {
            panic!("Stroke unexpectedly unchanged")
        };
        let staged = draft.seal(&|| Ok(())).ok().unwrap();
        let mut expected = source.layers[0].pixels.clone();
        let mut settings = settings;
        settings.texture_origin = input.origin();
        let points = points.map(|p| [p[0] + 3., p[1] - 2.]);
        crate::brush::paint_with_pressure(
            &mut expected,
            &points,
            Some(&pressures),
            settings,
            [17, 83, 219, 127],
            false,
            None,
            None,
        )
        .unwrap();
        let region = staged
            .surface()
            .prepare_region(Region::new([0, 0, 9, 7]).unwrap(), &mut || Ok(()))
            .unwrap();
        for y in 0..7 {
            for x in 0..9 {
                assert_eq!(region.pixel16(x, y).unwrap(), expected.get16(x, y));
            }
        }
        assert_eq!(document.key(), key);
        assert_eq!(document.metadata().revision(), revision);
        drop(region);
        staged.abort(&session, &|| Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
}
#[test]
fn refused_output_poison_preserves_baseline_and_retires_only_owned_staging() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    let source = source(16);
    let id = &source.layers[0].id;
    let document = native(&source, &session);
    let input = RasterInput::layer(&document, id).unwrap();
    let before = input.prepare_tile((0, 0), &mut || Ok(())).unwrap();
    let baseline = runtime.disk().used().unwrap();
    let mut generated = TileOutput::new(&input, &session).unwrap();
    let mut output =
        ScratchBytes::new(input.pool(), before.bytes().len(), before.bytes().len()).unwrap();
    output.copy_from_slice(before.bytes());
    output[6..8].copy_from_slice(&65535u16.to_be_bytes());
    generated
        .replace_tile(&before, output.freeze(), &mut || Ok(()))
        .unwrap();
    let error = generated.replace_tile(&before, before.bytes().clone(), &mut || {
        Err("cancelled".into())
    });
    assert!(error.is_err());
    let refused = generated.finish(&mut || Ok(())).err().unwrap();
    assert!(refused.reason().contains("partial work"));
    refused.abort().unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let unchanged = TileOutput::new(&input, &session)
        .unwrap()
        .finish(&mut || Ok(()))
        .ok()
        .unwrap();
    assert!(matches!(unchanged, BuiltTiles::Unchanged));
}

#[test]
fn prepared_soft_restriction_matches_the_real_legacy_native_rounding_and_origin() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = source(depth);
        source.layers[0].x = 0;
        source.layers[0].y = 0;
        source.selection = Some([0, 0, 9, 7]);
        let mut mask = Raster::new_depth(9, 7, 16);
        for y in 0..7 {
            for x in 0..9 {
                mask.set16(x, y, [32769, 65535, 7, 0]);
            }
        }
        let coverage = crate::selection::Coverage {
            mask,
            bounds: source.selection.unwrap(),
            origin: [-1, 1],
            contours: vec![],
        };
        source.selection_coverage = Some(coverage.clone());
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let before = input.prepare_tile((0, 0), &mut || Ok(())).unwrap();
        let mut after = source.layers[0].pixels.clone();
        for y in 0..7 {
            for x in 0..9 {
                after.set16(x, y, [655, 54321, 32111, 43001]);
            }
        }
        let bpp = if depth == 16 { 8 } else { 4 };
        let mut candidate =
            ScratchBytes::new(input.pool(), before.bytes().len(), before.bytes().len()).unwrap();
        candidate.copy_from_slice(before.bytes());
        for y in 0..7 {
            for x in 0..9 {
                let at = (y * 256 + x) as usize * bpp;
                for (c, v) in after.get16(x, y).into_iter().enumerate() {
                    if depth == 16 {
                        candidate[at + c * 2..at + c * 2 + 2].copy_from_slice(&v.to_be_bytes());
                    } else {
                        candidate[at + c] = project16(v);
                    }
                }
            }
        }
        let prepared = input
            .selection()
            .unwrap()
            .prepare([0, 0, 9, 7], &mut || Ok(()))
            .unwrap();
        let result = restrict_tile(
            &input,
            (0, 0),
            before.bytes(),
            &candidate.freeze(),
            &prepared,
            &mut || Ok(()),
        )
        .unwrap();
        crate::selection::restrict_raster(&mut after, &source.layers[0].pixels, &coverage, [0, 0]);
        for y in 0..7 {
            for x in 0..9 {
                let at = (y * 256 + x) as usize * bpp;
                let actual = std::array::from_fn::<_, 4, _>(|c| {
                    if depth == 16 {
                        u16::from_be_bytes([result[at + c * 2], result[at + c * 2 + 1]])
                    } else {
                        u16::from(result[at + c]) * 257
                    }
                });
                assert_eq!(actual, after.get16(x, y));
            }
        }
        assert!(result[9 * bpp..256 * bpp].iter().all(|b| *b == 0));
    }
}

#[test]
fn projected_frame_and_brush_share_one_complete_output_with_the_legacy_soft_baseline() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = source(depth);
        source.width = 20;
        source.height = 14;
        source.selection = Some([0, 0, 20, 14]);
        let mut mask = Raster::new_depth(20, 14, 16);
        for y in 0..14 {
            for x in 0..20 {
                mask.set16(x, y, [32769, 0, 0, 0]);
            }
        }
        let coverage = crate::selection::Coverage {
            mask,
            bounds: source.selection.unwrap(),
            origin: [0, 0],
            contours: vec![],
        };
        source.selection_coverage = Some(coverage.clone());
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let frame = input.project_frame([15, 7, 20, 13]).unwrap().unwrap();
        assert_eq!(frame.geometry().new_shape(), [23, 14]);
        assert_eq!(frame.geometry().new_origin(), [-3, 0]);
        let settings = crate::brush::Settings::from_command(
            &serde_json::json!({"radius":2.,"opacity":0.7,"flow":0.6}),
        )
        .unwrap();
        let stroke =
            LocalStroke::for_frame(&input, &frame, &[[18., 10.]], None, &mut || Ok(())).unwrap();
        let baseline = runtime.disk().used().unwrap();
        let BuiltTiles::Draft(draft) = crate::brush::input::stage(
            &input,
            &stroke,
            settings,
            [17, 83, 219, 127],
            false,
            Some(&frame),
            &session,
            &mut || Ok(()),
        )
        .unwrap() else {
            panic!("Frame was not staged")
        };
        let staged = draft.seal(&|| Ok(())).ok().unwrap();
        let mut expected = source.clone();
        crate::edit_bounds::prepare(
            &mut expected,
            &serde_json::json!({"op":"paint","layer":source.layers[0].id,
            "points":[[18.,10.]],"radius":2.,"opacity":0.7,"flow":0.6,"color":[17,83,219,127]}),
        )
        .unwrap();
        let before = expected.layers[0].pixels.clone();
        let origin = [expected.layers[0].x, expected.layers[0].y];
        let mut settings = settings;
        settings.texture_origin = origin;
        let point = [[18. - origin[0] as f32, 10. - origin[1] as f32]];
        let clip = source.selection.map(|r| {
            [
                r[0] - origin[0],
                r[1] - origin[1],
                r[2] - origin[0],
                r[3] - origin[1],
            ]
        });
        crate::brush::paint_with_pressure(
            &mut expected.layers[0].pixels,
            &point,
            None,
            settings,
            [17, 83, 219, 127],
            false,
            clip,
            None,
        )
        .unwrap();
        crate::selection::restrict_raster(
            &mut expected.layers[0].pixels,
            &before,
            &coverage,
            origin,
        );
        let region = staged
            .surface()
            .prepare_region(Region::new([0, 0, 23, 14]).unwrap(), &mut || Ok(()))
            .unwrap();
        for y in 0..14 {
            for x in 0..23 {
                assert_eq!(
                    region.pixel16(x, y).unwrap(),
                    expected.layers[0].pixels.get16(x, y)
                );
            }
        }
        drop(region);
        staged.abort(&session, &|| Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert!(
            crate::edit_bounds::output::stage_frame(&input, &frame, &session, &mut || {
                if runtime.disk().used().unwrap() > baseline + 4096 {
                    Err("cancelled after native frame staging".into())
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
}

#[test]
fn projected_mask_frames_keep_the_color_translation_with_their_own_native_shape() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    let mut source = source(16);
    source.width = 20;
    source.height = 14;
    let mut pixels = Raster::new_depth(5, 4, 16);
    pixels.set16(1, 2, [35731, 35731, 35731, 49153]);
    source.layers[0].mask = Some(crate::engine::Mask {
        enabled: false,
        linked: false,
        cache_key: crate::engine::id(),
        steps: vec![crate::engine::MaskStep {
            id: "mask-paint".into(),
            kind: "paint".into(),
            enabled: false,
            weight: 0.4,
            value: 0.,
            pixels: pixels.clone(),
            settings: serde_json::json!({}),
        }],
    });
    let document = native(&source, &session);
    let color = RasterInput::layer(&document, &source.layers[0].id).unwrap();
    let mask = RasterInput::mask_step(&document, &source.layers[0].id, "mask-paint").unwrap();
    assert_eq!(mask.color([7, 19, 31, 127]), [7, 7, 7, 127]);
    assert_eq!(mask.shape(), [5, 4]);
    let frame = color.project_frame([15, 7, 20, 13]).unwrap().unwrap();
    let mask_frame = frame.for_plane(&mask).unwrap();
    assert_eq!(
        mask_frame.geometry().new_shape(),
        frame.geometry().new_shape()
    );
    assert_eq!(mask_frame.geometry().shift(), [0, 2]);
    let baseline = runtime.disk().used().unwrap();
    let staged =
        crate::edit_bounds::output::stage_frame(&mask, &mask_frame, &session, &mut || Ok(()))
            .unwrap()
            .seal(&|| Ok(()))
            .ok()
            .unwrap();
    let expected = crate::transform::copy_shift(&pixels, 23, 14, 0, 2);
    let region = staged
        .surface()
        .prepare_region(Region::new([0, 0, 23, 14]).unwrap(), &mut || Ok(()))
        .unwrap();
    for y in 0..14 {
        for x in 0..23 {
            assert_eq!(region.pixel16(x, y).unwrap(), expected.get16(x, y));
        }
    }
    drop(region);
    staged.abort(&session, &|| Ok(())).unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
}

#[test]
fn fill_visual_baseline_fixes_unselected_loss_and_keeps_one_real_undo_redo() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = source(depth);
        source.layers[0].x = 0;
        source.layers[0].y = 0;
        source.layers[0].kind = "fill".into();
        source.layers[0].color = [32, 96, 160, 128];
        source.selection = Some([1, 1, 4, 4]);
        let mut mask = Raster::new_depth(9, 7, 16);
        mask.set16(2, 2, [32769, 0, 0, 0]);
        let coverage = crate::selection::Coverage {
            mask,
            bounds: source.selection.unwrap(),
            origin: [0, 0],
            contours: vec![],
        };
        source.selection_coverage = Some(coverage.clone());
        let command = serde_json::json!({"op":"paint.fill","layer":source.layers[0].id,"color":[240,16,48,255]});
        // Reproducer of the former ordering: its before-snapshot sees the raw
        // zero fill raster rather than the procedural visible color.
        let mut unsafe_order = source.clone();
        let raw_before = unsafe_order.clone();
        crate::fill::apply(&mut unsafe_order, &command).unwrap();
        crate::selection::restrict_document(&mut unsafe_order, &raw_before, &coverage);
        assert_eq!(unsafe_order.layers[0].pixels.get16(7, 5), [0; 4]);
        let mut legacy = crate::engine::Engine::new();
        legacy.doc = source.clone();
        legacy
            .edit("human", &[command], None, None, "Fill")
            .unwrap();
        let literal = source.layers[0].color.map(|v| u16::from(v) * 257);
        assert_eq!(legacy.doc.layers[0].pixels.get16(7, 5), literal);
        assert_eq!(legacy.doc.layers[0].kind, "paint");
        assert_eq!(legacy.undo.len(), 1);
        let after = legacy.doc.layers[0].pixels.clone();
        legacy.undo("human").unwrap();
        assert_eq!(legacy.doc.layers[0].kind, "fill");
        assert_eq!(
            legacy.doc.layers[0].pixels.get16(0, 0),
            source.layers[0].pixels.get16(0, 0)
        );
        legacy.redo("human").unwrap();
        assert_eq!(legacy.doc.layers[0].pixels.get16(7, 5), literal);
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let generated = crate::fill::input::stage_fill(
            &input,
            [240, 16, 48, 255],
            None,
            None,
            &session,
            &mut || Ok(()),
        )
        .unwrap();
        assert!(generated.materializes_fill());
        assert_eq!(generated.baseline().unwrap().binding(), input.binding());
        let (BuiltTiles::Draft(draft), proof) = generated.into_parts() else {
            panic!("Materialized fill requires paired output")
        };
        assert_eq!(proof.unwrap().color(), source.layers[0].color);
        let staged = draft.seal(&|| Ok(())).ok().unwrap();
        let region = staged
            .surface()
            .prepare_region(Region::new([0, 0, 9, 7]).unwrap(), &mut || Ok(()))
            .unwrap();
        for y in 0..7 {
            for x in 0..9 {
                assert_eq!(region.pixel16(x, y).unwrap(), after.get16(x, y));
            }
        }
        drop(region);
        staged.abort(&session, &|| Ok(())).unwrap();
    }
}

#[test]
fn corrected_fill_frame_keeps_visible_pixels_at_original_document_coordinates() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = Document::new_depth(12, 10, depth).unwrap();
        source.layers[0].pixels = Raster::new_depth(4, 3, depth);
        source.layers[0]
            .pixels
            .set16(0, 0, [12347, 23521, 43217, 49153]);
        source.layers[0].x = 5;
        source.layers[0].y = 4;
        source.layers[0].kind = "fill".into();
        source.layers[0].color = [32, 96, 160, 128];
        source.selection = Some([0, 0, 12, 10]);
        let mut mask = Raster::new_depth(12, 10, 16);
        mask.set16(1, 1, [32769, 0, 0, 0]);
        source.selection_coverage = Some(crate::selection::Coverage {
            mask,
            bounds: source.selection.unwrap(),
            origin: [0, 0],
            contours: vec![],
        });
        let command = serde_json::json!({"op":"paint.fill","layer":source.layers[0].id,"rect":[0,0,2,2],"color":[240,16,48,255]});
        let mut legacy = crate::engine::Engine::new();
        legacy.doc = source.clone();
        legacy
            .edit("human", &[command], None, None, "Fill")
            .unwrap();
        assert_eq!([legacy.doc.layers[0].x, legacy.doc.layers[0].y], [0, 0]);
        assert_eq!(
            legacy.doc.layers[0].pixels.get16(6, 5),
            [32, 96, 160, 128].map(|v| v * 257)
        );
        assert_eq!(legacy.doc.layers[0].pixels.get16(0, 5), [0; 4]);
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let frame = input.project_frame([0, 0, 2, 2]).unwrap().unwrap();
        let before = runtime.disk().used().unwrap();
        assert!(crate::fill::input::stage_fill(
            &input,
            [240, 16, 48, 255],
            Some([0, 0, 2, 2]),
            None,
            &session,
            &mut || Ok(()),
        )
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), before);
        let generated = crate::fill::input::stage_fill(
            &input,
            [240, 16, 48, 255],
            Some([0, 0, 2, 2]),
            Some(&frame),
            &session,
            &mut || Ok(()),
        )
        .unwrap();
        assert!(generated.materializes_fill());
        let (BuiltTiles::Draft(draft), _) = generated.into_parts() else {
            panic!("Frame fill missing")
        };
        let staged = draft.seal(&|| Ok(())).ok().unwrap();
        let region = staged
            .surface()
            .prepare_region(Region::new([0, 0, 12, 10]).unwrap(), &mut || Ok(()))
            .unwrap();
        for y in 0..10 {
            for x in 0..12 {
                assert_eq!(
                    region.pixel16(x, y).unwrap(),
                    legacy.doc.layers[0].pixels.get16(x, y)
                );
            }
        }
        drop(region);
        staged.abort(&session, &|| Ok(())).unwrap();
    }
}

#[test]
fn fill_clear_consumes_soft_coverage_once_and_keeps_transparent_baseline_rgb() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        for color in [[37, 83, 127, 113], [7, 11, 13, 0], [0, 0, 0, 0]] {
            let mut source = Document::new_depth(9, 7, depth).unwrap();
            source.layers[0].kind = "fill".into();
            source.layers[0].color = color;
            source.selection = Some([1, 1, 4, 4]);
            let mut mask = Raster::new_depth(9, 7, 16);
            mask.set16(2, 2, [32769, 0, 0, 0]);
            source.selection_coverage = Some(crate::selection::Coverage {
                mask,
                bounds: source.selection.unwrap(),
                origin: [0, 0],
                contours: vec![],
            });
            let mut legacy = crate::engine::Engine::new();
            legacy.doc = source.clone();
            legacy.edit("human",&[serde_json::json!({"op":"paint.clear_selection","layer":source.layers[0].id})],None,None,"Clear").unwrap();
            assert_eq!(legacy.undo.len(), 1);
            assert_eq!(legacy.doc.layers[0].kind, "paint");
            assert_eq!(
                legacy.doc.layers[0].pixels.get16(7, 5),
                color.map(|v| u16::from(v) * 257)
            );
            let document = native(&source, &session);
            let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
            let generated =
                crate::fill::input::stage_clear(&input, &session, &mut || Ok(())).unwrap();
            assert!(generated.materializes_fill());
            let (BuiltTiles::Draft(draft), _) = generated.into_parts() else {
                panic!("Fill transition missing")
            };
            let staged = draft.seal(&|| Ok(())).ok().unwrap();
            let region = staged
                .surface()
                .prepare_region(Region::new([0, 0, 9, 7]).unwrap(), &mut || Ok(()))
                .unwrap();
            for y in 0..7 {
                for x in 0..9 {
                    assert_eq!(
                        region.pixel16(x, y).unwrap(),
                        legacy.doc.layers[0].pixels.get16(x, y)
                    );
                }
            }
            drop(region);
            staged.abort(&session, &|| Ok(())).unwrap();
        }
    }
}

#[test]
fn native_fill_refuses_unresolved_folder_and_editable_source_kinds() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for kind in ["group", "adjustment", "text", "vector"] {
        let mut source = Document::new_depth(9, 7, 16).unwrap();
        source.layers[0].kind = kind.into();
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let before = runtime.disk().used().unwrap();
        assert!(crate::fill::input::stage_fill(
            &input,
            [240, 16, 48, 255],
            None,
            None,
            &session,
            &mut || Ok(()),
        )
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), before);
    }
}

#[test]
fn ordered_session_smudge_matches_native_resident_with_one_final_soft_selection() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = Document::new_depth(9, 7, depth).unwrap();
        for y in 0..7 {
            for x in 0..9 {
                source.layers[0].pixels.set16(
                    x,
                    y,
                    [
                        12347 + x as u16 * 911,
                        23521 + y as u16 * 617,
                        43217,
                        (if (x + y) % 4 == 0 { 0 } else { 49153 }),
                    ],
                );
            }
        }
        source.selection = Some([0, 0, 9, 7]);
        let mut coverage = Raster::new_depth(9, 7, 16);
        for y in 0..7 {
            for x in 0..9 {
                coverage.set16(x, y, [if x < 4 { 32769 } else { 49153 }, 0, 0, 0]);
            }
        }
        source.selection_coverage = Some(crate::selection::Coverage {
            mask: coverage,
            bounds: source.selection.unwrap(),
            origin: [0, 0],
            contours: vec![],
        });
        let command = serde_json::json!({"op":"smudge","layer":source.layers[0].id,
            "points":[[2.25,2.5,0.7],[5.25,3.25,0.9],[7.,5.,0.6]],"color":[37,83,127,193],
            "radius":1.8,"spacing":0.8,"hardness":0.4,"flow":0.6,"opacity":0.7,
            "load":0.3,"pickup":0.4,"wetness":0.8});
        let (points, pressures) = crate::brush::points_from_command(&command).unwrap();
        let settings = crate::brush::Settings::from_command(&command).unwrap();
        let mut legacy = crate::engine::Engine::new();
        legacy.doc = source.clone();
        legacy
            .edit("human", &[command], None, None, "Smudge")
            .unwrap();
        assert_eq!(legacy.undo.len(), 1);
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let stroke =
            LocalStroke::new(&input, &points, pressures.as_deref(), &mut || Ok(())).unwrap();
        let before = runtime.disk().used().unwrap();
        let BuiltTiles::Draft(draft) = crate::smudge::input::stage(
            &input,
            &stroke,
            settings,
            [37, 83, 127, 193],
            &session,
            &mut || Ok(()),
        )
        .unwrap() else {
            panic!("No smudge output")
        };
        let output = draft.seal(&|| Ok(())).ok().unwrap();
        let region = output
            .surface()
            .prepare_region(Region::new([0, 0, 9, 7]).unwrap(), &mut || Ok(()))
            .unwrap();
        for y in 0..7 {
            for x in 0..9 {
                assert_eq!(
                    region.pixel16(x, y).unwrap(),
                    legacy.doc.layers[0].pixels.get16(x, y),
                    "depth{depth} at{x},{y}"
                );
            }
        }
        drop(region);
        output.abort(&session, &|| Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), before);
        assert_eq!(document.key(), input.binding().generation());
    }
}

#[test]
fn prepared_bilinear_native_source_preserves_origin_edges_and_explicit_query_errors() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = source(depth);
        for y in 0..7 {
            for x in 0..9 {
                source.layers[0].pixels.set16(
                    x,
                    y,
                    [
                        12347 + x as u16 * 911,
                        23521 + y as u16 * 617,
                        43217,
                        if (x + y) % 4 == 0 { 0 } else { 49153 },
                    ],
                );
            }
        }
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let pixels = input
            .prepare_samples([-4., 1., 6.5, 9.], &mut || Ok(()))
            .unwrap();
        for y in [1., 1.25, 2., 3.75, 8., 9.] {
            for x in [-4., -3.75, -3., -0.5, 5., 6.5] {
                assert_eq!(
                    pixels.sample16(x, y).unwrap(),
                    source.layers[0].pixels.sample16(x + 3., y - 2.)
                );
            }
        }
        assert!(pixels.sample16(6.75, 9.).is_err());
        let before = runtime.resident().used();
        let exterior = input
            .prepare_samples([300_000., 300_000., 300_383., 300_383.], &mut || Ok(()))
            .unwrap();
        assert_eq!(runtime.resident().used(), before);
        assert_eq!(exterior.sample16(300_127.5, 300_271.25).unwrap(), [0; 4]);
        assert!(exterior.sample16(300_384., 300_000.).is_err());
    }
}

#[test]
fn session_clone_and_heal_match_resident_source_offsets_and_soft_selection() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        for heal in [false, true] {
            let mut source = Document::new_depth(9, 7, depth).unwrap();
            let mut sample = crate::engine::Layer::new("Sample", "paint", 9, 7);
            sample.pixels = Raster::new_depth(9, 7, depth);
            sample.x = -2;
            sample.y = 1;
            for y in 0..7 {
                for x in 0..9 {
                    source.layers[0].pixels.set16(
                        x,
                        y,
                        [12347 + x as u16 * 911, 23521 + y as u16 * 617, 43217, 49153],
                    );
                    sample.pixels.set16(
                        x,
                        y,
                        [
                            23521 + x as u16 * 173,
                            43217 + y as u16 * 131,
                            12347 + (x * y) as u16 * 113,
                            if (x + y) % 4 == 0 { 0 } else { 45679 },
                        ],
                    );
                }
            }
            let source_id = sample.id.clone();
            source.layers.push(sample);
            source.selection = Some([0, 0, 9, 7]);
            let mut coverage = Raster::new_depth(9, 7, 16);
            for y in 0..7 {
                for x in 0..9 {
                    coverage.set16(x, y, [if x < 4 { 32769 } else { 49153 }, 0, 0, 0]);
                }
            }
            source.selection_coverage = Some(crate::selection::Coverage {
                mask: coverage,
                bounds: source.selection.unwrap(),
                origin: [0, 0],
                contours: vec![],
            });
            let command = serde_json::json!({"op":if heal {"heal"}else{"clone"},"layer":source.layers[0].id,
            "source_layer":source_id,"source":[3.5,1.25],
            "points":[[2.25,2.5,0.7],[5.25,3.25,0.9],[7.,5.,0.6]],
            "radius":1.8,"spacing":0.8,"hardness":0.4,"flow":0.6,"opacity":0.7,"heal_radius":2});
            let (points, pressures) = crate::brush::points_from_command(&command).unwrap();
            let settings = crate::brush::Settings::from_command(&command).unwrap();
            let mut legacy = crate::engine::Engine::new();
            legacy.doc = source.clone();
            legacy
                .edit("human", &[command], None, None, "Retouch")
                .unwrap();
            assert_eq!(legacy.undo.len(), 1);
            let document = native(&source, &session);
            let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
            let sampling = RasterInput::layer(&document, &source_id).unwrap();
            let stroke =
                LocalStroke::new(&input, &points, pressures.as_deref(), &mut || Ok(())).unwrap();
            let before = runtime.disk().used().unwrap();
            let BuiltTiles::Draft(draft) = crate::retouch::input::stage_raster(
                &input,
                &sampling,
                &stroke,
                settings,
                [3.5 - points[0][0], 1.25 - points[0][1]],
                heal.then_some(2),
                None,
                &session,
                &mut || Ok(()),
            )
            .unwrap() else {
                panic!("No retouch output")
            };
            let output = draft.seal(&|| Ok(())).ok().unwrap();
            let region = output
                .surface()
                .prepare_region(Region::new([0, 0, 9, 7]).unwrap(), &mut || Ok(()))
                .unwrap();
            for y in 0..7 {
                for x in 0..9 {
                    assert_eq!(
                        region.pixel16(x, y).unwrap(),
                        legacy.doc.layers[0].pixels.get16(x, y),
                        "depth{depth} heal{heal} at{x},{y}"
                    );
                }
            }
            drop(region);
            output.abort(&session, &|| Ok(())).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), before);
        }
    }
}

#[test]
fn framed_healing_uses_original_document_source_and_translated_destination_halo() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = Document::new_depth(12, 10, depth).unwrap();
        source.layers[0].x = 4;
        source.layers[0].y = 3;
        source.layers[0].pixels = Raster::new_depth(5, 4, depth);
        for y in 0..4 {
            for x in 0..5 {
                source.layers[0].pixels.set16(
                    x,
                    y,
                    [12347 + x as u16 * 911, 23521 + y as u16 * 617, 43217, 49153],
                );
            }
        }
        let mut sample = crate::engine::Layer::new("Sample", "paint", 12, 10);
        sample.pixels = Raster::new_depth(12, 10, depth);
        for y in 0..10 {
            for x in 0..12 {
                sample.pixels.set16(
                    x,
                    y,
                    [
                        23521 + x as u16 * 173,
                        43217 + y as u16 * 131,
                        12347 + (x * y) as u16 * 113,
                        45679,
                    ],
                );
            }
        }
        let source_id = sample.id.clone();
        source.layers.push(sample);
        let command = serde_json::json!({"op":"heal","layer":source.layers[0].id,"source_layer":source_id,
            "source":[7.5,2.25],"points":[[1.25,1.5,0.7],[6.25,4.25,0.9]],
            "radius":1.8,"spacing":0.8,"hardness":0.4,"flow":0.6,"opacity":0.7,"heal_radius":2});
        let (points, pressures) = crate::brush::points_from_command(&command).unwrap();
        let settings = crate::brush::Settings::from_command(&command).unwrap();
        let mut legacy = crate::engine::Engine::new();
        legacy.doc = source.clone();
        legacy
            .edit("human", &[command], None, None, "Heal")
            .unwrap();
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let sampling = RasterInput::layer(&document, &source_id).unwrap();
        let frame = input.project_frame([0, 0, 9, 7]).unwrap().unwrap();
        let stroke = LocalStroke::for_frame(
            &input,
            &frame,
            &points,
            pressures.as_deref(),
            &mut || Ok(()),
        )
        .unwrap();
        let BuiltTiles::Draft(draft) = crate::retouch::input::stage_raster(
            &input,
            &sampling,
            &stroke,
            settings,
            [7.5 - points[0][0], 2.25 - points[0][1]],
            Some(2),
            Some(&frame),
            &session,
            &mut || Ok(()),
        )
        .unwrap() else {
            panic!("No framed retouch output")
        };
        let output = draft.seal(&|| Ok(())).ok().unwrap();
        let shape = frame.geometry().new_shape();
        assert_eq!(
            shape,
            [
                legacy.doc.layers[0].pixels.width,
                legacy.doc.layers[0].pixels.height
            ]
        );
        assert_eq!(
            frame.geometry().new_origin(),
            [legacy.doc.layers[0].x, legacy.doc.layers[0].y]
        );
        let region = output
            .surface()
            .prepare_region(
                Region::new([0, 0, shape[0] as i32, shape[1] as i32]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        for y in 0..shape[1] as i32 {
            for x in 0..shape[0] as i32 {
                assert_eq!(
                    region.pixel16(x, y).unwrap(),
                    legacy.doc.layers[0].pixels.get16(x, y),
                    "depth{depth} at{x},{y}"
                );
            }
        }
        drop(region);
        output.abort(&session, &|| Ok(())).unwrap();
    }
}

#[test]
fn owned_frame_seed_reads_prior_updates_and_retires_unreferenced_blocks_before_seal() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = Document::new_depth(12, 10, depth).unwrap();
        source.layers[0].pixels = Raster::new_depth(5, 4, depth);
        source.layers[0]
            .pixels
            .set16(2, 2, [12347, 23521, 43217, 49153]);
        source.layers[0].x = 4;
        source.layers[0].y = 3;
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let frame = input.project_frame([0, 0, 9, 7]).unwrap().unwrap();
        let before = runtime.disk().used().unwrap();
        let mut seed = crate::edit_bounds::output::stage_frame_seed(
            &input,
            &frame,
            &session,
            &mut || Ok(()),
            |_, bytes, _| Ok(bytes.clone()),
        )
        .unwrap();
        let tile = seed.prepare_tile((0, 0), &mut || Ok(())).unwrap();
        let bpp = if depth == 16 { 8 } else { 4 };
        let mut changed =
            crate::large_document::ScratchBytes::new(input.pool(), tile.len(), tile.len()).unwrap();
        changed.copy_from_slice(&tile);
        if depth == 16 {
            changed[..8].copy_from_slice(&[0x30, 0x3b, 0x5b, 0xe1, 0xa8, 0xd1, 0xc0, 0x01]);
        } else {
            changed[..4].copy_from_slice(&[37, 83, 127, 193]);
        }
        let expected = changed[..bpp].to_vec();
        seed.replace_tile((0, 0), changed.freeze(), &mut || Ok(()))
            .unwrap();
        let strip = seed.prepare_strip([0, 0, 1, 1], &mut || Ok(())).unwrap();
        assert_eq!(&*strip, expected.as_slice());
        drop((strip, tile));
        let zero = crate::large_document::ScratchBytes::new(
            input.pool(),
            256 * 256 * bpp,
            256 * 256 * bpp,
        )
        .unwrap()
        .freeze();
        seed.replace_tile((0, 0), zero, &mut || Ok(())).unwrap();
        let draft = seed.finish(&mut || Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), before);
        let output = draft.seal(&|| Ok(())).ok().unwrap();
        assert!(output.surface().is_known_zero());
        output.abort(&session, &|| Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), before);

        let seed = crate::edit_bounds::output::stage_frame_seed(
            &input,
            &frame,
            &session,
            &mut || Ok(()),
            |_, bytes, _| Ok(bytes.clone()),
        )
        .unwrap();
        assert!(seed
            .finish(&mut || Err("Cancelled after complete frame writes".into()))
            .is_err());
        assert_eq!(runtime.disk().used().unwrap(), before);
    }
}

#[test]
fn reframed_session_smudge_matches_resident_after_all_dabs_and_once_only_soft_coverage() {
    let f = Fixture::new();
    let (runtime, _connection) = f.runtime();
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let mut source = Document::new_depth(12, 10, depth).unwrap();
        source.layers[0].pixels = Raster::new_depth(5, 4, depth);
        source.layers[0].x = 4;
        source.layers[0].y = 3;
        for y in 0..4 {
            for x in 0..5 {
                source.layers[0].pixels.set16(
                    x,
                    y,
                    [12347 + x as u16 * 911, 23521 + y as u16 * 617, 43217, 49153],
                );
            }
        }
        source.selection = Some([0, 0, 12, 10]);
        let mut coverage = Raster::new_depth(12, 10, 16);
        for y in 0..10 {
            for x in 0..12 {
                coverage.set16(x, y, [if x < 6 { 32769 } else { 49153 }, 0, 0, 0]);
            }
        }
        source.selection_coverage = Some(crate::selection::Coverage {
            mask: coverage,
            bounds: source.selection.unwrap(),
            origin: [0, 0],
            contours: vec![],
        });
        let command = serde_json::json!({"op":"smudge","layer":source.layers[0].id,
            "points":[[5.25,4.5,0.7],[3.25,3.25,0.9],[1.,1.,0.6]],"color":[37,83,127,193],
            "radius":1.8,"spacing":0.8,"hardness":0.4,"flow":0.6,"opacity":0.7,
            "load":0.3,"pickup":0.4,"wetness":0.8});
        let (points, pressures) = crate::brush::points_from_command(&command).unwrap();
        let settings = crate::brush::Settings::from_command(&command).unwrap();
        let mut legacy = crate::engine::Engine::new();
        legacy.doc = source.clone();
        legacy
            .edit("human", &[command], None, None, "Smudge")
            .unwrap();
        assert_eq!(legacy.undo.len(), 1);
        let document = native(&source, &session);
        let input = RasterInput::layer(&document, &source.layers[0].id).unwrap();
        let frame = input.project_frame([0, 0, 9, 7]).unwrap().unwrap();
        let stroke = LocalStroke::for_frame(
            &input,
            &frame,
            &points,
            pressures.as_deref(),
            &mut || Ok(()),
        )
        .unwrap();
        let before = runtime.disk().used().unwrap();
        assert!(crate::smudge::input::stage(
            &input,
            &stroke,
            settings,
            [37, 83, 127, 193],
            &session,
            &mut || Ok(())
        )
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), before);
        let BuiltTiles::Draft(draft) = crate::smudge::input::stage_in_frame(
            &input,
            &stroke,
            settings,
            [37, 83, 127, 193],
            Some(&frame),
            &session,
            &mut || Ok(()),
        )
        .unwrap() else {
            panic!("No framed smudge output")
        };
        let output = draft.seal(&|| Ok(())).ok().unwrap();
        let shape = frame.geometry().new_shape();
        assert_eq!(
            shape,
            [
                legacy.doc.layers[0].pixels.width,
                legacy.doc.layers[0].pixels.height
            ]
        );
        let region = output
            .surface()
            .prepare_region(
                Region::new([0, 0, shape[0] as i32, shape[1] as i32]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        for y in 0..shape[1] as i32 {
            for x in 0..shape[0] as i32 {
                assert_eq!(
                    region.pixel16(x, y).unwrap(),
                    legacy.doc.layers[0].pixels.get16(x, y),
                    "depth{depth} at{x},{y}"
                );
            }
        }
        drop(region);
        output.abort(&session, &|| Ok(())).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), before);
    }
}
