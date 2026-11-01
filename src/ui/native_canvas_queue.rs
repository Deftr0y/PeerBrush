//! Captured native canvas work stays on the shared serial preview worker.
use super::{edit_job::Commands, native_texture, preview_worker::Worker};
use crate::{
    engine::native::DocumentKey,
    large_document::{Reservation, ResidentBudget},
    render::native_preview::{self, Origin, SourceSupplement, Tag},
    server::Shared,
};
use eframe::egui;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, Mutex,
};

type Result<T> = std::result::Result<T, String>;
pub(super) struct Spec<'a> {
    pub project: &'a str,
    pub document: &'a str,
    pub key: DocumentKey,
    pub revision: u64,
    pub rect: Option<[i32; 4]>,
    pub edge: u32,
    pub target: Option<&'a str>,
    pub mask: bool,
}
struct Expected {
    project: String,
    document: String,
    key: DocumentKey,
    revision: u64,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<String>,
    mask: bool,
    job: u64,
    _charge: Reservation,
}
pub(super) enum Binding {
    Committed(Tag),
    Transient(Arc<Origin>),
}
impl Binding {
    pub(super) fn tag(&self) -> &Tag {
        match self {
            Self::Committed(tag) => tag,
            Self::Transient(origin) => origin.candidate_tag(),
        }
    }
    fn matches(&self, expected: &Expected) -> bool {
        match self {
            Self::Committed(tag) => {
                tag.key().source == expected.key
                    && tag.key().identity.project_id == expected.project
                    && tag.key().identity.document_id == expected.document
                    && tag.key().identity.revision == expected.revision
            }
            Self::Transient(origin) => {
                origin.matches_source(&expected.project, expected.key, expected.revision)
            }
        }
    }
}
pub(super) struct Output {
    pub binding: Binding,
    pub uploads: native_texture::Prepared,
    pub source: Option<SourceSupplement>,
}
struct Reply {
    job: u64,
    result: Result<Output>,
}
pub(super) struct Queue {
    expected: Option<Arc<Expected>>,
    reply: Arc<Mutex<Option<Reply>>>,
    retirement: native_texture::Retirement,
    pool: ResidentBudget,
}
impl Queue {
    pub(super) fn new(pool: &ResidentBudget) -> Result<Self> {
        Ok(Self {
            expected: None,
            reply: Default::default(),
            retirement: native_texture::Retirement::new(pool)?,
            pool: pool.clone(),
        })
    }
    pub(super) fn uses_pool(&self, pool: &ResidentBudget) -> bool {
        self.pool.same_pool(pool)
    }
    pub(super) fn request(
        &mut self,
        worker: &mut Worker,
        shared: &Shared,
        spec: Spec<'_>,
        commands: Commands,
        job: u64,
        epoch: Arc<AtomicU64>,
        side: usize,
        ctx: &egui::Context,
    ) -> Result<()> {
        let bytes = spec
            .project
            .len()
            .checked_add(spec.document.len())
            .and_then(|n| n.checked_add(spec.target.map_or(0, str::len)))
            .and_then(|n| n.checked_add(std::mem::size_of::<Expected>() + 1024))
            .ok_or("Native preview metadata overflow")?;
        let charge = self
            .pool
            .reserve_bytes(bytes, 64 * 1024)
            .map_err(|e| e.to_string())?;
        let expected = Arc::new(Expected {
            project: spec.project.into(),
            document: spec.document.into(),
            key: spec.key,
            revision: spec.revision,
            rect: spec.rect,
            edge: spec.edge,
            target: spec.target.map(str::to_owned),
            mask: spec.mask,
            job,
            _charge: charge,
        });
        let input = expected.clone();
        // A queued job has no scene/session or strong document handle. Only the
        // active actor acquires an authenticated source operation.
        let shared = Arc::downgrade(shared);
        let retirement = self.retirement.clone();
        let reply = self.reply.clone();
        let failed = self.reply.clone();
        let repaint = ctx.clone();
        let failed_repaint = ctx.clone();
        worker
            .request(
                move || {
                    let check = || {
                        if epoch.load(Ordering::Relaxed) == job {
                            Ok(())
                        } else {
                            Err("Native preview superseded".into())
                        }
                    };
                    let result = (|| {
                        check()?;
                        let shared = shared.upgrade().ok_or("Preview project was closed")?;
                        let (binding, frame, source) = if commands.values().is_empty() {
                            let captured = {
                                let engine =
                                    shared.lock().map_err(|_| "Preview engine is unavailable")?;
                                engine.ensure_open()?;
                                engine.capture_scene(None)?
                            };
                            let scene = captured.into_native()?;
                            if scene.key() != input.key
                                || scene.identity().project_id != input.project
                                || scene.identity().document_id != input.document
                                || scene.identity().revision != input.revision
                            {
                                return Err("Native preview baseline changed".into());
                            }
                            let session = crate::workspace::native_dispatch::begin_read_scene(
                                &shared, &scene,
                            )?;
                            let request = native_preview::Request::capture(
                                Arc::new(scene),
                                session,
                                input.rect,
                                input.edge,
                                input.target.as_deref(),
                                input.mask,
                            )?;
                            let tag = request.tag();
                            let frame = request.render(&mut || check())?;
                            drop(request);
                            (Binding::Committed(tag), frame, None)
                        } else {
                            let rendered = native_preview::preview_shared(
                                &shared,
                                "human",
                                commands.values(),
                                Some(input.revision),
                                Some(&input.document),
                                input.rect,
                                input.edge,
                                input.target.as_deref(),
                                input.mask,
                                &check,
                            )?;
                            let (origin, frame, source) = rendered.into_parts();
                            if !origin.matches_frame(&frame) {
                                return Err(
                                    "Native candidate pixels do not match their origin".into()
                                );
                            }
                            (Binding::Transient(origin), frame, source)
                        };
                        if !binding.matches(&input) {
                            return Err("Native preview belongs to another baseline".into());
                        }
                        check()?;
                        let uploads = native_texture::Prepared::from_frame(
                            frame,
                            &retirement,
                            side,
                            &mut || check(),
                        )?;
                        check()?;
                        Ok(Output {
                            binding,
                            uploads,
                            source,
                        })
                    })();
                    let previous = reply
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace(Reply { job, result });
                    drop(previous);
                    repaint.request_repaint();
                },
                move || {
                    let previous = failed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .replace(Reply {
                        job,
                        result: Err(
                            "Native preview worker could not complete; previous canvas retained"
                                .into(),
                        ),
                    });
                    drop(previous);
                    failed_repaint.request_repaint();
                },
            )
            .map_err(|_| "Native preview worker could not start")?;
        self.expected = Some(expected);
        Ok(())
    }
    pub(super) fn take(
        &mut self,
        project: &str,
        key: DocumentKey,
        revision: u64,
        job: u64,
    ) -> Result<Option<Output>> {
        let Some(reply) = self.reply.lock().unwrap_or_else(|e| e.into_inner()).take() else {
            return Ok(None);
        };
        let Some(expected) = &self.expected else {
            return Ok(None);
        };
        if expected.job != job
            || reply.job != job
            || expected.project != project
            || expected.key != key
            || expected.revision != revision
        {
            return Ok(None);
        }
        let output = reply.result?;
        if !output.binding.matches(expected)
            || output.uploads.tag().key() != output.binding.tag().key()
        {
            return Err("Native display reply does not match its captured origin".into());
        }
        Ok(Some(output))
    }
    pub(super) fn clear(&mut self) {
        self.expected = None;
        drop(self.reply.lock().unwrap_or_else(|e| e.into_inner()).take());
    }
}
