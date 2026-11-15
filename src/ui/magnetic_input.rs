//! Ordered magnetic anchors. Hover can coalesce; clicked anchors cannot.
use super::*;
use crate::{engine::native::DocumentKey, psd::memory36::MetadataCharge};
use std::collections::VecDeque;

const LIMIT: usize = 8192;
struct Point {
    position: [f32; 2],
    width: f32,
    clicked: bool,
    snap: bool,
}

#[cfg(test)]
#[path = "magnetic_input_tests.rs"]
mod tests;
pub(super) struct Input {
    engine: std::sync::Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    revision: u64,
    native: Option<DocumentKey>,
    layer: String,
    queue: VecDeque<Point>,
    ticket: Option<color_sample::Ticket>,
    commit: Option<egui::Id>,
    completed: Option<([f32; 2], f32)>,
    closing: bool,
    error: Option<String>,
    // Includes the separately held selection_path; clear it before this owner.
    _charge: Option<Arc<MetadataCharge>>,
}
impl Input {
    fn matches(&self, app: &PeerBrush, doc: DocumentView<'_>) -> bool {
        self.engine.as_ptr() == Arc::as_ptr(&app.shared)
            && self.project == app.project_id
            && self.document == doc.id()
            && self.revision == doc.revision()
            && self.native == doc.native().map(|doc| doc.key())
            && self.layer == app.selected
    }
    fn complete_point(&mut self, position: [f32; 2], path: &mut Vec<[f32; 2]>) {
        let requested = self.queue.pop_front().expect("Pending magnetic point");
        self.completed = Some((requested.position, requested.width));
        if path.last() != Some(&position) {
            path.push(position);
        }
    }
}
impl PeerBrush {
    pub(super) fn native_magnetic_canvas(
        &mut self,
        ui: &egui::Ui,
        doc: DocumentView<'_>,
        response: &egui::Response,
        painter: &egui::Painter,
        projection: &view::Projection,
        click: Option<(Pos2, egui::Modifiers)>,
    ) {
        if ui.input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete)) {
            self.pop_magnetic_point();
        }
        if let Some((pos, modifiers)) = click {
            if !self.selection_path_pending() {
                self.selection_gesture_mode = Some(self.selection_mode_with(modifiers));
            }
            if let Err(error) = self.queue_magnetic_point(doc, projection.document(pos), true) {
                self.message = error.into();
            }
        } else if self.selection_path_pending() {
            if let Some(pos) = response.hover_pos().filter(|pos| projection.contains(*pos)) {
                if let Err(error) =
                    self.queue_magnetic_point(doc, projection.document(pos), false)
                {
                    self.message = error.into();
                }
            }
        }
        if (click.is_some() && response.double_clicked()) || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            self.close_magnetic_path();
        }
        self.poll_magnetic_path(ui.ctx(), doc);
        if !self.selection_path.is_empty() {
            let mut points = self
                .selection_path
                .iter()
                .map(|p| projection.point(*p))
                .collect::<Vec<_>>();
            if let Some(p) = response.hover_pos() {
                points.push(p);
            }
            painter.add(egui::Shape::line(points, Stroke::new(1., ACCENT)));
        }
        if self
            .magnetic_input
            .as_ref()
            .is_some_and(|input| input.ticket.is_some())
            && response.hovered()
        {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Progress);
        }
        if !self.selection_path_pending() && !ui.input(|i| i.pointer.primary_down()) {
            self.clear_selection_path();
            self.selection_gesture_source = None;
            self.selection_gesture_mode = None;
        }
    }
    pub(super) fn selection_path_pending(&self) -> bool {
        !self.selection_path.is_empty()
            || self
                .magnetic_input
                .as_ref()
                .is_some_and(|input| !input.queue.is_empty())
    }
    pub(super) fn clear_selection_path(&mut self) {
        if self
            .magnetic_input
            .as_ref()
            .is_some_and(|input| input.commit.is_some())
        {
            self.cancel_pending_edit();
        }
        self.magnetic_sampler.cancel();
        self.selection_path = Vec::new();
        self.magnetic_input = None;
    }
    fn start_magnetic_input(&mut self, doc: DocumentView<'_>) -> Result<(), String> {
        if self.magnetic_input.is_some() {
            return Ok(());
        }
        let bytes = [self.project_id.len(), doc.id().len(), self.selected.len()]
            .into_iter()
            .try_fold(
                LIMIT * (std::mem::size_of::<Point>() + 8) * 2 + 4096,
                |sum, count| {
                    sum.checked_add(count)
                        .ok_or("Magnetic input metadata overflow")
                },
            )?;
        if bytes > 64 * 1024 * 1024 {
            return Err("Magnetic input exceeds admission".into());
        }
        let charge = doc
            .native()
            .map(|doc| MetadataCharge::reserve(doc.budget(), bytes, &|| Ok(())))
            .transpose()?;
        if self.selection_path.len() > LIMIT {
            return Err("Use at most 8192 lasso anchors".into());
        }
        let mut queue = VecDeque::new();
        queue
            .try_reserve_exact(LIMIT)
            .map_err(|_| "Magnetic input allocation refused")?;
        // A pre-existing resident draft is copied only after native admission.
        let mut path = Vec::new();
        path.try_reserve_exact(LIMIT)
            .map_err(|_| "Magnetic path allocation refused")?;
        path.extend_from_slice(&self.selection_path);
        self.selection_path = path;
        self.magnetic_input = Some(Input {
            engine: Arc::downgrade(&self.shared),
            project: self.project_id.clone(),
            document: doc.id().into(),
            revision: doc.revision(),
            native: doc.native().map(|doc| doc.key()),
            layer: self.selected.clone(),
            queue,
            ticket: None,
            commit: None,
            completed: None,
            closing: false,
            error: None,
            _charge: charge,
        });
        Ok(())
    }
    pub(super) fn queue_magnetic_point(
        &mut self,
        doc: DocumentView<'_>,
        point: [f32; 2],
        clicked: bool,
    ) -> Result<(), String> {
        if point.iter().any(|v| !v.is_finite() || v.abs() > 100000.) {
            return Err("Magnetic anchor exceeds document coordinate limits".into());
        }
        let size = [doc.width(), doc.height()];
        let snap = crate::render::sample::point(size, point).is_ok();
        if snap {
            crate::render::magnetic::area(size, point, self.selection_width)?;
        } else if !clicked {
            return Ok(());
        }
        self.start_magnetic_input(doc)?;
        if !self.magnetic_input.as_ref().unwrap().matches(self, doc) {
            return Err(
                "Magnetic selection source changed; cancel the draft before continuing".into(),
            );
        }
        let input = self.magnetic_input.as_mut().unwrap();
        if input.closing || input.error.is_some() {
            return Ok(());
        }
        if let Some(last) = input
            .queue
            .back_mut()
            .filter(|last| last.position == point && last.width == self.selection_width)
        {
            last.clicked |= clicked;
            return Ok(());
        }
        if input.queue.is_empty() && input.completed == Some((point, self.selection_width)) {
            return Ok(());
        }
        let last = input
            .queue
            .back()
            .map(|point| point.position)
            .or_else(|| self.selection_path.last().copied());
        if last.is_some_and(|last| {
            if clicked {
                last == point
            } else {
                (last[0] - point[0]).hypot(last[1] - point[1]) <= 3.
            }
        }) {
            return Ok(());
        }
        // Keep the active point and all clicks; replace only an unstarted hover.
        if !clicked
            && input.queue.back().is_some_and(|point| !point.clicked)
            && (input.queue.len() > 1 || input.ticket.is_none())
        {
            input.queue.pop_back();
        }
        if self.selection_path.len() + input.queue.len() == LIMIT {
            return Err("Use at most 8192 lasso anchors".into());
        }
        input.queue.push_back(Point {
            position: point,
            width: self.selection_width,
            clicked,
            snap,
        });
        Ok(())
    }
    pub(super) fn pop_magnetic_point(&mut self) {
        if let Some(input) = &mut self.magnetic_input {
            input.closing = false;
            input.completed = None;
            if input.queue.pop_back().is_some() {
                if input.queue.is_empty() {
                    self.magnetic_sampler.cancel();
                    input.ticket = None;
                    input.error = None;
                }
                return;
            }
        }
        self.selection_path.pop();
    }
    pub(super) fn close_magnetic_path(&mut self) {
        if let Some(input) = &mut self.magnetic_input {
            input.closing = true;
        }
    }
    pub(super) fn poll_magnetic_path(&mut self, ctx: &egui::Context, doc: DocumentView<'_>) {
        let Some(mut input) = self.magnetic_input.take() else {
            return;
        };
        let result = (|| -> Result<(), String> {
            if !input.matches(self, doc) {
                return Err(
                    "Magnetic selection source changed; cancel the draft before continuing".into(),
                );
            }
            if input.error.is_some() {
                return Ok(());
            }
            if let Some(ticket) = input.ticket {
                if let Some(point) = self.magnetic_sampler.take(
                    ticket,
                    &self.shared,
                    &self.project_id,
                    &self.selected,
                    false,
                    None,
                )? {
                    input.ticket = None;
                    input.complete_point(point, &mut self.selection_path);
                }
            }
            if input.ticket.is_none() {
                // Plain workspace anchors share click order with pending snaps.
                while let Some(point) = input.queue.front().filter(|point| !point.snap) {
                    input.complete_point(point.position, &mut self.selection_path);
                }
                if let Some(point) = input.queue.front() {
                    input.ticket = Some(self.magnetic_sampler.request_magnetic(
                        &self.shared,
                        &self.project_id,
                        doc,
                        point.position,
                        point.width,
                        &self.selected,
                        ctx,
                    )?);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            self.message = error.clone().into();
            input.error = Some(error);
        }
        let close = input.closing
            && input.queue.is_empty()
            && input.error.is_none()
            && input.commit.is_none()
            && !self.edit_worker.pending();
        self.magnetic_input = Some(input);
        if close {
            if self.selection_path.len() >= 3 {
                if let Err(error) = self.commit_magnetic_path(ctx, doc) {
                    self.message = error.clone().into();
                    self.magnetic_input.as_mut().unwrap().error = Some(error);
                }
            } else {
                self.magnetic_input.as_mut().unwrap().closing = false;
            }
        }
    }
    fn commit_magnetic_path(
        &mut self,
        ctx: &egui::Context,
        doc: DocumentView<'_>,
    ) -> Result<(), String> {
        {
            let engine = self
                .shared
                .try_lock()
                .map_err(|_| "Project is busy; retry the selection")?;
            engine.ensure_open()?;
            if engine.project_id != self.project_id
                || !crate::engine::view::current(&engine).same_observation(doc)
            {
                return Err("Selection source changed before publication".into());
            }
        }
        let _working = doc
            .native()
            .map(|doc| {
                crate::psd::memory36::MetadataCharge::reserve(
                    doc.budget(),
                    self.selection_path.len() * 256 + 4096,
                    &|| Ok(()),
                )
            })
            .transpose()?;
        let command = self.select_command(
            doc,
            &self.selection_path,
            self.selection_gesture_mode
                .as_deref()
                .unwrap_or(&self.selection_mode),
        );
        let native = doc
            .native()
            .map(|doc| {
                crate::engine::native::copy_admitted(&command, doc.budget(), true, &|| Ok(()))
            })
            .transpose()?;
        let token = egui::Id::new(crate::engine::id());
        self.magnetic_input.as_mut().unwrap().commit = Some(token);
        if let Some(command) = native {
            let request = edit_job::Request {
                project: self.project_id.clone(),
                document: doc.id().into(),
                revision: doc.revision(),
                label: "Lasso selection".into(),
                selection_edit: true,
                origin: edit_job::Origin::Magnetic(token),
            };
            if let Err(error) =
                self.submit_edit_job(request, edit_job::Commands::Native(command), ctx)
            {
                self.magnetic_finished(token, Err(error));
            }
        } else {
            self.apply_action(
                vec![command],
                "Lasso selection",
                Some((doc.id(), doc.revision())),
                edit_job::Origin::Magnetic(token),
            );
        }
        Ok(())
    }
    pub(super) fn magnetic_finished(&mut self, token: egui::Id, result: Result<(), String>) {
        let Some(input) = self
            .magnetic_input
            .as_mut()
            .filter(|input| input.commit == Some(token))
        else {
            return;
        };
        input.commit = None;
        match result {
            Ok(()) => {
                self.clear_selection_path();
                self.selection_gesture_source = None;
                self.selection_gesture_mode = None;
            }
            Err(error) => {
                input.error = Some(error.clone());
                self.message = error.into();
            }
        }
    }
    pub(super) fn magnetic_input_window(&mut self, ctx: &egui::Context) {
        let Some(error) = self
            .magnetic_input
            .as_ref()
            .and_then(|input| input.error.clone())
        else {
            return;
        };
        egui::Window::new("Magnetic selection paused")
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label(error);
                if ui.button("Retry").clicked() {
                    self.magnetic_sampler.cancel();
                    if let Some(input) = &mut self.magnetic_input {
                        input.ticket = None;
                        input.error = None;
                    }
                }
                if ui.button("Cancel selection draft").clicked() {
                    self.clear_selection_path();
                    self.selection_gesture_source = None;
                    self.selection_gesture_mode = None;
                }
            });
    }
}
