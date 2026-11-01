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
    cell::{Cell, RefCell},
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, Mutex},
};
// Observation only: production builds contain no hook or control interface.
// Each test's worker thread retains its own counter, restored even on unwind.
thread_local! {
    static COMPLETED_TILES: RefCell<Option<Rc<Cell<usize>>>> = const { RefCell::new(None) };
}
struct CompletedTiles {
    count: Rc<Cell<usize>>,
    previous: Option<Rc<Cell<usize>>>,
}
impl CompletedTiles {
    fn observe() -> Self {
        let count = Rc::new(Cell::new(0));
        let previous = COMPLETED_TILES.with(|slot| slot.replace(Some(count.clone())));
        Self { count, previous }
    }
    fn count(&self) -> usize {
        self.count.get()
    }
}
impl Drop for CompletedTiles {
    fn drop(&mut self) {
        COMPLETED_TILES.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}
pub(super) fn observe_completed_tile() {
    COMPLETED_TILES.with(|slot| {
        if let Some(count) = slot.borrow().as_ref() {
            count.set(count.get().checked_add(1).unwrap());
        }
    });
}
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
    fn block_files(&self) -> Vec<(PathBuf, Vec<u8>)> {
        // Inspect only this exact private fixture, with finite depth, entry and
        // byte bounds. No symlink or other-file contents are followed/read.
        self.held.as_ref().unwrap().check().unwrap();
        let mut pending = vec![(self.path.clone(), 0usize)];
        let mut files = Vec::new();
        let mut entries = 0;
        while let Some((directory, depth)) = pending.pop() {
            assert!(depth <= 8);
            for entry in fs::read_dir(directory).unwrap() {
                entries += 1;
                assert!(entries <= 128);
                let entry = entry.unwrap();
                let metadata = fs::symlink_metadata(entry.path()).unwrap();
                assert!(!metadata.file_type().is_symlink());
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    assert_eq!(metadata.file_attributes() & 0x400, 0);
                }
                if metadata.is_dir() {
                    pending.push((entry.path(), depth + 1));
                } else if entry.file_name() == "blocks.pbt" {
                    assert!(metadata.is_file() && metadata.len() <= 1024 * 1024);
                    files.push((entry.path(), fs::read(entry.path()).unwrap()));
                }
            }
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        self.held.as_ref().unwrap().check().unwrap();
        files
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
        },
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
    let before_files = fixture.block_files();
    let original = base.key();
    let completed = CompletedTiles::observe();
    let foreign_path = fixture.path.join("unrelated.bin");
    fs::write(&foreign_path, b"preserve unrelated bytes").unwrap();
    let error = render(&p, [280, 19], 16, &base, &session, &mut || {
        // Store::put has returned only after the complete tile record, seal,
        // identity/stamp and SHA rechecks. A reservation alone proves none of
        // these facts and can fire during an unverified partial append.
        if completed.count() != 0 {
            Err("vector canceled after completed provisional tile".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(
        error.contains("vector canceled"),
        "VECTOR_REFUSAL_STATUS completed={} canceled={} retained={}",
        completed.count(),
        error.contains("vector canceled"),
        error.contains("cleanup retained inventory")
    );
    assert_eq!(completed.count(), 1);
    assert_eq!(session.disk().used().unwrap(), before);
    assert_eq!(fixture.block_files(), before_files);
    assert_eq!(base.key(), original);
    assert_eq!(
        fs::read(&foreign_path).unwrap(),
        b"preserve unrelated bytes"
    );
    let foreign = ResidentBudget::new(256 * 1024 * 1024).unwrap();
    let p = PreparedSource::new(&source, None, &foreign, &mut || Ok(())).unwrap();
    assert!(render(&p, [280, 19], 16, &base, &session, &mut || Ok(())).is_err());
    assert_eq!(session.disk().used().unwrap(), before);
}

#[test]
fn actual_session_vector_mid_record_refusal_retains_unverified_bytes_and_quota() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime(16 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base = base(&session, 16);
    let original = base.key();
    let source = definition();
    let prepared = PreparedSource::new(&source, None, &session.resident(), &mut || Ok(())).unwrap();
    let before = session.disk().used().unwrap();
    let before_files = fixture.block_files();
    let completed = CompletedTiles::observe();
    let error = render(&prepared, [280, 19], 16, &base, &session, &mut || {
        if session.disk().used().map_err(|e| e.to_string())? > before + 4096 {
            Err("vector canceled during unverified append".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(
        error.contains("vector canceled"),
        "VECTOR_REFUSAL_STATUS completed={} canceled={} retained={}",
        completed.count(),
        error.contains("vector canceled"),
        error.contains("cleanup retained inventory")
    );
    assert!(error.contains("cleanup retained inventory"));
    assert_eq!(completed.count(), 0);
    let retained = session.disk().used().unwrap();
    assert!(retained > before + 4096);
    let retained_files = fixture.block_files();
    assert_eq!(retained_files.len(), before_files.len() + 1);
    let new = retained_files
        .iter()
        .filter(|(name, _)| !before_files.iter().any(|(old, _)| old == name))
        .collect::<Vec<_>>();
    assert_eq!(new.len(), 1);
    assert!(new[0].1.starts_with(b"PBTILES1") && new[0].1.len() > 24);
    for (name, bytes) in &before_files {
        assert!(retained_files.contains(&(name.clone(), bytes.clone())));
    }
    assert_eq!(base.key(), original);
    // A separate admission refusal cannot opportunistically retire this
    // unknown prefix or refund its charge.
    let foreign = ResidentBudget::new(256 * 1024 * 1024).unwrap();
    let prepared = PreparedSource::new(&source, None, &foreign, &mut || Ok(())).unwrap();
    assert!(render(&prepared, [280, 19], 16, &base, &session, &mut || Ok(())).is_err());
    assert_eq!(session.disk().used().unwrap(), retained);
    assert_eq!(fixture.block_files(), retained_files);
    assert_eq!(base.key(), original);
}
