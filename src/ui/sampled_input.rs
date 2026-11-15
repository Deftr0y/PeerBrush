//! Canvas adapter for the ordered sample/stroke timeline. Raster work stays on
//! the sampling worker; finished gestures use the shared edit worker.
use super::sample_input::{Action, Id, Timeline};
use super::*;
use crate::render::sample::Color;

pub(super) struct Input {
    sample: Option<(Id, color_sample::Ticket, AltSample)>,
    stroke: Option<(Id, egui::Id)>,
    sample_pixel: Option<Color>,
    receipt: Option<Result<u64, String>>,
    latest: Id,
    color: Pixel,
    color16: Option<crate::raster::Pixel16>,
    mask_value: u8,
    mask_value16: Option<u16>,
    color_ready: bool,
    error: Option<String>,
    preview: Option<(sample_input::PreviewStamp, sample_input::Preview)>,
    timeline: Timeline,
}
impl Input {
    pub(super) fn preview_commands(&self) -> &[crate::psd::memory36::Admitted<Value>] {
        self.preview
            .as_ref()
            .map_or(&[], |(_, preview)| preview.commands.as_slice())
    }
    fn palette_matches(&self, app: &PeerBrush) -> bool {
        app.color == self.color && app.color16 == self.color16
            && app.mask_value == self.mask_value && app.mask_value16 == self.mask_value16
    }
}
impl PeerBrush {
    pub(super) fn sampled_publication_pending(&self, doc: DocumentView<'_>) -> bool {
        self.sampled_input.as_ref().is_some_and(|input| {
            input.error.is_none()
                && input.stroke.is_some()
                && input
                    .timeline
                    .awaiting_publication(&self.shared, &self.project_id, doc)
        })
    }
    pub(super) fn sampled_input_pending(&self) -> bool {
        self.sampled_input
            .as_ref()
            .is_some_and(|input| input.timeline.pending() || input.error.is_some())
    }
    pub(super) fn sampled_canvas_input(&self) -> bool {
        self.tool == Tool::Brush
            && self.lifecycle.is_none()
            && self.save_loss_review.is_none()
            && !self.show_new
            && self
                .sampled_input
                .as_ref()
                .map_or(self.sampled_input_frame, |input| input.error.is_none())
            && (!self.edit_worker.pending()
                || self.edit_worker.request().is_some_and(|request| {
                    matches!(request.origin, edit_job::Origin::SampledStroke(_))
                }))
    }
    pub(super) fn sampled_line_anchor(
        &self,
        doc: DocumentView<'_>,
    ) -> Option<Option<([f32; 2], f32)>> {
        self.sampled_input
            .as_ref()
            .filter(|input| input.timeline.has_strokes())
            .map(|input| {
                input
                    .timeline
                    .last_endpoint(
                        &self.shared,
                        &self.project_id,
                        doc,
                        &self.selected,
                        self.mask,
                        self.mask_step.as_deref(),
                    )
                    .ok()
                    .flatten()
            })
    }
    pub(super) fn sampled_stroke_waiting(&self) -> bool {
        self.tool == Tool::Brush
            && self
                .sampled_input
                .as_ref()
                .is_some_and(|input| !input.color_ready && input.palette_matches(self))
    }
    pub(super) fn cancel_sampled_input(&mut self) {
        self.sampled_input_frame = false;
        self.alt_sampler.cancel();
        if let Some((_, token)) = self.sampled_input.as_ref().and_then(|input| input.stroke) {
            self.cancel_pending_edit();
            self.stroke_finished(token, None);
        } else {
            self.message = "Sampled input cancelled".into();
        }
        self.sampled_input = None;
        self.alt_sample = None;
        self.points.clear();
        self.stroke_pressures.clear();
        self.stroke_has_pressure = false;
        self.current_pressure = None;
        self.drag_start = None;
        self.drag_revision = None;
        self.workspace_stroke = None;
        self.straight.cancel();
        self.straight.cancelled = true;
        self.live_gesture = None;
    }
    pub(super) fn sampled_focus_lost(&mut self) {
        self.sampled_input_frame = false;
        if !self
            .sampled_input
            .as_ref()
            .is_some_and(|input| input.timeline.has_strokes())
        {
            self.cancel_sampled_input();
        } else {
            self.alt_sample = None;
            self.points.clear();
            self.stroke_pressures.clear();
            self.stroke_has_pressure = false;
            self.current_pressure = None;
            self.drag_start = None;
            self.drag_revision = None;
            self.workspace_stroke = None;
            self.straight.cancel();
            self.straight.cancelled = true;
            self.live_gesture = None;
        }
    }
    pub(super) fn queue_alt_sample<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
        point: [f32; 2],
    ) {
        let doc = doc.into();
        let shared = self.shared.clone();
        let engine = if self.sampled_input.is_none() {
            match shared.try_lock() {
                Ok(engine) => Some(engine),
                Err(_) => {
                    ctx.request_repaint();
                    return;
                }
            }
        } else {
            None
        };
        let view = engine
            .as_ref()
            .map(|engine| crate::engine::view::current(engine))
            .unwrap_or(doc);
        if view.id() != doc.id() {
            return;
        }
        let Ok(point_key) = crate::render::sample::point([view.width(), view.height()], point)
        else {
            return;
        };
        if self.sampled_input.is_none() {
            if let Some(sample) = self.alt_sample.as_ref().filter(|sample| {
                sample.engine.as_ptr() == Arc::as_ptr(&self.shared)
                    && sample.project == self.project_id
                    && sample.document == doc.id()
                    && sample.revision == view.revision()
                    && sample.native == view.native().map(|doc| doc.key())
                    && sample.point == point_key
                    && sample.selected == self.selected
                    && sample.mask == self.mask
                    && sample.step == self.mask_step
            }) {
                self.accept_color_sample(sample.value);
                return;
            }
        }
        let result = (|| -> Result<(), String> {
            if let Some(input) = &mut self.sampled_input {
                if input.error.is_some() {
                    return Ok(());
                }
                let enqueued = input.timeline.sample(
                    &self.shared,
                    &self.project_id,
                    view,
                    point,
                    &self.selected,
                    self.mask,
                    self.mask_step.as_deref(),
                )?;
                if enqueued.superseded.is_some_and(|id| {
                    input
                        .sample
                        .as_ref()
                        .is_some_and(|(active, _, _)| *active == id)
                }) {
                    self.alt_sampler.cancel();
                    input.sample = None;
                    input.sample_pixel = None;
                }
                if input.latest != enqueued.id {
                    input.latest = enqueued.id;
                    input.color = self.color;
                    input.color16 = self.color16;
                    input.mask_value = self.mask_value;
                    input.mask_value16 = self.mask_value16;
                    input.color_ready = false;
                }
            } else {
                let mut timeline = Timeline::new(&self.shared, &self.project_id, view)?;
                let latest = timeline
                    .sample(
                        &self.shared,
                        &self.project_id,
                        view,
                        point,
                        &self.selected,
                        self.mask,
                        self.mask_step.as_deref(),
                    )?
                    .id;
                self.sampled_input = Some(Input {
                    timeline,
                    sample: None,
                    stroke: None,
                    sample_pixel: None,
                    receipt: None,
                    latest,
                    color: self.color,
                    color16: self.color16,
                    mask_value: self.mask_value,
                    mask_value16: self.mask_value16,
                    color_ready: false,
                    error: None,
                    preview: None,
                });
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.sampled_input_error(error);
        }
        drop(engine);
        self.dispatch_sampled_input(ctx);
    }
    fn sampled_input_error(&mut self, error: String) {
        self.message = error.clone().into();
        if let Some(input) = &mut self.sampled_input {
            input.error = Some(error);
        }
    }
    pub(super) fn enqueue_sampled_stroke(
        &mut self,
        command: &mut Value,
        label: &str,
    ) -> Result<bool, String> {
        let Some(mut input) = self.sampled_input.take() else {
            return Ok(false);
        };
        let result = (|| {
            if let Some(error) = &input.error {
                return Err(error.clone());
            }
            let view = self.doc_snapshot.view();
            command["document_id"] = json!(input.timeline.document());
            command["source_revision"] = json!(input.timeline.revision());
            let use_sample = input.palette_matches(self);
            input.timeline.stroke(
                &self.shared,
                &self.project_id,
                view,
                command,
                label,
                use_sample,
            )?;
            Ok(true)
        })();
        if let Err(error) = &result {
            input.error = Some(error.clone());
            self.message = error.clone().into();
        }
        if result.as_ref().is_ok_and(|accepted| *accepted) {
            self.message = "Waiting for sampled stroke…".into();
        }
        self.sampled_input = Some(input);
        result
    }
    pub(super) fn sampled_stroke_finished(&mut self, id: Id, result: Result<u64, String>) {
        if let Some(input) = self
            .sampled_input
            .as_mut()
            .filter(|input| input.stroke.is_some_and(|(active, _)| active == id))
        {
            input.receipt = Some(result);
        }
    }
    #[cfg(test)]
    pub(super) fn poll_sampled_input(&mut self, ctx: &egui::Context) {
        self.pump_sampled_input(ctx, true, true);
    }
    pub(super) fn process_sampled_input(&mut self, ctx: &egui::Context) {
        self.pump_sampled_input(ctx, true, false);
    }
    pub(super) fn dispatch_sampled_input(&mut self, ctx: &egui::Context) {
        self.pump_sampled_input(ctx, false, true);
    }
    fn pump_sampled_input(&mut self, ctx: &egui::Context, process: bool, dispatch: bool) {
        let Some(mut input) = self.sampled_input.take() else {
            return;
        };
        if input.error.is_some() {
            self.sampled_input = Some(input);
            return;
        }
        let shared = self.shared.clone();
        let frame = self.doc_snapshot.clone();
        let result = (|| -> Result<(), String> {
            if process {
                if let Some(receipt) = input.receipt.take() {
                    let (id, token) = input.stroke.unwrap();
                    match receipt {
                        Err(error) => {
                            input.timeline.failed(id);
                            input.stroke = None;
                            self.stroke_finished(token, None);
                            return Err(error);
                        }
                        Ok(revision) => {
                            let view = frame.view();
                            if view.id() == input.timeline.document() && view.revision() < revision
                            {
                                input.receipt = Some(Ok(revision));
                                return Ok(());
                            }
                            let previous = input.timeline.revision();
                            let accepted = input.timeline.committed(
                                id,
                                revision,
                                &shared,
                                &self.project_id,
                                view,
                            );
                            input.stroke = None;
                            self.stroke_finished(token, Some(revision));
                            accepted?;
                            if self.drag_revision == Some(previous)
                                && self.straight.document.as_deref() == Some(view.id())
                            {
                                self.drag_revision = Some(revision);
                                if let Some(stroke) = &mut self.workspace_stroke {
                                    stroke.advance_sampled_source(view, previous);
                                }
                            }
                        }
                    }
                }
                if let Some((_, ticket, _)) = &input.sample {
                    if input.sample_pixel.is_none() {
                        input.sample_pixel =
                            self.alt_sampler
                                .take_captured(*ticket, &shared, &self.project_id)?;
                    }
                    if let Some(pixel) = input.sample_pixel {
                        let (id, _, mut sample) = input.sample.take().unwrap();
                        input.sample_pixel = None;
                        input.timeline.sampled_color(
                            id,
                            pixel,
                            &shared,
                            &self.project_id,
                            frame.view(),
                        )?;
                        if id == input.latest {
                            input.color_ready = true;
                            let held = ctx.input(|i| {
                                i.focused && i.modifiers.alt && i.pointer.primary_down()
                            });
                            if self.tool == Tool::Brush
                                && self.selected == sample.selected
                                && self.mask == sample.mask
                                && self.mask_step == sample.step
                                && ctx.input(|i| i.focused)
                                && (held || input.palette_matches(self))
                            {
                                self.accept_color_sample(pixel);
                                input.color = self.color;
                                input.color16 = self.color16;
                                input.mask_value = self.mask_value;
                                input.mask_value16 = self.mask_value16;
                                if held {
                                    sample.value = pixel;
                                    self.alt_sample = Some(sample);
                                }
                            }
                        }
                    }
                }
            }
            if !dispatch {
                return Ok(());
            }
            if input.sample.is_some() || input.stroke.is_some() || self.edit_worker.pending() {
                return Ok(());
            }
            let engine = match shared.try_lock() {
                Ok(engine) => engine,
                Err(_) => return Ok(()),
            };
            let view = crate::engine::view::current(&engine);
            let Some(ready) = input.timeline.start(&shared, &self.project_id, view)? else {
                return Ok(());
            };
            match ready.action {
                Action::Sample {
                    point,
                    layer,
                    mask,
                    step,
                } => {
                    let charge = view
                        .native()
                        .map(|doc| {
                            let bytes = [
                                self.project_id.len(),
                                ready.document.len(),
                                layer.len(),
                                step.map_or(0, str::len),
                            ]
                            .into_iter()
                            .try_fold(1024usize, |size, count| {
                                size.checked_add(count)
                                    .ok_or("Sample cache metadata exceeds admission")
                            })?;
                            if bytes > 64 * 1024 * 1024 {
                                return Err("Sample cache metadata exceeds admission".to_owned());
                            }
                            crate::psd::memory36::MetadataCharge::reserve(
                                doc.budget(),
                                bytes,
                                &|| Ok(()),
                            )
                        })
                        .transpose()?;
                    let ticket = self.alt_sampler.request(
                        &shared,
                        &self.project_id,
                        view,
                        [point[0] as f32, point[1] as f32],
                        layer,
                        mask,
                        step,
                        ctx,
                    )?;
                    #[cfg(test)]
                    {
                        self.color_pick_runs += 1;
                    }
                    input.sample = Some((
                        ready.id,
                        ticket,
                        AltSample {
                            native: view.native().map(|doc| doc.key()),
                            _charge: charge,
                            engine: Arc::downgrade(&shared),
                            project: self.project_id.clone(),
                            document: ready.document.into(),
                            revision: ready.revision,
                            point,
                            selected: layer.into(),
                            mask,
                            step: step.map(str::to_owned),
                            value: Color::Byte([0; 4]),
                        },
                    ));
                }
                Action::Stroke {
                    command,
                    label,
                    endpoint,
                } => {
                    let budget = view.native().map(|doc| doc.budget().clone());
                    drop(engine);
                    let token = self.track_stroke_command(
                        &command,
                        endpoint,
                        ready.document,
                        ready.revision,
                        budget.as_ref(),
                    )?;
                    let request = edit_job::Request {
                        project: self.project_id.clone(),
                        document: ready.document.into(),
                        revision: ready.revision,
                        label: label.into(),
                        selection_edit: false,
                        origin: edit_job::Origin::SampledStroke(ready.id),
                    };
                    let id = ready.id;
                    if let Err(error) =
                        self.submit_edit_job(request, edit_job::Commands::Native(command), ctx)
                    {
                        self.stroke_finished(token, None);
                        return Err(error);
                    }
                    input.stroke = Some((id, token));
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            input.error = Some(error.clone());
            self.message = error.into();
        }
        if input.timeline.pending()
            || input.error.is_some()
            || input.stroke.is_some()
            || input.sample.is_some()
        {
            self.sampled_input = Some(input);
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
    pub(super) fn refresh_sampled_preview(&mut self, doc: DocumentView<'_>) {
        let Some(input) = &mut self.sampled_input else {
            return;
        };
        if input.error.is_some() {
            input.preview = None;
            return;
        }
        let result = (|| -> Result<(), String> {
            let stamp = input
                .timeline
                .preview_stamp(&self.shared, &self.project_id, doc)?;
            if input
                .preview
                .as_ref()
                .is_some_and(|(cached, _)| *cached == stamp)
            {
                return Ok(());
            }
            // Retire the previous derived copy before admitting its replacement.
            input.preview = None;
            input.preview = Some((
                stamp,
                input
                    .timeline
                    .preview(&self.shared, &self.project_id, doc)?,
            ));
            Ok(())
        })();
        if let Err(error) = result {
            input.preview = None;
            self.message = error.into();
        }
    }
    pub(super) fn sampled_preview_commands(&self) -> &[crate::psd::memory36::Admitted<Value>] {
        self.sampled_input
            .as_ref()
            .and_then(|input| input.preview.as_ref())
            .map_or(&[], |(_, preview)| preview.commands.as_slice())
    }
    pub(super) fn sampled_preview_key(&self) -> Option<String> {
        self.sampled_input
            .as_ref()
            .and_then(|input| input.preview.as_ref())
            .filter(|(_, preview)| !preview.commands.is_empty())
            .map(|(stamp, _)| format!("{stamp:?}"))
    }
    pub(super) fn sampled_input_window(&mut self, ctx: &egui::Context) {
        let Some(error) = self
            .sampled_input
            .as_ref()
            .and_then(|input| input.error.clone())
        else {
            return;
        };
        egui::Window::new("Sampled input paused")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(error);
                ui.label("Uncommitted strokes are retained. Discard them to continue editing.");
                if ui.button("Discard pending input").clicked() {
                    self.cancel_sampled_input();
                }
            });
    }
}

#[cfg(test)]
#[path = "sampled_input_tests.rs"]
pub(super) mod tests;
