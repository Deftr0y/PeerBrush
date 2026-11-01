//! Presentation accepts only complete tagged display pixels from the worker.
//! Acquiring/capturing a source session belongs to the off-UI dispatcher.
use super::preview_worker::{NativeReplies, Worker};
use crate::engine::native::DocumentKey;
use crate::render::native_preview::{Frame, Request, Tag};
use eframe::egui;

#[derive(Default)]
pub(super) struct Presentation {
    expected: Option<(u64, Tag)>,
    replies: NativeReplies,
}
impl Presentation {
    pub fn request(
        &mut self,
        worker: &mut Worker,
        request: Request,
        job: u64,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let tag = request.tag();
        let ctx = ctx.clone();
        worker
            .request_native(request, job, self.replies.clone(), move || {
                ctx.request_repaint()
            })
            .map_err(|_| "Native preview worker could not start".to_owned())?;
        self.expected = Some((job, tag));
        Ok(())
    }

    /// A raw layer thumbnail shares the same full-canvas/48px preview contract.
    /// Mask-step previews require a distinct prepared provider and cannot enter.
    pub fn thumbnail(
        &mut self,
        worker: &mut Worker,
        request: Request,
        job: u64,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        if !request.is_thumbnail() {
            return Err("Native thumbnail requires a captured full-canvas layer preview".into());
        }
        self.request(worker, request, job, ctx)
    }

    /// Rendered pixels remain charged while the presenter/texture upload owns
    /// the Frame. This adapter never copies them to an unowned Vec or Raster.
    pub fn take(
        &mut self,
        current: DocumentKey,
        revision: u64,
        job: u64,
    ) -> Result<Option<Frame>, String> {
        let Some(reply) = self.replies.take() else {
            return Ok(None);
        };
        let Some((expected_job, expected_tag)) = &self.expected else {
            return Ok(None);
        };
        if *expected_job != job
            || reply.job != job
            || expected_tag.key() != reply.tag.key()
            || expected_tag.key().source != current
            || expected_tag.key().identity.revision != revision
        {
            return Ok(None);
        }
        let frame = reply.result?;
        if !frame.matches(expected_tag) {
            return Err("Native preview reply does not match its captured request".into());
        }
        Ok(Some(frame))
    }
    pub fn clear(&mut self) {
        self.expected = None;
        drop(self.replies.take());
    }
}

impl super::PeerBrush {
    /// The native dispatcher hands over an already-captured request. Live
    /// Engine document migration is separate from this nominal worker route.
    pub(crate) fn queue_native_preview(
        &mut self,
        request: Request,
        ctx: &egui::Context,
    ) -> Result<(), String> {
        let job = self
            .preview_epoch
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            .wrapping_add(1);
        self.preview_job = job;
        self.pending = true;
        if let Err(error) = self
            .native_preview
            .request(&mut self.preview_worker, request, job, ctx)
        {
            self.pending = false;
            return Err(error);
        }
        Ok(())
    }
    pub(crate) fn take_native_preview(
        &mut self,
        current: DocumentKey,
        revision: u64,
    ) -> Result<Option<Frame>, String> {
        let frame = self
            .native_preview
            .take(current, revision, self.preview_job);
        if !matches!(&frame, Ok(None)) {
            self.pending = false;
        }
        frame
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{
            native::NativeDocument,
            scene::{
                Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster,
                SceneIdentity, SceneSnapshot,
            },
            Document, Engine,
        },
        large_document::{
            root::{Directory, PersistentRootContext},
            Limits,
        },
        psd::memory36::{Admitted, MetadataCharge},
        raster::{Raster, Surface},
        workspace::source_runtime::{Limits as RuntimeLimits, SourceRuntime, SourceSession},
    };
    use std::{
        fs,
        path::PathBuf,
        sync::{atomic::AtomicU64, Arc, Mutex},
        time::{Duration, Instant},
    };

    struct Fixture {
        path: PathBuf,
        held: Option<Directory>,
    }
    impl Fixture {
        fn new() -> Self {
            let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
            let path = parent.join(format!("pb-native-preview-ui-{}", crate::engine::id()));
            fs::create_dir(&path).unwrap();
            Self {
                held: Some(Directory::open(&path).unwrap()),
                path,
            }
        }
        fn runtime(&self) -> (Arc<SourceRuntime>, crate::server::Connection) {
            let connection =
                crate::server::start(Arc::new(Mutex::new(Engine::new())), self.path.join("state"))
                    .unwrap();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let runtime = SourceRuntime::new(
                context,
                RuntimeLimits {
                    resident_bytes: 32 * 1024 * 1024,
                    disk_bytes: 16 * 1024 * 1024,
                    max_files: 16,
                    store: Limits {
                        disk_bytes: 16 * 1024 * 1024,
                        max_blocks: 64,
                        max_resident_blocks: 16,
                        manifest_bytes: 1024 * 1024,
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
            assert_eq!(self.path.parent(), Some(parent.as_path()));
            assert!(self
                .path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pb-native-preview-ui-"));
            self.held.as_ref().unwrap().check().unwrap();
            assert_eq!(fs::canonicalize(&self.path).unwrap(), self.path);
            drop(self.held.take());
            fs::remove_dir_all(&self.path).unwrap();
        }
    }
    // This fixture explicitly proves a zero resident source; production never
    // replaces unreadable/nonzero source bytes with a transparent Surface.
    struct ZeroSources {
        sources: Vec<Surface>,
    }
    impl RasterCollector for ZeroSources {
        fn resident(&mut self, raster: &Raster) -> Result<RasterRef36, String> {
            for y in 0..raster.height as i32 {
                for x in 0..raster.width as i32 {
                    assert_eq!(raster.get16(x, y), [0; 4]);
                }
            }
            self.sources.push(
                Surface::zero(raster.width, raster.height, raster.depth)
                    .map_err(|e| e.to_string())?,
            );
            Ok(RasterRef36 {
                id: self.sources.len() as u32,
                width: raster.width,
                height: raster.height,
                depth: raster.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
            Err("Unexpected fixture source".into())
        }
    }
    fn capture(session: &SourceSession, depth: u16) -> (Document, Arc<SceneSnapshot>) {
        let mut doc = Document::new_depth(9, 7, depth).unwrap();
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = [19, 43, 71, 199];
        let mut collector = ZeroSources { sources: vec![] };
        let charge = MetadataCharge::reserve(&session.resident(), 1024 * 1024, &|| Ok(())).unwrap();
        let metadata = Document36::capture(&doc, &mut collector).unwrap();
        let pending = PendingScene::new(
            Admitted::from_parts(Arc::new(metadata), charge),
            RasterRoster::new(collector.sources).unwrap(),
            session.resident(),
            session.disk(),
        )
        .unwrap();
        let native = NativeDocument::from_pending(pending).unwrap();
        let snapshot = SceneSnapshot::from_native(
            SceneIdentity {
                project_id: "preview-test".into(),
                document_id: doc.id.clone(),
                revision: doc.revision,
            },
            native,
            vec![],
            None,
            None,
        )
        .unwrap();
        (doc, Arc::new(snapshot))
    }
    fn wait(
        presentation: &mut Presentation,
        source: DocumentKey,
        revision: u64,
        job: u64,
    ) -> Frame {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(frame) = presentation.take(source, revision, job).unwrap() {
                return frame;
            }
            assert!(Instant::now() < end, "Native preview did not complete");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    #[test]
    fn actual_session_worker_releases_all_sources_before_native8_16_presentation() {
        for depth in [8, 16] {
            let fixture = Fixture::new();
            let (runtime, connection) = fixture.runtime();
            let session = Arc::new(runtime.begin().unwrap());
            let (doc, scene) = capture(&session, depth);
            let source = scene.key();
            let weak_scene = Arc::downgrade(&scene);
            let weak_session = Arc::downgrade(&session);
            let request = Request::capture(
                scene.clone(),
                session.clone(),
                None,
                48,
                Some(&doc.layers[0].id),
                false,
            )
            .unwrap();
            let mut worker = Worker::new(Arc::new(AtomicU64::new(7)));
            let mut presentation = Presentation::default();
            presentation
                .thumbnail(&mut worker, request, 7, &egui::Context::default())
                .unwrap();
            drop(scene);
            drop(session);
            let frame = wait(&mut presentation, source, doc.revision, 7);
            assert!(weak_scene.upgrade().is_none());
            assert!(weak_session.upgrade().is_none());
            assert_eq!(
                &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                &doc.preview(None, 48, Some(&doc.layers[0].id), false)
                    .unwrap()
                    .2
            );
            drop(frame);
            presentation.clear();
            worker.stop();
            drop(presentation);
            drop(worker);
            drop(runtime);
            drop(connection);
        }
    }
    #[test]
    fn native_worker_checks_epoch_and_presenter_rejects_changed_source_generation() {
        let fixture = Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let session = Arc::new(runtime.begin().unwrap());
        let (doc, scene) = capture(&session, 16);
        let source = scene.key();
        let epoch = Arc::new(AtomicU64::new(9));
        let mut worker = Worker::new(epoch.clone());
        let mut presentation = Presentation::default();
        let request =
            Request::capture(scene.clone(), session.clone(), None, 48, None, false).unwrap();
        assert!(!request.is_thumbnail());
        presentation
            .request(&mut worker, request, 8, &egui::Context::default())
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            if let Err(error) = presentation.take(source, doc.revision, 8) {
                assert_eq!(error, "Preview superseded");
                break;
            }
            assert!(Instant::now() < end);
            std::thread::sleep(Duration::from_millis(2));
        }
        let request =
            Request::capture(scene.clone(), session.clone(), None, 48, None, false).unwrap();
        presentation
            .request(&mut worker, request, 9, &egui::Context::default())
            .unwrap();
        let frame = wait(&mut presentation, source, doc.revision, 9);
        let (_, other) = capture(&session, 16);
        presentation
            .replies
            .complete(super::super::preview_worker::NativeReply {
                job: 9,
                tag: frame.tag(),
                result: Ok(frame),
            });
        assert!(presentation
            .take(other.key(), doc.revision, 9)
            .unwrap()
            .is_none());
        drop(other);
        drop(scene);
        drop(session);
        presentation.clear();
        worker.stop();
        drop(worker);
        drop(presentation);
        drop(runtime);
        drop(connection);
    }
}
