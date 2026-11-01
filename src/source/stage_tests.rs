use super::*;
use crate::{
    engine::{
        scene::{
            Content36, Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster,
            Source36,
        },
        Document, Engine,
    },
    large_document::{
        root::{Directory, PersistentRootContext},
        ResidentBudget,
    },
    psd::memory36::{Admitted, MetadataCharge},
    workspace::source_runtime::{Limits, SourceRuntime},
};
use std::{
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};
struct Fixture {
    path: PathBuf,
    held: Option<Directory>,
}
impl Fixture {
    fn new() -> Self {
        let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = parent.join(format!("pb-native-vector-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self {
            held: Some(Directory::open(&path).unwrap()),
            path,
        }
    }
    fn runtime(&self, disk: u64) -> (Arc<SourceRuntime>, crate::server::Connection) {
        let connection =
            crate::server::start(Arc::new(Mutex::new(Engine::new())), self.path.join("state"))
                .unwrap();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(
            context,
            Limits {
                resident_bytes: 256 * 1024 * 1024,
                disk_bytes: disk,
                max_files: 16,
                store: crate::large_document::Limits {
                    disk_bytes: disk,
                    max_blocks: 128,
                    max_resident_blocks: 8,
                    manifest_bytes: 8 * 1024 * 1024,
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
        if self.path.parent() == Some(parent.as_path())
            && self
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pb-native-vector-")
            && self.held.as_ref().is_some_and(|h| h.check().is_ok())
            && fs::canonicalize(&self.path).is_ok_and(|p| p == self.path)
        {
            drop(self.held.take());
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
}
struct Collector {
    rasters: Vec<Surface>,
}
impl RasterCollector for Collector {
    fn resident(&mut self, r: &crate::raster::Raster) -> Result<RasterRef36, String> {
        self.rasters
            .push(Surface::zero(r.width, r.height, r.depth).unwrap());
        Ok(RasterRef36 {
            id: self.rasters.len() as u32,
            width: r.width,
            height: r.height,
            depth: r.depth,
            retained: None,
        })
    }
    fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
        Err("Unexpected existing source".into())
    }
}
fn base(session: &SourceSession, depth: u16) -> NativeDocument {
    let mut collector = Collector { rasters: vec![] };
    let metadata =
        Document36::capture(&Document::new_depth(8, 8, depth).unwrap(), &mut collector).unwrap();
    let owner = MetadataCharge::reserve(&session.resident(), 64 * 1024, &|| Ok(())).unwrap();
    NativeDocument::from_pending(
        PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), owner),
            RasterRoster::new(collector.rasters).unwrap(),
            session.resident(),
            session.disk(),
        )
        .unwrap(),
    )
    .unwrap()
}
fn definition() -> Source36 {
    Source36 {
        width: 280,
        height: 19,
        matrix: [1., 0., 0., 1., 0., 0.],
        content: Content36::Shape {
            shape: "rectangle".into(),
            bounds: [0., 0., 280., 19.],
            points: vec![],
            closed: true,
            fill: [12345, 23457, 45679, 43871],
            stroke: [54321, 32109, 10987, 49153],
            stroke_width: 1.375,
        }
        .into(),
    }
}
#[test]
fn actual_session_vector_output_is_sealed_native_faithful_and_abortable() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime(16 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let base = base(&session, depth);
        let source = definition();
        let p = PreparedSource::new(&source, None, &session.resident(), &mut || Ok(())).unwrap();
        let before = session.disk().used().unwrap();
        let staged = render(&p, [280, 19], depth, &base, &session, &mut || Ok(())).unwrap();
        let actual = staged
            .surface()
            .prepare_region(Region::new([0, 0, 280, 19]).unwrap(), &mut || Ok(()))
            .unwrap();
        let legacy: crate::source::Source =
            serde_json::from_value(serde_json::to_value(&source).unwrap()).unwrap();
        let expected = legacy.render(280, 19, depth).unwrap();
        for y in 0..19 {
            for x in 0..280 {
                assert_eq!(actual.pixel16(x, y).unwrap(), expected.get16(x, y));
            }
        }
        drop(actual);
        staged.abort(&session, &|| Ok(())).unwrap();
        assert_eq!(session.disk().used().unwrap(), before);
    }
}
#[test]
fn actual_session_vector_refusal_reclaims_only_owned_provisional_files() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime(16 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base = base(&session, 16);
    let source = definition();
    let p = PreparedSource::new(&source, None, &session.resident(), &mut || Ok(())).unwrap();
    let before = session.disk().used().unwrap();
    let error = render(&p, [280, 19], 16, &base, &session, &mut || {
        if session.disk().used().map_err(|e| e.to_string())? > before + 4096 {
            Err("vector canceled after provisional admission".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(error.contains("vector canceled"), "{error}");
    assert_eq!(session.disk().used().unwrap(), before);
    let foreign = ResidentBudget::new(256 * 1024 * 1024).unwrap();
    let p = PreparedSource::new(&source, None, &foreign, &mut || Ok(())).unwrap();
    assert!(render(&p, [280, 19], 16, &base, &session, &mut || Ok(())).is_err());
    assert_eq!(session.disk().used().unwrap(), before);
}
