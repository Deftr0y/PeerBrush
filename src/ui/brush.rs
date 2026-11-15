use super::*;

pub(super) fn with_stroke_samples(
    mut command: Value,
    points: &[[f32; 2]],
    pressures: Option<&[f32]>,
) -> Value {
    command["points"] = json!(points);
    if let Some(pressures) = pressures {
        command["pressures"] = json!(pressures);
    }
    command
}

#[derive(Clone, Copy)]
struct StrokePress {
    screen: Pos2,
    point: [f32; 2],
    force: Option<f32>,
    shift: bool,
}

pub(super) struct WorkspaceStroke {
    source: selection::GestureSource,
    press: Option<StrokePress>,
    tool: Tool,
    mask: bool,
    step: Option<String>,
}

impl WorkspaceStroke {
    pub(super) fn observes(&self, app: &PeerBrush, doc: DocumentView<'_>) -> bool {
        self.source.observes(app, doc)
            && self.tool == app.tool
            && self.mask == app.mask
            && self.step == app.mask_step
    }

    pub(super) fn advance_sampled_source(&mut self, doc: DocumentView<'_>, previous: u64) {
        self.source.advance_sampled_stroke(doc, previous);
    }
}

fn touch_force(event: &egui::Event, phase: egui::TouchPhase, position: Pos2) -> Option<f32> {
    match event {
        egui::Event::Touch { phase: reported, pos, force: Some(force), .. }
            if *reported == phase && *pos == position && (0.0..=1.0).contains(force) => Some(*force),
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct Straight {
    last: Option<Last>,
    pending: Option<(egui::Id, Last)>,
    pub axis: Option<(usize, &'static str)>,
    pub line: bool,
    pub document: Option<String>,
    pub cancelled: bool,
    shift: bool,
    start: usize,
}
struct Last {
    engine: std::sync::Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    revision: u64,
    layer: String,
    mask: bool,
    step: Option<String>,
    tool: Tool,
    point: [f32; 2],
    pressure: f32,
    _charge: Option<Arc<crate::psd::memory36::MetadataCharge>>,
}
fn endpoint(command: &Value) -> Option<([f32; 2], f32)> {
    crate::brush::endpoint_from_command(command).ok()
}
impl Straight {
    pub(super) fn cancel(&mut self) {
        self.axis = None;
        self.line = false;
        self.shift = false;
        self.start = 0;
        self.document = None;
        self.cancelled = false;
    }
    pub(super) fn reset(&mut self) {
        self.cancel();
        self.last = None;
        self.pending = None;
    }
}
impl PeerBrush {
    pub(super) fn brush_tool(&self) -> bool {
        matches!(
            self.tool,
            Tool::Brush | Tool::Eraser | Tool::Smudge | Tool::Clone | Tool::Heal
        )
    }
    pub(super) fn straight_click<'a>(
        &mut self,
        doc: impl Into<DocumentView<'a>>,
        point: [f32; 2],
        shift: bool,
    ) {
        let doc = doc.into();
        if !shift || !self.brush_tool() {
            return;
        }
        let published = self
            .straight
            .last
            .as_ref()
            .filter(|v| {
                v.engine.as_ptr() == Arc::as_ptr(&self.shared)
                    && v.project == self.project_id
                    && v.document == doc.id()
                    && v.revision == doc.revision()
                    && v.layer == self.selected
                    && v.mask == self.mask
                    && v.step == self.mask_step
                    && v.tool == self.tool
            })
            .map(|last| (last.point, last.pressure));
        let anchor = if self.tool == Tool::Brush {
            self.sampled_line_anchor(doc).unwrap_or(published)
        } else {
            published
        };
        let Some((start, pressure)) = anchor else {
            return;
        };
        if !self.begin_retouch(doc, start) {
            return;
        }
        self.straight.cancel();
        self.straight.line = true;
        self.straight.document = Some(doc.id().to_owned());
        self.drag_start = Some(point);
        self.drag_revision = Some(doc.revision());
        self.points = vec![start, point];
        self.stroke_pressures = vec![pressure, self.current_pressure.unwrap_or(1.)];
        self.stroke_has_pressure = self.current_pressure.is_some() || pressure != 1.;
        self.live_gesture = Some(crate::engine::id());
    }
    pub(super) fn straight_drag(&mut self, shift: bool) {
        self.straight.cancel();
        self.straight.shift = shift;
    }
    pub(super) fn straight_modifier(&mut self, shift: bool) {
        if !self.brush_tool() || self.straight.line {
            return;
        }
        if self.straight.shift && !shift {
            if self.straight.axis.is_some() && !self.points.is_empty() {
                if let Ok((points, _)) = crate::brush::points_from_command(
                    &self.stroke_command(&self.points, self.color),
                ) {
                    self.points = points;
                }
            }
            self.straight.axis = None;
        }
        if shift && !self.straight.shift {
            self.straight.start = self.points.len().saturating_sub(1);
        }
        self.straight.shift = shift;
    }
    pub(super) fn straight_axis(&mut self) {
        if self.brush_tool()
            && self.straight.shift
            && !self.straight.line
            && self.straight.axis.is_none()
            && self.points.len() > self.straight.start + 1
        {
            let a = self.points[self.straight.start];
            let b = *self.points.last().unwrap();
            if (a[0] - b[0]).hypot(a[1] - b[1]) > 0. {
                self.straight.axis = Some((
                    self.straight.start,
                    if crate::brush::horizontal_axis(a, b) {
                        "horizontal"
                    } else {
                        "vertical"
                    },
                ));
            }
        }
    }
    pub(super) fn toolbar_brush_opacity(&mut self, ui: &mut egui::Ui) {
        controls::label(
            ui,
            if self.tool == Tool::Smudge {
                "Strength"
            } else {
                "Opacity"
            },
        );
        let ai = matches!(
            (self.animation.tool(ui.input(|i| i.time)), self.tool),
            (Some("brush"), Tool::Brush)
                | (Some("eraser"), Tool::Eraser)
                | (Some("smudge"), Tool::Smudge)
                | (Some("clone"), Tool::Clone)
                | (Some("heal"), Tool::Heal)
        );
        let ai = ai || self.tool_preferences_ai(ui.input(|i| i.time));
        let mut percent =
            self.tool_display_number("opacity", self.brush.opacity, ui.input(|i| i.time)) * 100.;
        let response = controls::range_with_color(
            ui,
            "toolbar brush opacity",
            &mut percent,
            0.0..=100.0,
            104.,
            "%",
            0,
            false,
            if ai {
                AI_BLUE
            } else {
                Color32::from_rgb(190, 70, 29)
            },
        );
        if response.changed() {
            self.brush.opacity = percent / 100.;
        }
        response.on_hover_text(if self.tool == Tool::Smudge {
            "Smudge strength"
        } else {
            "Brush opacity · double-click to enter a value"
        });
    }
    pub(super) fn pointer_pressure(ui: &egui::Ui) -> Option<f32> {
        ui.input(|input| {
            Self::force_at_pointer(&input.events, input.pointer.interact_pos()?)
        })
    }
    pub(super) fn force_at_pointer(events: &[egui::Event], pointer: Pos2) -> Option<f32> {
        events.iter().rev().find_map(|event| match event {
            egui::Event::Touch {
                force: Some(force),
                phase: egui::TouchPhase::Start | egui::TouchPhase::Move,
                pos,
                ..
            } if *pos == pointer && (0.0..=1.0).contains(force) => Some(*force),
            _ => None,
        })
    }
    pub(super) fn pointer_contact_cancelled(ctx: &egui::Context) -> bool {
        ctx.input(|input| {
            // The pointer adapter emits PointerGone immediately after the
            // cancellation of the contact driving the pointer.
            input.events.windows(2).any(|events| {
                matches!(
                    events,
                    [
                        egui::Event::Touch { phase: egui::TouchPhase::Cancel, .. },
                        egui::Event::PointerGone
                    ]
                )
            })
        })
    }
    pub(super) fn cancel_contact_stroke(&mut self) {
        if !self.points.is_empty() && self.live_gesture.is_some() {
            self.preview_epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.last_preview = None;
            self.pending = false;
        }
        if !self.points.is_empty() {
            self.message = "Pen contact cancelled · stroke discarded".into();
        }
        self.points.clear();
        self.stroke_pressures.clear();
        self.stroke_has_pressure = false;
        self.current_pressure = None;
        self.drag_start = None;
        self.drag_revision = None;
        self.live_gesture = None;
        self.workspace_stroke = None;
        self.straight.cancel();
    }

    pub(super) fn sync_workspace_stroke(&mut self, doc: DocumentView<'_>, ctx: &egui::Context) {
        if self.workspace_stroke.as_ref().is_some_and(|stroke| !stroke.observes(self, doc)) {
            let changed_image = self.drag_revision.is_some_and(|revision| revision != doc.revision())
                || self.straight.document.as_deref().is_some_and(|id| id != doc.id());
            self.cancel_contact_stroke();
            self.straight.cancelled = ctx.input(|input| input.pointer.primary_down());
            self.message = if changed_image && self.tool == Tool::Liquify {
                "Image changed during Liquify stroke · preview cancelled"
            } else if changed_image {
                "Image changed during brush stroke · preview cancelled"
            } else {
                "Stroke target changed · draft cancelled"
            }.into();
        }
    }

    pub(super) fn stroke_canvas(
        &mut self,
        ui: &egui::Ui,
        doc: DocumentView<'_>,
        response: &egui::Response,
        projection: &view::Projection,
        mut pending: Option<WorkspaceStroke>,
    ) {
        let cancelled = ui.input(|input| {
            !input.focused
                || input.key_pressed(egui::Key::Escape)
                || input.key_down(egui::Key::Space)
                || input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
        });
        if cancelled {
            let had_draft = self.drag_start.is_some() || pending.is_some();
            self.cancel_contact_stroke();
            if had_draft {
                self.message = "Stroke cancelled · current work preserved".into();
            }
            self.straight.cancelled = ui.input(|input| input.pointer.primary_down());
            return;
        }
        let held = self.drag_start.is_some() || pending.is_some();
        let (press, release, start, end) = ui.input(|input| {
            let events = &input.events;
            let press = (!held).then(|| events.iter().enumerate().find_map(|(index, event)| {
                let egui::Event::PointerButton { button: egui::PointerButton::Primary,
                    pressed: true, pos, modifiers, .. } = event else { return None };
                let force = events[..index].iter().rev()
                    .find(|event| !matches!(event, egui::Event::PointerMoved(point) if point == pos))
                    .and_then(|event| touch_force(event, egui::TouchPhase::Start, *pos));
                Some((index, StrokePress {
                    screen: *pos, point: projection.document(*pos), force, shift: modifiers.shift,
                }))
            })).flatten();
            let start = press.map_or(0, |(index, _)| index + 1);
            let release = events[start..].iter().enumerate().find_map(|(index, event)| {
                match event {
                    egui::Event::PointerButton { button: egui::PointerButton::Primary,
                        pressed: false, pos, modifiers, .. } => Some((index + start, *pos, modifiers.shift)),
                    _ => None,
                }
            });
            (press.map(|(_, press)| press), release, start, release.map_or(events.len(), |r| r.0))
        });
        if release.is_none() && !ui.input(|input| input.pointer.primary_down()) {
            if held {
                self.cancel_contact_stroke();
            }
            return;
        }
        let press = press.filter(|press| canvas_press_hits(ui, response, press.screen));
        let pointer = release.map(|(_, pos, _)| pos).or_else(|| ui.input(|input| input.pointer.interact_pos()));
        let moved = |origin: Pos2| {
            let slop = ui.ctx().options(|options| options.input_options.max_click_dist);
            response.dragged_by(egui::PointerButton::Primary)
                || pointer.is_some_and(|pos| origin.distance(pos) > slop)
                || ui.input(|input| input.events[start..end].iter().any(|event| {
                    matches!(event, egui::Event::PointerMoved(pos) if origin.distance(*pos) > slop)
                }))
        };
        let press = if let Some(waiting) = pending.as_ref().and_then(|stroke| stroke.press) {
            if !moved(waiting.screen) {
                if release.is_none() {
                    self.workspace_stroke = pending;
                }
                return;
            }
            pending.as_mut().unwrap().press = None;
            Some(waiting)
        } else {
            press
        };
        if let Some(press) = press {
            let inside = projection.contains(press.screen);
            let moved = moved(press.screen);
            if !inside && !moved && release.is_some() {
                return;
            }
            if pending.is_none() {
                let source = match self.canvas_gesture_source(doc) {
                    Ok(source) => source,
                    Err(error) => {
                        self.message = error.into();
                        return;
                    }
                };
                pending = Some(WorkspaceStroke {
                    source, press: (!inside && !moved).then_some(press),
                    tool: self.tool, mask: self.mask, step: self.mask_step.clone(),
                });
            }
            if !inside && !moved {
                self.workspace_stroke = pending;
                return;
            }
            self.current_pressure = press.force;
            if inside && !moved {
                self.straight_click(doc, press.point, press.shift);
            }
            let shift = if start > 0 { press.shift } else { ui.input(|input| input.modifiers.shift) };
            if !self.straight.line && !self.begin_freehand(doc, press.point, shift) {
                self.current_pressure = None;
                return;
            }
        } else if let Some(origin) = self.drag_start.filter(|_| self.straight.line) {
            if moved(projection.point(origin)) {
                let shift = ui.input(|input| input.modifiers.shift);
                if !self.begin_freehand(doc, origin, shift) {
                    return;
                }
            }
        }
        if self.drag_start.is_none() || self.points.is_empty() {
            return;
        }
        let shift = press.filter(|_| start > 0).map_or_else(
            || release.map_or_else(|| ui.input(|input| input.modifiers.shift), |r| r.2),
            |press| press.shift,
        );
        self.straight_modifier(shift);
        if !self.straight.line {
            ui.input(|input| {
                let events = &input.events;
                for (index, event) in events[start..end].iter().enumerate() {
                    if let egui::Event::PointerMoved(pos) = event {
                        if let Some(force) = (index + start).checked_sub(1)
                            .and_then(|prior| touch_force(&events[prior], egui::TouchPhase::Move, *pos))
                        {
                            self.current_pressure = Some(force);
                        }
                        self.push_stroke_sample(projection.document(*pos), false);
                    }
                }
            });
        }
        if let Some((_, pos, _)) = release {
            self.push_stroke_sample(projection.document(pos), true);
        }
        self.straight_axis();
        if release.is_none() {
            self.workspace_stroke = pending;
            return;
        }
        self.transient.clear();
        if self.brush_tool() {
            let command = self.stroke_command(&self.points, self.stroke_color());
            let label = if self.points.len() == 1 { "Brush dot" } else { "Brush stroke" };
            let _ = self.submit_stroke(command, label);
        } else {
            self.edit(vec![self.liquify_command(&self.points)], if self.points.len() == 1 {
                "Liquify dab"
            } else {
                "Liquify stroke"
            });
        }
        self.points.clear();
        self.stroke_pressures.clear();
        self.stroke_has_pressure = false;
        self.current_pressure = None;
        self.drag_start = None;
        self.drag_revision = None;
        self.straight.cancel();
        self.live_gesture = None;
    }

    fn begin_freehand(&mut self, doc: DocumentView<'_>, point: [f32; 2], shift: bool) -> bool {
        if !self.begin_retouch(doc, point) {
            return false;
        }
        self.straight_drag(shift);
        self.straight.document = Some(doc.id().to_owned());
        self.drag_start = Some(point);
        self.drag_revision = Some(doc.revision());
        self.points = vec![point];
        self.stroke_pressures = vec![self.current_pressure.unwrap_or(1.)];
        self.stroke_has_pressure = self.current_pressure.is_some();
        self.live_gesture = Some(crate::engine::id());
        true
    }

    fn push_stroke_sample(&mut self, point: [f32; 2], release: bool) {
        let previous = *self.points.last().unwrap();
        let distance = (point[0] - previous[0]).abs() + (point[1] - previous[1]).abs();
        if distance > 0.5 || release && distance > 0. {
            self.points.push(point);
            self.stroke_pressures.push(self.current_pressure.unwrap_or(1.));
            self.stroke_has_pressure |= self.current_pressure.is_some();
            self.straight_axis();
        } else if distance == 0. {
            if let (Some(force), Some(last)) = (self.current_pressure, self.stroke_pressures.last_mut()) {
                *last = force;
                self.stroke_has_pressure = true;
            }
        }
    }
    pub(super) fn tip_outline(&self, painter: &egui::Painter, center: Pos2, scale: f32) {
        let (sin, cos) = self.brush.angle.to_radians().sin_cos();
        let points: Vec<Pos2> = (0..=48)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 48.0;
                let x = a.cos() * self.radius * scale;
                let y = a.sin() * self.radius * scale * self.brush.roundness;
                center
                    + self
                        .view
                        .vector(Vec2::new(x * cos - y * sin, x * sin + y * cos))
            })
            .collect();
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(3.0_f32, Color32::BLACK),
        ));
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.2_f32, Color32::WHITE),
        ));
        // A small center mark remains legible when the real tip is subpixel at low zoom.
        for direction in [Vec2::X, Vec2::Y] {
            let line = [center - direction * 3.0, center + direction * 3.0];
            painter.line_segment(line, Stroke::new(2.5_f32, Color32::BLACK));
            painter.line_segment(line, Stroke::new(1.0_f32, Color32::WHITE));
        }
    }
    pub(super) fn swap_colors(&mut self) {
        if self.mask {
            std::mem::swap(&mut self.mask_value, &mut self.mask_background);
            std::mem::swap(&mut self.mask_value16, &mut self.mask_background16);
        } else {
            std::mem::swap(&mut self.color, &mut self.background);
            std::mem::swap(&mut self.color16, &mut self.background16);
        }
    }
    pub(super) fn palette(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(44.0, 60.0), egui::Sense::hover());
        let fore = if self.mask {
            [self.mask_value, self.mask_value, self.mask_value, 255]
        } else {
            self.color
        };
        let back = if self.mask {
            [
                self.mask_background,
                self.mask_background,
                self.mask_background,
                255,
            ]
        } else {
            self.background
        };
        let mut focused_swatch = None;
        for (foreground, pixel, at) in [
            (false, back, rect.min + Vec2::new(14.0, 15.0)),
            (true, fore, rect.min + Vec2::new(0.0, 2.0)),
        ] {
            let swatch = Rect::from_min_size(at, Vec2::splat(28.0));
            let label = match (foreground, self.mask) {
                (true, false) => "Foreground color",
                (false, false) => "Background color",
                (true, true) => "Foreground mask value",
                (false, true) => "Background mask value",
            };
            let response = ui.interact(
                swatch,
                ui.id().with(if foreground {
                    "foreground"
                } else {
                    "background"
                }),
                egui::Sense::click(),
            ).on_hover_text(label);
            response.widget_info(|| egui::WidgetInfo::labeled(
                egui::WidgetType::Button, response.enabled(), label,
            ));
            super::color::checker(ui.painter(), swatch);
            ui.painter().rect_filled(
                swatch,
                2,
                Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]),
            );
            ui.painter().rect_stroke(
                swatch,
                2,
                Stroke::new(1.0_f32, Color32::from_gray(155)),
                egui::StrokeKind::Inside,
            );
            if response.clicked() {
                let target = match (foreground, self.mask) {
                    (true, false) => super::color::Target::Foreground,
                    (false, false) => super::color::Target::Background,
                    (true, true) => super::color::Target::MaskForeground,
                    (false, true) => super::color::Target::MaskBackground,
                };
                self.open_color(target);
            }
            if response.has_focus() {
                focused_swatch = Some(response);
            }
        }
        if let Some(response) = focused_swatch {
            icons::paint_focus(ui, &response);
        }
        let swap = ui
            .interact(
                Rect::from_min_size(rect.min + Vec2::new(29.0, -2.0), Vec2::splat(16.0)),
                ui.id().with("swap"),
                egui::Sense::click(),
            )
            .on_hover_text("Swap foreground / background · X");
        swap.widget_info(|| egui::WidgetInfo::labeled(
            egui::WidgetType::Button,
            swap.enabled(),
            if self.mask { "Swap mask values" } else { "Swap colors" },
        ));
        icons::paint(ui.painter(), swap.rect, Icon::Swap, swap.enabled());
        icons::paint_focus(ui, &swap);
        if swap.clicked() {
            self.swap_colors();
        }
    }
    pub(super) fn stroke_color(&self) -> Pixel {
        if self.mask {
            [
                self.mask_value,
                self.mask_value,
                self.mask_value,
                self.color[3],
            ]
        } else {
            self.color
        }
    }
    fn track_stroke(
        &mut self,
        endpoint: Option<([f32; 2], f32)>,
        document: &str,
        revision: u64,
        budget: Option<&crate::large_document::ResidentBudget>,
    ) -> Result<egui::Id, String> {
        self.straight.last = None;
        self.straight.pending = None;
        let charge = budget
            .map(|budget| {
                let bytes = [
                    document.len(),
                    self.project_id.len(),
                    self.selected.len(),
                    self.mask_step.as_ref().map_or(0, String::len),
                ]
                .into_iter()
                .try_fold(1024usize, |size, count| {
                    size.checked_add(count)
                        .ok_or("Stroke endpoint metadata exceeds admission")
                })?;
                if bytes > 64 * 1024 * 1024 {
                    return Err("Stroke endpoint metadata exceeds admission".to_owned());
                }
                crate::psd::memory36::MetadataCharge::reserve(budget, bytes, &|| Ok(()))
            })
            .transpose()?;
        let token = egui::Id::new(crate::engine::id());
        self.straight.pending = endpoint.map(|(point, pressure)| {
            (
                token,
                Last {
                    _charge: charge,
                    engine: Arc::downgrade(&self.shared),
                    project: self.project_id.clone(),
                    document: document.into(),
                    revision,
                    layer: self.selected.clone(),
                    mask: self.mask,
                    step: self.mask_step.clone(),
                    tool: self.tool,
                    point,
                    pressure,
                },
            )
        });
        Ok(token)
    }
    pub(super) fn stroke_finished(&mut self, token: egui::Id, revision: Option<u64>) {
        if !self
            .straight
            .pending
            .as_ref()
            .is_some_and(|(pending, _)| *pending == token)
        {
            return;
        }
        let (_, mut last) = self.straight.pending.take().unwrap();
        if revision.is_some()
            && revision == last.revision.checked_add(1)
            && last.engine.as_ptr() == Arc::as_ptr(&self.shared)
            && last.project == self.project_id
            && last.layer == self.selected
            && last.mask == self.mask
            && last.step == self.mask_step
            && last.tool == self.tool
        {
            last.revision = revision.unwrap();
            self.straight.last = Some(last);
        }
    }
    pub(super) fn track_stroke_command(
        &mut self,
        command: &Value,
        endpoint: ([f32; 2], f32),
        document: &str,
        revision: u64,
        budget: Option<&crate::large_document::ResidentBudget>,
    ) -> Result<egui::Id, String> {
        if self.tool != Tool::Brush
            || command["layer"].as_str() != Some(self.selected.as_str())
            || command["mask"].as_bool() != Some(self.mask)
            || command["step"].as_str() != self.mask_step.as_deref()
        {
            self.straight.last = None;
            self.straight.pending = None;
            return Ok(egui::Id::new(crate::engine::id()));
        }
        self.track_stroke(Some(endpoint), document, revision, budget)
    }
    pub(super) fn submit_stroke(&mut self, mut command: Value, label: &str) -> bool {
        match self.enqueue_sampled_stroke(&mut command, label) {
            Ok(true) => return true,
            Err(_) => return false,
            Ok(false) => (),
        }
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; wait for its result".into();
            return false;
        }
        let endpoint = endpoint(&command);
        let shared = self.shared.clone();
        let engine = shared.lock().unwrap();
        let view = crate::engine::view::current(&engine);
        let document = command["document_id"].as_str().unwrap_or(view.id());
        let revision = command["source_revision"]
            .as_u64()
            .unwrap_or(view.revision());
        let token = match self.track_stroke(
            endpoint,
            document,
            revision,
            view.native().map(|doc| doc.budget()),
        ) {
            Ok(token) => token,
            Err(error) => {
                self.message = error.into();
                return false;
            }
        };
        let document = document.to_owned();
        drop(engine);
        self.cancel_free_transform();
        self.close_proposal();
        self.apply_action(
            vec![command],
            label,
            Some((&document, revision)),
            edit_job::Origin::Stroke(token),
        );
        true
    }
    pub(super) fn stroke_command(&self, points: &[[f32; 2]], color: Pixel) -> Value {
        with_stroke_samples(
            self.stroke_command_fields(points.first().copied(), color),
            points,
            self.pressures_for_stroke(points.len()),
        )
    }
    pub(super) fn pressures_for_stroke(&self, point_count: usize) -> Option<&[f32]> {
        (self.stroke_has_pressure && self.stroke_pressures.len() == point_count)
            .then_some(self.stroke_pressures.as_slice())
    }
    pub(super) fn stroke_command_fields(&self, first: Option<[f32; 2]>, color: Pixel) -> Value {
        let b = self.brush;
        let mut command = json!({"op":if self.tool==Tool::Smudge {"smudge"}else{"paint"},"layer":self.selected,"radius":self.radius,"color":color,
            "mask":self.mask,"step":self.mask_step,"erase":self.tool==Tool::Eraser,
            "hardness":b.hardness,"opacity":b.opacity,"flow":b.flow,"spacing":b.spacing,
            "roundness":b.roundness,"angle":b.angle,"smoothing":b.smoothing,
            "tip":b.tip.name(),"density":b.density,"grain":b.grain,"seed":b.seed,
            "pressure_size":b.pressure_size,"pressure_opacity":b.pressure_opacity,"pressure_gamma":b.pressure_gamma,"pressure_min":b.pressure_min,"pressure_max":b.pressure_max,
            "taper_start":b.taper_start,"taper_end":b.taper_end,"taper_size":b.taper_size,"taper_opacity":b.taper_opacity,
            "wetness":b.wetness,"load":b.load,"pickup":b.pickup});
        if let Some((start, axis)) = self.straight.axis {
            command["stroke_axis"] = json!(axis);
            command["stroke_axis_from"] = json!(start);
        }
        if let Some(revision) = self.drag_revision {
            command["source_revision"] = json!(revision);
            command["document_id"] = json!(self
                .straight
                .document
                .as_deref()
                .unwrap_or(self.doc_snapshot.view().id()));
        }
        self.retouch_command(&mut command, first);
        if matches!(command["op"].as_str(), Some("paint" | "smudge")) && color == self.stroke_color() {
            if let Some(words) = self.foreground_words(self.doc_snapshot.view().depth()) {
                command["color16"] = json!(words);
            }
        }
        command
    }
    pub(super) fn brush_settings(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new(match self.tool {
            Tool::Smudge => "Smudge",
            Tool::Clone => "Clone",
            Tool::Heal => "Heal",
            _ => "Brush",
        })
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_pos(egui::pos2(85.0, 125.0))
        .default_width(680.0)
        .show(ctx, |ui| {
            ui.columns(2, |columns| {
                self.preset_browser(&mut columns[0]);
                self.brush_parameters(&mut columns[1]);
            });
        });
        self.show_brush = open;
    }
    fn brush_parameters(&mut self, ui: &mut egui::Ui) {
        let time = ui.input(|i| i.time);
        let ai = self.tool_preferences_ai(time);
        let display = self.tool_display_values(time);
        let shown = |key: &str| display[key].as_f64().unwrap_or(0.) as f32;
        ui.label(RichText::new(&self.brush_name).color(if ai { AI_BLUE } else { ACCENT }));
        let tips = [
            (crate::brush::TipKind::Round, "Round"),
            (crate::brush::TipKind::Dry, "Dry"),
            (crate::brush::TipKind::Chalk, "Chalk"),
            (crate::brush::TipKind::Grain, "Grain"),
            (crate::brush::TipKind::Bristle, "Bristle"),
        ];
        controls::label(ui, "Brush tip");
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 3.;
            for (tip, label) in tips {
                let cache = egui::Id::new(("brush tip stroke", tip));
                let preview = ui.ctx().data(|data| data.get_temp::<TextureHandle>(cache))
                    .or_else(|| {
                        let settings = crate::brush::Settings {
                            tip, density: 0.65, grain: 2.5, ..Default::default()
                        };
                        let raster = crate::brush_library::preview(settings, 64, 24).ok()?;
                        let texture = ui.ctx().load_texture(
                            format!("{label} tip"),
                            egui::ColorImage::from_rgba_unmultiplied([64, 24], &raster.rgba()),
                            egui::TextureOptions::LINEAR,
                        );
                        ui.ctx().data_mut(|data| data.insert_temp(cache, texture.clone()));
                        Some(texture)
                    });
                let selected = self.brush.tip == tip;
                let button = match &preview {
                    Some(texture) => egui::Button::image((texture.id(), Vec2::new(54., 20.25))),
                    None => egui::Button::new(label),
                };
                let response = ui.add(button.selected(selected)).on_hover_text(label);
                response.widget_info(|| egui::WidgetInfo::selected(
                    egui::WidgetType::RadioButton, response.enabled(), selected, label,
                ));
                icons::paint_focus(ui, &response);
                if response.clicked() {
                    self.brush.tip = tip;
                }
            }
            if let Some((_, label)) = tips.iter().find(|(tip, _)| *tip == self.brush.tip) {
                ui.label(*label);
            }
        });
        let preview_size = Vec2::new(260., 54.);
        let (preview, painter) = ui.allocate_painter(preview_size, egui::Sense::hover());
        egui::Grid::new("brush parameters")
            .spacing(Vec2::new(10., 8.))
            .show(ui, |ui| {
                let mut size = self.radius * 2.;
                controls::label(ui, "Size");
                let response = tool_settings::number(
                    ui,
                    "tip size",
                    &mut size,
                    shown("radius") * 2.,
                    1.0..=1024.,
                    200.,
                    " px",
                    0,
                    true,
                    ai,
                );
                if response.changed() {
                    self.radius = size / 2.;
                }
                ui.end_row();
                for (id, label, value, min, max) in [
                    ("hardness", "Hardness", &mut self.brush.hardness, 0., 100.),
                    (
                        "opacity",
                        if self.tool == Tool::Smudge {
                            "Strength"
                        } else {
                            "Opacity"
                        },
                        &mut self.brush.opacity,
                        0.,
                        100.,
                    ),
                    ("flow", "Flow", &mut self.brush.flow, 0., 100.),
                    ("spacing", "Spacing", &mut self.brush.spacing, 1., 200.),
                    (
                        "roundness",
                        "Roundness",
                        &mut self.brush.roundness,
                        5.,
                        100.,
                    ),
                    (
                        "smoothing",
                        "Smoothing",
                        &mut self.brush.smoothing,
                        0.,
                        100.,
                    ),
                ] {
                    let mut percent = *value * 100.;
                    controls::label(ui, label);
                    if tool_settings::number(
                        ui,
                        id,
                        &mut percent,
                        shown(id) * 100.,
                        min..=max,
                        200.,
                        "%",
                        0,
                        false,
                        ai,
                    )
                    .changed()
                    {
                        *value = percent / 100.;
                    }
                    ui.end_row();
                }
                controls::label(ui, "Angle");
                let mut angle = f64::from(shown("angle"));
                let response = ui.scope(|ui| {
                    if ai {
                        ui.visuals_mut().selection.stroke.color = AI_BLUE;
                    }
                    controls::angle(
                        ui, "tip angle", &mut angle, true, "Brush tip angle",
                        "0° is horizontal. +90° turns the tip clockwise.",
                    )
                }).inner;
                if response.changed() {
                    self.brush.angle = angle as f32;
                }
                ui.end_row();
                controls::label(ui, "Pressure curve");
                self.pressure_curve(ui, "brush curve");
                ui.end_row();
                if self.brush.tip != crate::brush::TipKind::Round {
                    let mut density = self.brush.density * 100.;
                    controls::label(ui, "Density");
                    if tool_settings::number(
                        ui,
                        "tip density",
                        &mut density,
                        shown("density") * 100.,
                        5.0..=100.,
                        200.,
                        "%",
                        0,
                        false,
                        ai,
                    )
                    .changed()
                    {
                        self.brush.density = density / 100.;
                    }
                    ui.end_row();
                    controls::label(ui, "Grain");
                    tool_settings::number(
                        ui,
                        "tip grain",
                        &mut self.brush.grain,
                        shown("grain"),
                        0.5..=32.,
                        200.,
                        " px",
                        1,
                        false,
                        ai,
                    );
                    ui.end_row();
                }
                for (id, label, value) in [
                    ("taper_start", "Start", &mut self.brush.taper_start),
                    ("taper_end", "End", &mut self.brush.taper_end),
                ] {
                    controls::label(ui, label);
                    tool_settings::number(
                        ui,
                        id,
                        value,
                        shown(id),
                        0.0..=512.,
                        200.,
                        " px",
                        0,
                        false,
                        ai,
                    )
                    .on_hover_text("Taper length along the stroke");
                    ui.end_row();
                }
                if self.tool == Tool::Smudge {
                    for (id, label, value) in [
                        ("wetness", "Wet", &mut self.brush.wetness),
                        ("load", "Load", &mut self.brush.load),
                        ("pickup", "Pickup", &mut self.brush.pickup),
                    ] {
                        let mut percent = *value * 100.;
                        controls::label(ui, label);
                        if tool_settings::number(
                            ui,
                            id,
                            &mut percent,
                            shown(id) * 100.,
                            0.0..=100.,
                            200.,
                            "%",
                            0,
                            false,
                            ai,
                        )
                        .changed()
                        {
                            *value = percent / 100.;
                        }
                        ui.end_row();
                    }
                }
            });
        ui.horizontal_wrapped(|ui| {
            controls::label(ui, "Pressure affects");
            icons::toggle(ui, Icon::StrokeWidth, &mut self.brush.pressure_size, "Pressure affects size")
                .on_hover_text("Lower pressure makes a narrower stroke. Uses pen force or explicitly supplied pressure samples.");
            icons::toggle(ui, Icon::StrokeOpacity, &mut self.brush.pressure_opacity, "Pressure affects opacity")
                .on_hover_text("Lower pressure makes the stroke more transparent. Uses pen force or explicitly supplied pressure samples.");
            if ui.small_button("Calibrate pressure…").clicked() {
                self.show_calibration = true;
            }
        });
        ui.horizontal_wrapped(|ui| {
            controls::label(ui, "Taper affects");
            icons::toggle(ui, Icon::StrokeWidth, &mut self.brush.taper_size, "Taper affects size")
                .on_hover_text("Narrow the stroke over the configured start and end lengths.");
            icons::toggle(ui, Icon::StrokeOpacity, &mut self.brush.taper_opacity, "Taper affects opacity")
                .on_hover_text("Fade the stroke over the configured start and end lengths.");
        });
        let mut current = self.brush;
        current.radius = self.radius;
        let key = format!("{current:?}");
        if self.brush_preview.as_ref().map(|p| p.0.as_str()) != Some(key.as_str()) {
            if let Ok(r) = crate::brush_library::preview(current, 260, 54) {
                let texture = ui.ctx().load_texture(
                    "brush tip",
                    egui::ColorImage::from_rgba_unmultiplied([260, 54], &r.rgba()),
                    egui::TextureOptions::LINEAR,
                );
                self.brush_preview = Some((key, texture));
            }
        }
        if let Some((_, texture)) = &self.brush_preview {
            painter.image(
                texture.id(),
                preview.rect,
                Rect::from_min_max(Pos2::ZERO, egui::pos2(1., 1.)),
                Color32::WHITE,
            );
        }
    }
    fn preset_browser(&mut self, ui: &mut egui::Ui) {
        let library = self.shared.lock().unwrap().brush_library.clone();
        let (mut presets, error) = {
            let l = library.lock().unwrap();
            (l.presets(), l.error.clone())
        };
        presets.sort_by_key(|preset| {
            crate::brush_library::CATEGORIES
                .iter()
                .position(|category| *category == preset.category.as_str())
                .unwrap_or(crate::brush_library::CATEGORIES.len())
        });
        self.brush_thumbnails
            .retain(|id, _| presets.iter().any(|p| &p.id == id));
        ui.add(
            egui::TextEdit::singleline(&mut self.brush_search)
                .hint_text("Search brushes")
                .desired_width(ui.available_width()),
        );
        let mut categories = vec!["All".to_owned()];
        for category in crate::brush_library::CATEGORIES {
            categories.push((*category).into());
        }
        categories.push("Custom".to_owned());
        for preset in &presets {
            if !categories.contains(&preset.category) {
                categories.push(preset.category.clone());
            }
        }
        egui::ComboBox::from_id_salt("brush category")
            .selected_text(&self.brush_category)
            .show_ui(ui, |ui| {
                for category in categories {
                    ui.selectable_value(&mut self.brush_category, category.clone(), category);
                }
            });
        let search = self.brush_search.to_lowercase();
        let mut count = 0;
        egui::ScrollArea::vertical()
            .id_salt("brush library")
            .max_height(350.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut section = None;
                for preset in &presets {
                    if self.brush_category != "All"
                        && !(self.brush_category == "Custom" && preset.custom)
                        && self.brush_category != preset.category
                    {
                        continue;
                    }
                    if !format!("{} {}", preset.name, preset.category)
                        .to_lowercase()
                        .contains(&search)
                    {
                        continue;
                    }
                    count += 1;
                    if section != Some(preset.category.as_str()) {
                        if section.is_some() {
                            ui.add_space(6.);
                        }
                        ui.label(RichText::new(&preset.category).small().color(ACCENT));
                        section = Some(preset.category.as_str());
                    }
                    let cached = self
                        .brush_thumbnails
                        .get(&preset.id)
                        .filter(|(settings, _)| *settings == preset.settings)
                        .map(|(_, texture)| texture.clone());
                    let texture = cached.or_else(|| {
                        let raster =
                            crate::brush_library::preview(preset.settings, 240, 36).ok()?;
                        let texture = ui.ctx().load_texture(
                            format!("brush {}", preset.id),
                            egui::ColorImage::from_rgba_unmultiplied([240, 36], &raster.rgba()),
                            egui::TextureOptions::LINEAR,
                        );
                        self.brush_thumbnails
                            .insert(preset.id.clone(), (preset.settings, texture.clone()));
                        Some(texture)
                    });
                    let selected = self.brush_preset.as_deref() == Some(&preset.id);
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 52.0),
                        egui::Sense::click(),
                    );
                    if response.hovered() || selected {
                        ui.painter().rect_filled(
                            rect,
                            0,
                            Color32::from_white_alpha(if selected { 12 } else { 5 }),
                        );
                    }
                    ui.painter().text(
                        rect.left_top() + Vec2::new(5., 7.),
                        egui::Align2::LEFT_TOP,
                        &preset.name,
                        egui::FontId::proportional(12.),
                        if selected { ACCENT } else { Color32::WHITE },
                    );
                    if preset.custom {
                        ui.painter().text(
                            rect.right_top() - Vec2::new(5., -7.),
                            egui::Align2::RIGHT_TOP,
                            "Custom",
                            egui::FontId::proportional(10.),
                            MUTED,
                        );
                    }
                    if let Some(texture) = texture {
                        ui.painter().image(
                            texture.id(),
                            Rect::from_min_size(
                                rect.left_bottom() - Vec2::new(-5., 33.),
                                Vec2::new(240., 32.),
                            ),
                            Rect::from_min_max(Pos2::ZERO, egui::pos2(1., 1.)),
                            Color32::WHITE,
                        );
                    }
                    if response
                        .on_hover_text(format!(
                            "{} · {}{}",
                            preset.name,
                            preset.category,
                            if preset.category == "Blend" {
                                " · choose Smudge to blend existing paint"
                            } else {
                                ""
                            }
                        ))
                        .clicked()
                    {
                        self.select_brush_preset(preset);
                    }
                }
                if count == 0 {
                    ui.label(RichText::new("No matching brushes").small().color(MUTED));
                }
            });
        ui.add_space(6.);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Name").small().color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut self.brush_name).desired_width(215.));
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Category").small().color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut self.brush_save_category).desired_width(195.));
        });
        let custom = self
            .brush_preset
            .as_ref()
            .is_some_and(|id| presets.iter().any(|p| p.id == *id && p.custom));
        ui.horizontal(|ui| {
            if ui
                .small_button("Save new")
                .on_hover_text("Save the current settings as a custom brush")
                .clicked()
            {
                self.save_brush_preset(false);
            }
            if ui
                .add_enabled(custom, egui::Button::new("Update").small())
                .clicked()
            {
                self.save_brush_preset(true);
            }
            if ui
                .add_enabled(custom, egui::Button::new("Delete").small())
                .clicked()
            {
                let result = library
                    .lock()
                    .unwrap()
                    .delete(self.brush_preset.as_deref().unwrap());
                match result {
                    Ok(()) => {
                        self.brush_preset = None;
                        self.shared.lock().unwrap().status = "Custom brush deleted".into();
                    }
                    Err(e) => self.shared.lock().unwrap().status = e.into(),
                }
            }
        });
        if let Some(error) = error {
            ui.label(RichText::new(error).small().color(Color32::LIGHT_RED));
        }
    }
    pub(super) fn select_brush_preset(&mut self, preset: &crate::brush_library::Preset) {
        if preset.category == "Blend" {
            self.select_tool(Tool::Smudge);
        } else if self.tool == Tool::Smudge {
            self.select_tool(Tool::Brush);
        }
        self.brush = preset.settings;
        self.radius = preset.settings.radius;
        self.brush_preset = Some(preset.id.clone());
        self.brush_name = preset.name.clone();
        self.brush_save_category = preset.category.clone();
        self.store_current_tool_profile();
    }
    pub(super) fn save_brush_preset(&mut self, update: bool) {
        let library = self.shared.lock().unwrap().brush_library.clone();
        let mut settings = self.brush;
        settings.radius = self.radius;
        let result = library.lock().unwrap().save(
            if update {
                self.brush_preset.as_deref()
            } else {
                None
            },
            &self.brush_name,
            &self.brush_save_category,
            settings,
        );
        match result {
            Ok(preset) => {
                self.select_brush_preset(&preset);
                self.shared.lock().unwrap().status = "Custom brush saved".into();
            }
            Err(error) => self.shared.lock().unwrap().status = error.into(),
        }
    }
}

#[cfg(test)]
#[path = "brush_commit_tests.rs"]
mod commit_tests;
