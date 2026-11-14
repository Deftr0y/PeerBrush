//! Source-bound pixel and geometry sampling on a bounded worker. Replies never edit state;
//! the input owner accepts its ticket or cancels it at the next gesture boundary.
use super::*;
use crate::{
    engine::{native::DocumentKey, scene::SceneSnapshot},
    psd::memory36::MetadataCharge,
    render::sample::Color,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Weak,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Ticket(u64);
struct Request {
    engine: Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    revision: u64,
    native: Option<DocumentKey>,
    point: [i32; 2],
    magnetic: Option<([f32; 2], f32)>,
    layer: String,
    mask: bool,
    step: Option<String>,
    _charge: Option<Arc<MetadataCharge>>,
}
impl Request {
    fn capture(
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        point: [f32; 2],
        layer: &str,
        mask: bool,
        step: Option<&str>,
    ) -> Result<Self, String> {
        let point = crate::render::sample::point([doc.width(), doc.height()], point)?;
        if mask && doc.find_layer(layer).is_none() {
            return Err("Sample mask layer no longer exists".into());
        }
        let charge = match doc {
            DocumentView::Resident(_) => None,
            DocumentView::Native(native) => {
                let bytes = [
                    project.len(),
                    doc.id().len(),
                    layer.len(),
                    step.map_or(0, str::len),
                ]
                .into_iter()
                .try_fold(2048usize, |n, v| {
                    n.checked_add(v).ok_or("Sample metadata exceeds admission")
                })?;
                if bytes > 64 * 1024 * 1024 {
                    return Err("Sample metadata exceeds admission".into());
                }
                Some(MetadataCharge::reserve(native.budget(), bytes, &|| Ok(()))?)
            }
        };
        Ok(Self {
            engine: Arc::downgrade(shared),
            project: project.into(),
            document: doc.id().into(),
            revision: doc.revision(),
            native: match doc {
                DocumentView::Native(native) => Some(native.key()),
                _ => None,
            },
            point,
            magnetic: None,
            layer: layer.into(),
            mask,
            step: step.map(str::to_owned),
            _charge: charge,
        })
    }
    fn source_matches(&self, shared: &Shared, project: &str, doc: DocumentView<'_>) -> bool {
        self.engine.as_ptr() == Arc::as_ptr(shared)
            && self.project == project
            && self.document == doc.id()
            && self.revision == doc.revision()
            && self.native
                == match doc {
                    DocumentView::Native(native) => Some(native.key()),
                    _ => None,
                }
    }
    fn target_matches(&self, layer: &str, mask: bool, step: Option<&str>) -> bool {
        self.layer == layer && self.mask == mask && self.step.as_deref() == step
    }
}
struct Pending<T> {
    ticket: Ticket,
    request: Arc<Request>,
    receiver: mpsc::Receiver<Result<T, String>>,
    ready: Option<Result<T, String>>,
    repaint: egui::Context,
}
pub(super) struct Sampler<T = Color> {
    worker: preview_worker::Worker,
    epoch: Arc<AtomicU64>,
    pending: Option<Pending<T>>,
    #[cfg(test)]
    test_gate: Option<(mpsc::Receiver<()>, mpsc::Sender<()>)>,
}
impl<T> Default for Sampler<T> {
    fn default() -> Self {
        let epoch = Arc::new(AtomicU64::new(0));
        Self {
            worker: preview_worker::Worker::new(epoch.clone()),
            epoch,
            pending: None,
            #[cfg(test)]
            test_gate: None,
        }
    }
}
impl<T: Send + 'static> Sampler<T> {
    #[cfg(test)]
    pub(super) fn hold_next(&mut self) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
        let (release, gate) = mpsc::channel();
        let (entered, running) = mpsc::channel();
        self.test_gate = Some((gate, entered));
        (release, running)
    }

    #[cfg(test)]
    pub(super) fn wait_ready(&mut self) {
        let pending = self.pending.as_mut().expect("Pending sample");
        if pending.ready.is_none() {
            pending.ready = Some(pending.receiver
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("Sample reply"));
        }
    }
    pub(super) fn cancel(&mut self) {
        self.epoch.fetch_add(1, Ordering::Relaxed);
        self.pending = None;
    }
    pub(super) fn stop(&mut self) {
        self.cancel();
        self.worker.stop();
    }
    pub(super) fn take(
        &mut self,
        ticket: Ticket,
        shared: &Shared,
        project: &str,
        layer: &str,
        mask: bool,
        step: Option<&str>,
    ) -> Result<Option<T>, String> {
        if self.pending.as_ref().is_some_and(|pending| {
            pending.ticket == ticket && !pending.request.target_matches(layer, mask, step)
        }) {
            self.cancel();
            return Err("Sample target changed; current work preserved".into());
        }
        self.take_captured(ticket, shared, project)
    }
    pub(super) fn take_captured(
        &mut self,
        ticket: Ticket,
        shared: &Shared,
        project: &str,
    ) -> Result<Option<T>, String> {
        let Some(pending) = &mut self.pending else {
            return Ok(None);
        };
        if pending.ticket != ticket {
            return Ok(None);
        }
        // In-flight sampling does not need to take the Engine lock on every
        // UI poll. Hold a completed reply until its source can be verified.
        if pending.ready.is_none() {
            pending.ready = Some(match pending.receiver.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return Ok(None),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Err("Sampling worker stopped before completion".into())
                }
            });
        }
        // Retain a ready reply while another client owns the engine lock.
        let engine = match shared.try_lock() {
            Ok(engine) => engine,
            Err(std::sync::TryLockError::WouldBlock) => {
                pending.repaint.request_repaint_after(Duration::from_millis(16));
                return Ok(None);
            }
            Err(_) => {
                self.cancel();
                return Err("Project is unavailable; current work preserved".into());
            }
        };
        let current = engine.ensure_open().is_ok()
            && engine.project_id == project
            && pending.request.source_matches(
                shared,
                project,
                crate::engine::view::current(&engine),
            );
        drop(engine);
        if !current {
            self.cancel();
            return Err("Sample source or target changed; current work preserved".into());
        }
        let result = pending.ready.take().ok_or("Sampling result disappeared")?;
        self.pending = None;
        result.map(Some)
    }
    fn queue(
        &mut self,
        request: Arc<Request>,
        ctx: &egui::Context,
        work: impl FnOnce(&dyn Fn() -> Result<(), String>) -> Result<T, String> + Send + 'static,
    ) -> Result<Ticket, String> {
        let ticket = Ticket(self.epoch.fetch_add(1, Ordering::Relaxed).wrapping_add(1));
        let epoch = self.epoch.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        let failed = sender.clone();
        let repaint = ctx.clone();
        let failed_repaint = ctx.clone();
        self.worker
            .request(
                move || {
                    let check = || {
                        if epoch.load(Ordering::Relaxed) == ticket.0 {
                            Ok(())
                        } else {
                            Err("Sample superseded".into())
                        }
                    };
                    let result = check().and_then(|_| work(&check)).and_then(|pixel| {
                        check()?;
                        Ok(pixel)
                    });
                    if sender.try_send(result).is_ok() {
                        repaint.request_repaint();
                    }
                },
                move || {
                    if failed.try_send(Err("Sampling worker could not complete".into())).is_ok() {
                        failed_repaint.request_repaint();
                    }
                },
            )
            .map_err(|_| "Sampling worker could not start")?;
        self.pending = Some(Pending {
            ticket,
            request,
            receiver,
            ready: None,
            repaint: ctx.clone(),
        });
        Ok(ticket)
    }
}
impl Sampler<Color> {
    pub(super) fn request(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        point: [f32; 2],
        layer: &str,
        mask: bool,
        step: Option<&str>,
        ctx: &egui::Context,
    ) -> Result<Ticket, String> {
        let point_key = match crate::render::sample::point([doc.width(), doc.height()], point) {
            Ok(point) => point,
            Err(error) => {
                self.cancel();
                return Err(error);
            }
        };
        if let Some(pending) = self.pending.as_ref().filter(|pending| {
            pending.request.source_matches(shared, project, doc)
                && pending.request.target_matches(layer, mask, step)
                && pending.request.point == point_key
        }) {
            return Ok(pending.ticket);
        }
        self.cancel();
        let request = Arc::new(Request::capture(
            shared, project, doc, point, layer, mask, step,
        )?);
        let input = request.clone();
        let shared = shared.clone();
        #[cfg(test)]
        let gate = self.test_gate.take();
        self.queue(request, ctx, move |check| {
            #[cfg(test)]
            if let Some((gate, entered)) = gate {
                let _ = entered.send(());
                gate.recv().map_err(|_| "Sampling gate was cancelled")?;
                check()?;
            }
            render(&shared, &input, check)
        })
    }
}

enum Capture {
    Resident(Arc<Document>),
    Native(Arc<SceneSnapshot>),
}
impl Sampler<crate::render::mask_thumbnail::Thumbnail> {
    pub(super) fn request_layer_thumbnail(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        layer: &str,
        mask: bool,
        ctx: &egui::Context,
    ) -> Result<Ticket, String> {
        if doc.native().is_none() {
            return Err("Layer thumbnail requires a native source".into());
        }
        let request = Arc::new(Request::capture(
            shared,
            project,
            doc,
            [0., 0.],
            layer,
            mask,
            None,
        )?);
        let input = request.clone();
        let shared = shared.clone();
        self.queue(request, ctx, move |check| {
            let Capture::Native(scene) = capture(&shared, &input, check)? else {
                return Err("Layer thumbnail storage changed".into());
            };
            let session = crate::workspace::native_dispatch::begin_read_scene(&shared, &scene)?;
            let request = crate::render::native_preview::Request::capture(
                scene,
                session,
                None,
                48,
                Some(&input.layer),
                input.mask,
            )?;
            let frame = request.render(&mut || check())?;
            let pixels = frame.copy_rgba8(&mut || check())?;
            check()?;
            Ok(crate::render::mask_thumbnail::Thumbnail {
                size: [frame.key().width, frame.key().height],
                pixels,
            })
        })
    }
    pub(super) fn request_mask_thumbnail(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        layer: &str,
        step: &str,
        ctx: &egui::Context,
    ) -> Result<Ticket, String> {
        if doc.native().is_none() {
            return Err("Mask thumbnail requires a native source".into());
        }
        let request = Arc::new(Request::capture(
            shared,
            project,
            doc,
            [0., 0.],
            layer,
            true,
            Some(step),
        )?);
        let input = request.clone();
        let shared = shared.clone();
        self.queue(request, ctx, move |check| {
            let Capture::Native(scene) = capture(&shared, &input, check)? else {
                return Err("Mask thumbnail source storage changed".into());
            };
            let session = crate::workspace::native_dispatch::begin_read_scene(&shared, &scene)?;
            crate::render::mask_thumbnail::render(
                &scene,
                &session,
                &input.layer,
                input.step.as_deref().ok_or("Mask thumbnail step missing")?,
                check,
            )
        })
    }
}
impl Sampler<[f32; 2]> {
    pub(super) fn request_magnetic(
        &mut self,
        shared: &Shared,
        project: &str,
        doc: DocumentView<'_>,
        point: [f32; 2],
        width: f32,
        layer: &str,
        ctx: &egui::Context,
    ) -> Result<Ticket, String> {
        if let Err(error) = crate::render::magnetic::area([doc.width(), doc.height()], point, width)
        {
            self.cancel();
            return Err(error);
        }
        if let Some(pending) = self.pending.as_ref().filter(|pending| {
            pending.request.source_matches(shared, project, doc)
                && pending.request.target_matches(layer, false, None)
                && pending.request.magnetic == Some((point, width))
        }) {
            return Ok(pending.ticket);
        }
        self.cancel();
        let mut request = Request::capture(shared, project, doc, point, layer, false, None)?;
        request.magnetic = Some((point, width));
        let request = Arc::new(request);
        let input = request.clone();
        let shared = shared.clone();
        #[cfg(test)]
        let gate = self.test_gate.take();
        self.queue(request, ctx, move |check| {
            #[cfg(test)]
            if let Some((gate, entered)) = gate {
                let _ = entered.send(());
                gate.recv().map_err(|_| "Sampling gate was cancelled")?;
                check()?;
            }
            match capture(&shared, &input, check)? {
                Capture::Resident(doc) => {
                    crate::render::magnetic::resident(&doc, point, width, &mut || check())
                }
                Capture::Native(scene) => {
                    let session =
                        crate::workspace::native_dispatch::begin_read_scene(&shared, &scene)?;
                    crate::render::magnetic::native(scene, session, point, width, &mut || check())
                }
            }
        })
    }
}
fn capture(
    shared: &Shared,
    request: &Request,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Capture, String> {
    check()?;
    let capture = {
        let engine = loop {
            check()?;
            match shared.try_lock() {
                Ok(engine) => break engine,
                Err(std::sync::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(2))
                }
                Err(_) => return Err("Project is unavailable; sampling cancelled".into()),
            }
        };
        engine.ensure_open()?;
        let view = crate::engine::view::current(&engine);
        if !request.source_matches(shared, &engine.project_id, view) {
            return Err("Sample source changed before capture".into());
        }
        match view {
            DocumentView::Resident(_) => {
                let DocumentSnapshot::Resident(doc) = engine.doc_snapshot() else {
                    return Err("Sample storage changed during capture".into());
                };
                Capture::Resident(doc)
            }
            DocumentView::Native(_) => {
                Capture::Native(Arc::new(engine.capture_scene(None)?.into_native()?))
            }
        }
    };
    check()?;
    Ok(capture)
}
fn render(
    shared: &Shared,
    request: &Request,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Color, String> {
    let capture = capture(shared, request, check)?;
    let target = request.mask.then_some(request.layer.as_str());
    let pixel = match capture {
        Capture::Resident(doc) => crate::render::sample::resident_color(&doc, request.point, target, check)?,
        Capture::Native(scene) => {
            let session = crate::workspace::native_dispatch::begin_read_scene(shared, &scene)?;
            crate::render::sample::native_color(scene, session, request.point, target, &mut || check())?
        }
    };
    check()?;
    Ok(pixel)
}

pub(super) struct Pick {
    ticket: Ticket,
    color: Pixel,
    color16: Option<crate::raster::Pixel16>,
    mask_value: u8,
    mask_value16: Option<u16>,
}

pub(super) struct Press {
    source: selection::GestureSource,
    tool: Tool,
    mask: bool,
    step: Option<String>,
    workspace: bool,
}

impl PeerBrush {
    pub(super) fn canvas_sample_release(
        &mut self,
        ui: &egui::Ui,
        doc: DocumentView<'_>,
        response: &egui::Response,
        projection: &view::Projection,
        mut held: Option<Press>,
    ) -> Option<[f32; 2]> {
        if !canvas_sample_input(ui) || ui.input(|input| input.pointer.middle_down() || input.pointer.secondary_down()) {
            return None;
        }
        let pixel_exists = |point| crate::render::sample::point([doc.width(), doc.height()], point).is_ok();
        let (press, release) = ui.input(|input| canvas_button_samples(input, held.is_some()));
        if let Some(pos) = press.filter(|&pos| canvas_press_hits(ui, response, pos)) {
            let source = match self.canvas_gesture_source(doc) {
                Ok(source) => source,
                Err(error) => {
                    self.message = error.into();
                    return None;
                }
            };
            held = Some(Press {
                source,
                tool: self.tool,
                mask: self.mask,
                step: self.mask_step.clone(),
                workspace: !pixel_exists(projection.document(pos)),
            });
        }
        let held = held.filter(|held| held.source.observes(self, doc)
            && held.tool == self.tool && held.mask == self.mask && held.step == self.mask_step)?;
        if let Some((pos, _)) = release {
            let point = projection.document(pos);
            (canvas_press_hits(ui, response, pos) && pixel_exists(point)
                && (held.workspace || response.clicked_by(egui::PointerButton::Primary))).then_some(point)
        } else {
            if ui.input(|input| input.pointer.primary_down()) {
                self.sample_press = Some(held);
            }
            None
        }
    }

    pub(super) fn cancel_color_pick(&mut self) {
        self.color_sampler.cancel();
        self.color_pick = None;
    }
    pub(super) fn queue_color_pick(
        &mut self,
        ctx: &egui::Context,
        doc: DocumentView<'_>,
        point: [f32; 2],
    ) {
        match self.color_sampler.request(
            &self.shared,
            &self.project_id,
            doc,
            point,
            &self.selected,
            self.mask,
            self.mask_step.as_deref(),
            ctx,
        ) {
            Ok(ticket) => {
                #[cfg(test)]
                if self
                    .color_pick
                    .as_ref()
                    .is_none_or(|pick| pick.ticket != ticket)
                {
                    self.color_pick_runs += 1;
                }
                self.color_pick = Some(Pick {
                    ticket,
                    color: self.color,
                    color16: self.color16,
                    mask_value: self.mask_value,
                    mask_value16: self.mask_value16,
                })
            }
            Err(error) => {
                self.cancel_color_pick();
                self.message = error.into();
            }
        }
    }
    pub(super) fn poll_color_pick(&mut self, ctx: &egui::Context) {
        let Some(pick) = &self.color_pick else {
            return;
        };
        if self.tool != Tool::Picker
            || self.color != pick.color
            || self.color16 != pick.color16
            || self.mask_value != pick.mask_value
            || self.mask_value16 != pick.mask_value16
            || ctx.input(|input| !input.focused || input.key_pressed(egui::Key::Escape))
        {
            self.cancel_color_pick();
            return;
        }
        match self.color_sampler.take(
            pick.ticket,
            &self.shared,
            &self.project_id,
            &self.selected,
            self.mask,
            self.mask_step.as_deref(),
        ) {
            Ok(Some(pixel)) => {
                self.accept_color_sample(pixel);
                self.color_pick = None;
            }
            Ok(None) => {}
            Err(error) => {
                self.color_pick = None;
                self.message = error.into();
            }
        }
    }

    pub(super) fn accept_color_sample(&mut self, sample: Color) {
        let pixel = sample.display();
        if self.mask {
            self.mask_value = pixel[0];
            self.mask_value16 = sample.words().map(|rgba| rgba[0]);
        } else {
            self.color = pixel;
            self.color16 = sample.words();
        }
    }
}

#[cfg(test)]
#[path = "color_sample_tests.rs"]
mod tests;
