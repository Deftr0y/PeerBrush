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
