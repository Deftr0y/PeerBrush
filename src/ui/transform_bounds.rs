//! A bounded geometry query worker. Replies contain no document or source session.
use super::*;
use crate::{
    engine::native::{DocumentKey, NativeDocument},
    large_document::Error,
    psd::memory36::MetadataCharge,
    workspace::native_dispatch::{begin_read_scene, retryable_source_read, try_source_engine},
};
use std::cell::RefCell;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Weak,
};
use std::time::Instant;

#[derive(Clone)]
pub(super) struct Geometry {
    pub bounds: [i32; 4],
    pub warp_domains: [Result<[f64; 4], String>; 2],
}
struct Stamp {
    engine: Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    source: DocumentKey,
    revision: u64,
    roots: Vec<String>,
    mask: bool,
    step: Option<String>,
    _charge: Arc<MetadataCharge>,
}
impl Stamp {
    fn capture(
        shared: &Shared,
        project: &str,
        doc: &NativeDocument,
        roots: &[String],
        mask: bool,
        step: Option<&str>,
    ) -> Result<Arc<Self>, String> {
        if roots.is_empty() || roots.len() > 100 {
            return Err("Choose 1–100 transform targets".into());
        }
        let strings = roots
            .iter()
            .map(|s| s.len())
            .chain([
                project.len(),
                doc.metadata().id().len(),
                step.map_or(0, str::len),
            ])
            .try_fold(0usize, |n, v| {
                n.checked_add(v).ok_or("Transform query metadata overflow")
            })?;
        let bytes = strings
            .checked_mul(8)
            .and_then(|v| v.checked_add(roots.len() * 2048 + 2048))
            .filter(|v| *v <= 64 * 1024 * 1024)
            .ok_or("Transform query metadata exceeds admission")?;
        let charge = MetadataCharge::reserve(doc.budget(), bytes, &|| Ok(()))?;
        Ok(Arc::new(Self {
            engine: Arc::downgrade(shared),
            project: project.into(),
            document: doc.metadata().id().into(),
            source: doc.key(),
            revision: doc.metadata().revision(),
            roots: roots.to_vec(),
            mask,
            step: step.map(str::to_owned),
            _charge: charge,
        }))
    }
    fn matches(
        &self,
        shared: &Shared,
        project: &str,
        doc: &NativeDocument,
        roots: &[String],
        mask: bool,
        step: Option<&str>,
    ) -> bool {
        self.engine.as_ptr() == Arc::as_ptr(shared)
            && self.project == project
            && self.document == doc.metadata().id()
            && self.source == doc.key()
            && self.revision == doc.metadata().revision()
            && self.roots == roots
            && self.mask == mask
            && self.step.as_deref() == step
    }
}
pub(super) struct Query {
    worker: preview_worker::Worker,
    epoch: Arc<AtomicU64>,
    stamp: Option<Arc<Stamp>>,
    receiver: Option<mpsc::Receiver<Result<Geometry, String>>>,
    result: Option<Result<Geometry, String>>,
    retry_at: Option<Instant>,
}
impl Default for Query {
    fn default() -> Self {
        let epoch = Arc::new(AtomicU64::new(0));
        Self {
            worker: preview_worker::Worker::new(epoch.clone()),
            epoch,
            stamp: None,
            receiver: None,
            result: None,
            retry_at: None,
        }
    }
}
impl Query {
    pub(super) fn clear(&mut self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.receiver = None;
        self.result = None;
        self.retry_at = None;
        self.stamp = None;
    }
    pub(super) fn stop(&mut self) {
        self.worker.stop();
        self.clear();
    }
    pub(super) fn get(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: &NativeDocument,
        roots: &[String],
        mask: bool,
        step: Option<&str>,
        ctx: &egui::Context,
    ) -> Result<Option<Geometry>, String> {
        self.poll();
        if self
            .stamp
            .as_ref()
            .is_none_or(|s| !s.matches(shared, project, doc, roots, mask, step))
            || self.retry_at.is_some_and(|retry| Instant::now() >= retry)
        {
            self.clear();
            let stamp = Stamp::capture(shared, project, doc, roots, mask, step)?;
            let shared = shared.clone();
            let input = stamp.clone();
            self.queue(stamp, ctx, move |check| prepare(&shared, &input, check))?;
        }
        if self.result.is_none() {
            let delay = self.retry_at.map_or(Duration::from_millis(40), |retry| {
                retry.saturating_duration_since(Instant::now())
            });
            ctx.request_repaint_after(delay);
        }
        self.result.clone().transpose()
    }
    fn poll(&mut self) {
        if let Some(receiver) = &self.receiver {
            match receiver.try_recv() {
                Ok(result) => {
                    self.receiver = None;
                    if result.as_ref().is_err_and(|error| retryable_source_read(error)) {
                        self.retry_at = Some(Instant::now() + INVENTORY_RETRY_DELAY);
                    } else {
                        self.result = Some(result);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.result = Some(Err(
                        "Transform geometry worker stopped before completion".into()
                    ));
                    self.receiver = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }
    fn queue(
        &mut self,
        stamp: Arc<Stamp>,
        ctx: &egui::Context,
        work: impl FnOnce(&dyn Fn() -> crate::large_document::Result<()>) -> Result<Geometry, String>
            + Send
            + 'static,
    ) -> Result<(), String> {
        let job = self.epoch.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let epoch = self.epoch.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let failed = sender.clone();
        let repaint = ctx.clone();
        let failed_repaint = ctx.clone();
        self.worker
            .request(
                move || {
                    let check = || {
                        if epoch.load(Ordering::Relaxed) == job {
                            Ok(())
                        } else {
                            Err(Error::Cancelled)
                        }
                    };
                    let result = check()
                        .map_err(|e| e.to_string())
                        .and_then(|_| work(&check))
                        .and_then(|value| {
                            check().map_err(|e| e.to_string())?;
                            Ok(value)
                        });
                    let _ = sender.try_send(result);
                    repaint.request_repaint();
                },
                move || {
                    let _ =
                        failed.try_send(Err("Transform geometry worker could not complete".into()));
                    failed_repaint.request_repaint();
                },
            )
            .map_err(|_| "Transform geometry worker could not start")?;
        self.stamp = Some(stamp);
        self.receiver = Some(receiver);
        self.result = None;
        self.retry_at = None;
        Ok(())
    }
}
fn prepare(
    shared: &Shared,
    stamp: &Stamp,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Geometry, String> {
    check().map_err(|e| e.to_string())?;
    let scene = {
        let engine = try_source_engine(shared)?;
        engine.ensure_open()?;
        let DocumentView::Native(doc) = crate::engine::view::current(&engine) else {
            return Err("Native transform authority changed".into());
        };
        if !stamp.matches(
            shared,
            &engine.project_id,
            doc,
            &stamp.roots,
            stamp.mask,
            stamp.step.as_deref(),
        ) {
            return Err("Transform source changed before geometry capture".into());
        }
        engine.capture_scene(None)?.into_native()?
    };
    let session = begin_read_scene(shared, &scene)?;
    let source_error = RefCell::new(None);
    let checked = || {
        check()?;
        session.check_scene(&scene).map_err(|error| {
            source_error.borrow_mut().get_or_insert(error);
            Error::Changed
        })
    };
    let result = geometry(
        scene.native_document()?,
        &stamp.roots,
        stamp.mask,
        stamp.step.as_deref(),
        &checked,
    );
    source_error.into_inner().map_or(result, Err)
}
fn geometry(
    doc: &NativeDocument,
    roots: &[String],
    mask: bool,
    step: Option<&str>,
    check: &dyn Fn() -> crate::large_document::Result<()>,
) -> Result<Geometry, String> {
    let bounds = crate::transform::native::tree_bounds(
        doc,
        roots,
        &mut crate::transform::native::Work::new(128_000_000)?,
        check,
    )?
    .unwrap_or([
        0,
        0,
        doc.metadata().width() as i32,
        doc.metadata().height() as i32,
    ]);
    let warp_domains = [false, true].map(|whole| {
        let commands = roots.iter().map(|id| json!({"op":"transform","layer":id,"mask":mask,
            "step":if roots.len()==1 {step}else{None},"selection_only":doc.metadata().selection().is_some()&&!whole})).collect::<Vec<_>>();
        crate::warp::domain(doc, &commands, check)
    });
    check().map_err(|e| e.to_string())?;
    Ok(Geometry {
        bounds,
        warp_domains,
    })
}

#[cfg(test)]
#[path = "transform_bounds_tests.rs"]
mod tests;
