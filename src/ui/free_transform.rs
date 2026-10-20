//! One source-bound affine preview; the shared engine owns every pixel and guard.
use super::*;

#[derive(Clone, PartialEq)]
struct Parameters {
    offset: [f64; 2],
    scale: [f64; 2],
    angle: f64,
    skew: [f64; 2],
    mirror: [bool; 2],
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            offset: [0.; 2],
            scale: [1.; 2],
            angle: 0.,
            skew: [0.; 2],
            mirror: [false; 2],
        }
    }
}
#[derive(Clone, Copy)]
enum Handle {
    Move,
    Rotate,
    Scale(bool, bool),
    Skew(usize),
    Pivot,
}
struct Drag {
    origin: [f64; 2],
    start: Parameters,
    handle: Handle,
}
struct Reference {
    point: [f64; 2],
    values: Parameters,
    snap: bool,
    command: Value,
    matrix: [f64; 6],
    offset: [f64; 2],
}

pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
    roots: Vec<String>,
    mask: bool,
    step: Option<String>,
    selected: bool,
    whole: bool,
    bounds: [f64; 4],
    whole_bounds: [f64; 4],
    pivot: [f64; 2],
    reference: Option<Reference>,
    values: Parameters,
    mode: Tool,
    skew_mode: bool,
    proportional: bool,
    snap: bool,
    prior_tool: Tool,
    gesture: String,
    drag: Option<Drag>,
    pub error: Option<String>,
    pub rendered: Option<String>,
    #[cfg(test)]
    inputs: [Option<Rect>; 9],
}
impl Editor {
    pub(super) fn mode(&self) -> Option<Tool> {
        (!self.skew_mode).then_some(self.mode)
    }
    pub(super) fn set_mode(&mut self, mode: Tool) {
        self.mode = mode;
        self.skew_mode = false;
        self.drag = None;
    }
    pub(super) fn live_key(&self) -> String {
        format!(
            "free-transform:{}:{}:{}",
            self.gesture, self.document, self.revision
        )
    }
    fn affine(&self) -> bool {
        self.values.scale != [1.; 2]
            || self.values.angle.rem_euclid(360.) != 0.
            || self.values.skew != [0.; 2]
            || self.values.mirror != [false; 2]
    }
    fn raw(&self, id: &str, snap: bool) -> Value {
        json!({"op":if self.affine() {"transform"}else{"move"},"layer":id,
            "dx":self.values.offset[0],"dy":self.values.offset[1],
            "angle":self.values.angle,"scale_x":self.values.scale[0],"scale_y":self.values.scale[1],
            "skew_x":self.values.skew[0],"skew_y":self.values.skew[1],
            "mirror_x":self.values.mirror[0],"mirror_y":self.values.mirror[1],"pivot":self.pivot,
            "snap":snap,"mask":self.mask,"step":if self.roots.len()==1 {self.step.clone()}else{None},
            "selection_only":self.selected&&!self.whole,"document_id":self.document,"source_revision":self.revision})
    }
    fn matrix(command: &Value) -> Result<[f64; 6], String> {
        let [dx, dy] = crate::transform::translation(command)?;
        if command["op"] == "move" {
            return Ok([1., 0., 0., 1., dx, dy]);
        }
        let (sx, sy) = crate::transform::scales(command)?;
        let (sin, cos) = command["angle"]
            .as_f64()
            .unwrap_or(0.)
            .to_radians()
            .sin_cos();
        let [a, b, c, d] =
            crate::transform::skewed([cos * sx, sin * sx, -sin * sy, cos * sy], command)?;
        let [px, py] = [
            command["pivot"][0].as_f64().unwrap_or(0.),
            command["pivot"][1].as_f64().unwrap_or(0.),
        ];
        Ok([
            a,
            b,
            c,
            d,
            px - a * px - c * py + dx,
            py - b * px - d * py + dy,
        ])
    }
    fn command(&self, id: &str, snap: bool) -> Result<Value, String> {
        let mut command = crate::transform::resolve_command(&self.raw(id, snap))?;
        let Some(reference) = &self.reference else {
            return Ok(command);
        };
        if self.values == reference.values && snap == reference.snap {
            // Reference movement is presentation only, including exact command bytes.
            let mut frozen = reference.command.clone();
            frozen["layer"] = json!(id);
            frozen["selection_only"] = json!(self.selected && !self.whole);
            return Ok(frozen);
        }
        let offset = crate::transform::translation(&command)?;
        let [a, b, c, d, _, _] = Self::matrix(&command)?;
        let [e, f, g, h, tx, ty] = reference.matrix;
        let det = e * h - f * g;
        let [u, v, w, z] = [
            (a * h - c * f) / det,
            (b * h - d * f) / det,
            (-a * g + c * e) / det,
            (-b * g + d * e) / det,
        ];
        let [qx, qy] = reference.point;
        let [px, py] = self.pivot;
        let delta = if [a, b, c, d] == [e, f, g, h] {
            let old = crate::transform::translation(&reference.command)?;
            [
                old[0] + offset[0] - reference.offset[0],
                old[1] + offset[1] - reference.offset[1],
            ]
        } else {
            [
                qx + u * (tx - qx) + w * (ty - qy) + offset[0] - reference.offset[0] - px
                    + a * px
                    + c * py,
                qy + v * (tx - qx) + z * (ty - qy) + offset[1] - reference.offset[1] - py
                    + b * px
                    + d * py,
            ]
        };
        command["dx"] = json!(delta[0]);
        command["dy"] = json!(delta[1]);
        if delta.iter().any(|value| value.fract() != 0.) {
            command["op"] = json!("transform");
            command["fractional_translation"] = json!(true);
        }
        // User values were snapped above; never round the reference compensation.
        command["snap"] = json!(false);
        crate::transform::resolve_command(&command)
    }
    fn reference_point(&self) -> [f64; 2] {
        let offset = crate::transform::resolve_command(&self.raw("", self.snap))
            .and_then(|command| crate::transform::translation(&command))
            .unwrap_or([0.; 2]);
        if let Some(reference) = &self.reference {
            [
                reference.point[0] + offset[0] - reference.offset[0],
                reference.point[1] + offset[1] - reference.offset[1],
            ]
        } else {
            [self.pivot[0] + offset[0], self.pivot[1] + offset[1]]
        }
    }
    fn set_reference(&mut self, point: [f64; 2]) -> Result<(), String> {
        if point
            .iter()
            .any(|value| !value.is_finite() || value.abs() > 100000.)
        {
            return Err("Reference point is outside the coordinate limits".into());
        }
        if point == self.reference_point() {
            return Ok(());
        }
        if self.reference.is_none() && self.values == Parameters::default() {
            self.pivot = point;
            return Ok(());
        }
        let command = self.command("", self.snap)?;
        let matrix = Self::matrix(&command)?;
        let offset = crate::transform::translation(&crate::transform::resolve_command(
            &self.raw("", self.snap),
        )?)?;
        self.reference = Some(Reference {
            point,
            values: self.values.clone(),
            snap: self.snap,
            command,
            matrix,
            offset,
        });
        Ok(())
    }
    pub(super) fn commands(&self) -> Result<Vec<Value>, String> {
        let command = self.command("", self.snap)?;
        if command["op"] == "move" && crate::transform::translation(&command)? == [0.; 2] {
            return Ok(vec![]);
        }
        self.roots
            .iter()
            .map(|id| self.command(id, self.snap))
            .collect()
    }
    pub(super) fn key(&self) -> Result<String, String> {
        self.commands()
            .map(|commands| serde_json::to_string(&commands).unwrap())
    }
    fn point(&self, p: [f64; 2]) -> [f64; 2] {
        let Ok(c) = self.command("", self.snap) else {
            return p;
        };
        let (sx, sy) = crate::transform::scales(&c).unwrap_or((1., 1.));
        let (sin, cos) = c["angle"].as_f64().unwrap_or(0.).to_radians().sin_cos();
        let [a, b, cc, d] = crate::transform::skewed([cos * sx, sin * sx, -sin * sy, cos * sy], &c)
            .unwrap_or([1., 0., 0., 1.]);
        let [dx, dy] = if c["op"] == "move" {
            [
                c["dx"].as_f64().unwrap_or(0.).round(),
                c["dy"].as_f64().unwrap_or(0.).round(),
            ]
        } else {
            crate::transform::translation(&c).unwrap_or([0.; 2])
        };
        [
            self.pivot[0] + a * (p[0] - self.pivot[0]) + cc * (p[1] - self.pivot[1]) + dx,
            self.pivot[1] + b * (p[0] - self.pivot[0]) + d * (p[1] - self.pivot[1]) + dy,
        ]
    }
    fn corners(&self) -> [[f64; 2]; 4] {
        let [l, t, r, b] = if self.selected && !self.whole {
            self.bounds
        } else {
            self.whole_bounds
        };
        [[l, t], [r, t], [r, b], [l, b]].map(|p| self.point(p))
    }
    fn changed(&mut self) {
        self.error = None;
        self.rendered = None;
    }
    fn drag_to(&mut self, p: [f64; 2], shift: bool) {
        let Some(drag) = &self.drag else {
            return;
        };
        let start = drag.start.clone();
        let origin = drag.origin;
        let handle = drag.handle;
        self.values = start.clone();
        match handle {
            Handle::Pivot => {
                if let Err(error) =
                    self.set_reference(p.map(|value| value.clamp(-100000., 100000.)))
                {
                    self.error = Some(error);
                }
            }
            Handle::Move => {
                for i in 0..2 {
                    self.values.offset[i] =
                        (start.offset[i] + p[i] - origin[i]).clamp(-100000., 100000.);
                }
            }
            Handle::Rotate => {
                let center = self.reference_point();
                self.values.angle = (start.angle
                    + ((p[1] - center[1]).atan2(p[0] - center[0])
                        - (origin[1] - center[1]).atan2(origin[0] - center[0]))
                    .to_degrees())
                .clamp(-100000., 100000.);
            }
            Handle::Scale(x, y) => {
                let (sin, cos) = start.angle.to_radians().sin_cos();
                let center = self.reference_point();
                let local = |point: [f64; 2]| {
                    let dx = point[0] - center[0];
                    let dy = point[1] - center[1];
                    [dx * cos + dy * sin, -dx * sin + dy * cos]
                };
                let a = local(origin);
                let b = local(p);
                let ratio = |i: usize| {
                    if a[i].abs() > 0.00001 {
                        b[i] / a[i]
                    } else {
                        1.
                    }
                };
                let mut factors = [if x { ratio(0) } else { 1. }, if y { ratio(1) } else { 1. }];
                let reflected = factors.map(|factor| factor < 0.);
                if shift || (self.proportional && x && y) {
                    let factor = if !x {
                        factors[1]
                    } else if !y || (factors[0].abs() - 1.).abs() > (factors[1].abs() - 1.).abs() {
                        factors[0]
                    } else {
                        factors[1]
                    };
                    factors = [factor.abs(); 2];
                }
                for i in 0..2 {
                    self.values.scale[i] = (start.scale[i] * factors[i].abs()).clamp(0.05, 20.);
                    self.values.mirror[i] = start.mirror[i] ^ reflected[i];
                }
            }
            Handle::Skew(axis) => {
                if p == origin && !shift {
                    return;
                }
                let Ok(command) = self.command("", self.snap) else {
                    return;
                };
                let (sx, sy) = crate::transform::scales(&command).unwrap();
                let (sin, cos) = command["angle"].as_f64().unwrap().to_radians().sin_cos();
                let center = self.reference_point();
                let local = |point: [f64; 2]| {
                    [
                        (point[0] - center[0]) * cos + (point[1] - center[1]) * sin,
                        -(point[0] - center[0]) * sin + (point[1] - center[1]) * cos,
                    ]
                };
                let a = local(origin);
                let b = local(p);
                let ky = command["skew_y"].as_f64().unwrap().to_radians().tan();
                // Derivative of R*Scale*Hy*Hx about the current reference.
                // X moves along the already vertically sheared horizontal edge.
                let direction = if axis == 0 {
                    let v = a[1] / sy - ky * a[0] / sx;
                    [sx * v, sy * ky * v]
                } else {
                    [0., sy * a[0] / sx]
                };
                let squared = direction[0] * direction[0] + direction[1] * direction[1];
                if squared <= 1e-10 {
                    return;
                }
                let tangent = command[if axis == 0 { "skew_x" } else { "skew_y" }]
                    .as_f64()
                    .unwrap()
                    .to_radians()
                    .tan()
                    + ((b[0] - a[0]) * direction[0] + (b[1] - a[1]) * direction[1]) / squared;
                self.values.skew[axis] = tangent
                    .atan()
                    .to_degrees()
                    .clamp(-crate::transform::SKEW_LIMIT, crate::transform::SKEW_LIMIT);
            }
        }
        if !matches!(handle, Handle::Pivot) {
            if let Ok(c) = crate::transform::resolve_command(&self.raw("", self.snap || shift)) {
                self.values.offset = [
                    c["dx"].as_f64().unwrap_or(0.),
                    c["dy"].as_f64().unwrap_or(0.),
                ];
                if c["op"] == "transform" {
                    self.values.scale = [
                        c["scale_x"].as_f64().unwrap(),
                        c["scale_y"].as_f64().unwrap(),
                    ];
                    self.values.angle = c["angle"].as_f64().unwrap();
                    if matches!(handle, Handle::Skew(_)) {
                        self.values.skew =
                            [c["skew_x"].as_f64().unwrap(), c["skew_y"].as_f64().unwrap()];
                    }
                }
            }
        }
    }
}
impl PeerBrush {
    fn free_transform_keyboard(ctx: &egui::Context) -> bool {
        ctx.top_layer_id()
            == Some(egui::LayerId::new(
                egui::Order::Middle,
                egui::Id::new("free transform"),
            ))
            && ctx.memory(|memory| !memory.any_popup_open() && memory.top_modal_layer().is_none())
    }
    pub(super) fn free_transform_action(&mut self, parameters: &Value) -> bool {
        let Some(editor) = &mut self.free_transform else {
            return false;
        };
        for (i, key) in ["mirror_x", "mirror_y"].iter().enumerate() {
            if parameters[*key] == true {
                editor.values.mirror[i] = !editor.values.mirror[i];
            }
        }
        editor.values.angle = (editor.values.angle + parameters["angle"].as_f64().unwrap_or(0.))
            .clamp(-100000., 100000.);
        editor.changed();
        self.last_preview = None;
        true
    }
    fn free_transform_stale(&self, doc: &Document) -> bool {
        self.free_transform.as_ref().is_some_and(|editor| {
            editor.document != doc.id
                || editor.revision != doc.revision
                || editor.roots
                    != crate::tree::roots(doc, &self.selection_layers.iter().cloned().collect())
                || editor.mask != self.mask
                || editor.step != self.mask_step
        })
    }
    pub(super) fn open_free_transform(&mut self, ctx: &egui::Context, doc: &Document) {
        if self.free_transform.is_some() {
            return;
        }
        if self.update_interaction_active()
            || doc.read_only
            || self.lifecycle_frame
            || self.pen.in_gesture()
            || self.drag_start.is_some()
            || self.size_drag.is_some()
            || self.shortcuts.open
            || ctx.wants_keyboard_input()
            || ctx.input(|i| i.pointer.any_down())
        {
            self.message =
                "Finish or cancel the current tool or text edit before Free Transform.".into();
            return;
        }
        let roots = crate::tree::roots(doc, &self.selection_layers.iter().cloned().collect());
        if roots.is_empty() {
            self.message = "Select a layer or folder to transform.".into();
            return;
        }
        let tree = crate::transform::tree_ids(doc, &roots);
        if doc
            .layers
            .iter()
            .any(|layer| tree.contains(&layer.id) && layer.locked)
        {
            self.message = "Unlock the selected content before Free Transform.".into();
            return;
        }
        let whole_bounds = crate::transform::tree_bounds(doc, &roots)
            .unwrap_or([0, 0, doc.width as i32, doc.height as i32])
            .map(f64::from);
        let b = doc
            .selection
            .map(|bounds| bounds.map(f64::from))
            .unwrap_or(whole_bounds);
        let prior_tool = self.tool;
        self.select_tool(Tool::Move);
        self.close_proposal();
        self.isolate = false;
        self.free_transform = Some(Editor {
            document: doc.id.clone(),
            revision: doc.revision,
            roots,
            mask: self.mask,
            step: self.mask_step.clone(),
            selected: doc.selection.is_some(),
            whole: doc.selection.is_none(),
            bounds: b,
            whole_bounds,
            pivot: [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5],
            reference: None,
            values: Parameters::default(),
            mode: Tool::Move,
            skew_mode: false,
            proportional: true,
            snap: false,
            prior_tool,
            gesture: crate::engine::id(),
            drag: None,
            error: None,
            rendered: None,
            #[cfg(test)]
            inputs: [None; 9],
        });
        self.free_transform_frame = true;
        self.last_preview = None;
        ctx.request_repaint();
    }
    pub(super) fn cancel_free_transform(&mut self) {
        if let Some(editor) = self.free_transform.take() {
            self.select_tool(editor.prior_tool);
            self.free_transform_frame = true;
            self.free_transform_cancelled_pointer = true;
            self.transient.clear();
            self.canvas_selection = None;
            self.last_preview = None;
        }
    }
    pub(super) fn sync_free_transform(&mut self, ctx: &egui::Context, doc: &Document) {
        let lost = ctx.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::WindowFocused(false)))
        });
        let other = self.lifecycle.is_some()
            || self.lifecycle_frame
            || self.geometry.is_some()
            || self.source_editor.is_some()
            || self.filter_editor.is_some()
            || self.refinement.is_some()
            || self.show_new
            || self.busy
            || self.shortcuts.open
            || !self.jobs.is_empty()
            || self.load_control.is_some()
            || self.pending_import.is_some()
            || self.pending_image_open.is_some();
        if self.free_transform.is_some() && (lost || other || self.free_transform_stale(doc)) {
            self.cancel_free_transform();
            self.message="Free Transform cancelled after focus, target or source changed; current work is preserved.".into();
        }
    }
    pub(super) fn free_transform_keys(&mut self, ctx: &egui::Context, doc: &Document) {
        self.sync_free_transform(ctx, doc);
        ctx.input(|input| {
            for event in &input.events {
                if let egui::Event::Key {
                    key,
                    physical_key,
                    pressed,
                    ..
                } = event
                {
                    for key in std::iter::once(*key).chain(*physical_key) {
                        if *pressed {
                            self.selection_shortcut_keys.insert(key);
                        } else {
                            self.selection_shortcut_keys.remove(&key);
                        }
                    }
                }
            }
        });
        if ctx.wants_keyboard_input()
            || !Self::free_transform_keyboard(ctx)
            || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))))
        {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_free_transform();
            self.message = "Free Transform cancelled.".into();
            return;
        }
        if let Some(editor) = &mut self.free_transform {
            for (action, tool) in [
                ("Move", Tool::Move),
                ("Rotate artwork", Tool::Rotate),
                ("Scale artwork", Tool::Scale),
            ] {
                if ctx.input(|input| self.shortcuts.pressed(action, input)) {
                    editor.set_mode(tool);
                }
            }
        }
    }
    pub(super) fn free_transform_canvas(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        rect: Rect,
        scale: f32,
        available: Rect,
    ) {
        self.sync_free_transform(ui.ctx(), doc);
        let Some(mut editor) = self.free_transform.take() else {
            return;
        };
        let before = editor.values.clone();
        let old_reference = editor.reference_point();
        let view = self.view;
        let screen = |p: [f64; 2]| view.point(p.map(|v| v as f32), rect, scale);
        let corners = editor.corners().map(screen);
        let middle = (corners[0] + corners[1].to_vec2()) * 0.5;
        let center = screen(editor.reference_point());
        let rotation = middle + (middle - center).normalized() * 26.;
        let points = [
            corners[0],
            corners[1],
            corners[2],
            corners[3],
            (corners[0] + corners[1].to_vec2()) * 0.5,
            (corners[1] + corners[2].to_vec2()) * 0.5,
            (corners[2] + corners[3].to_vec2()) * 0.5,
            (corners[3] + corners[0].to_vec2()) * 0.5,
        ];
        let navigating = ui.input(|i| {
            i.modifiers.alt
                || i.key_down(egui::Key::Space)
                || i.pointer.middle_down()
                || i.pointer.secondary_down()
        });
        if navigating {
            editor.drag = None;
        }
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hit = pointer
            .filter(|p| {
                !navigating
                    && available.contains(*p)
                    && ui.ctx().layer_id_at(*p) == Some(ui.layer_id())
            })
            .and_then(|p| {
                if p.distance(center) < 9. {
                    return Some(Handle::Pivot);
                }
                if p.distance(rotation) < 10. {
                    return Some(Handle::Rotate);
                }
                if editor.skew_mode {
                    return points[4..]
                        .iter()
                        .position(|point| p.distance(*point) < 9.)
                        .map(|index| Handle::Skew(index % 2));
                }
                if editor.mode == Tool::Scale {
                    return points
                        .iter()
                        .position(|point| p.distance(*point) < 9.)
                        .map(|index| {
                            Handle::Scale(index < 4 || index % 2 == 1, index < 4 || index % 2 == 0)
                        });
                }
                if editor.mode == Tool::Rotate {
                    return points
                        .iter()
                        .any(|point| p.distance(*point) < 10.)
                        .then_some(Handle::Rotate);
                }
                Rect::from_points(&corners)
                    .contains(p)
                    .then_some(Handle::Move)
            });
        if hit.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        }
        if let (Some(handle), Some(p)) = (hit, pointer) {
            if ui.input(|i| i.pointer.primary_pressed()) {
                editor.drag = Some(Drag {
                    origin: view.document(p, rect, scale).map(f64::from),
                    start: editor.values.clone(),
                    handle,
                });
            }
        }
        if let Some(p) = pointer.filter(|_| editor.drag.is_some()) {
            editor.drag_to(
                view.document(p, rect, scale).map(f64::from),
                ui.input(|i| i.modifiers.shift),
            );
        }
        if ui.input(|i| i.pointer.primary_released()) {
            editor.drag = None;
        }
        if before != editor.values {
            editor.changed();
            self.last_preview = None;
        }
        if old_reference != editor.reference_point() {
            ui.ctx().request_repaint();
        }
        let painter = ui.painter().with_clip_rect(available);
        let corners = editor.corners().map(screen);
        let middle = (corners[0] + corners[1].to_vec2()) * 0.5;
        let center = screen(editor.reference_point());
        let rotation = middle + (middle - center).normalized() * 26.;
        painter.add(egui::Shape::closed_line(
            corners.to_vec(),
            Stroke::new(1.25, ACCENT),
        ));
        let mut handles = if editor.skew_mode {
            vec![]
        } else {
            corners.to_vec()
        };
        if editor.mode == Tool::Scale || editor.skew_mode {
            for i in 0..4 {
                handles.push((corners[i] + corners[(i + 1) % 4].to_vec2()) * 0.5);
            }
        }
        for (index, p) in handles.into_iter().enumerate() {
            painter.rect_filled(Rect::from_center_size(p, Vec2::splat(7.)), 1., ACCENT);
            if editor.skew_mode {
                painter.text(
                    p + Vec2::new(8., -5.),
                    egui::Align2::LEFT_BOTTOM,
                    if index % 2 == 0 { "X" } else { "Y" },
                    egui::FontId::proportional(11.),
                    ACCENT,
                );
            }
        }
        painter.circle_stroke(rotation, 5., Stroke::new(1.5, ACCENT));
        painter.line_segment([middle, rotation], Stroke::new(1., ACCENT));
        painter.circle_stroke(center, 5., Stroke::new(1., Color32::WHITE));
        if let Ok(commands) = editor.commands() {
            self.transient = commands;
        }
        self.free_transform = Some(editor);
    }
    pub(super) fn free_transform_window(
        &mut self,
        ctx: &egui::Context,
        doc: &Document,
        drawn_generation: u64,
    ) {
        self.sync_free_transform(ctx, doc);
        let Some(mut editor) = self.free_transform.take() else {
            return;
        };
        let typing = ctx.wants_keyboard_input()
            || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))));
        let before = editor.key();
        let mut open = true;
        let (mut apply, mut cancel) = (false, false);
        egui::Window::new("Free Transform")
            .id(egui::Id::new("free transform"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(330.)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (mode, label) in [
                        (Tool::Move, "Move"),
                        (Tool::Rotate, "Rotate"),
                        (Tool::Scale, "Scale"),
                    ] {
                        if ui.selectable_label(!editor.skew_mode && editor.mode == mode, label).clicked() {
                            editor.set_mode(mode);
                        }
                    }
                    if ui.selectable_label(editor.skew_mode, "Skew").clicked() {
                        editor.skew_mode = true;
                        editor.drag = None;
                    }
                });
                if editor.skew_mode {
                    ui.label("Top/bottom: X · Left/right: Y · Shift: 15° snap.")
                        .on_hover_text("Drag an edge handle to shear the actual artwork. An edge on the reference axis stays fixed; drag the opposite edge or move the reference point. Release keeps the preview pending.");
                } else {
                    ui.label("Drag handles or enter values. Release keeps the preview pending.");
                }
                #[cfg(test)]
                let mut input_index = 0;
                #[cfg(test)]
                let mut inputs = [None; 9];
                let number = |ui: &mut egui::Ui,
                              label: &str,
                              value: &mut f64,
                              range: std::ops::RangeInclusive<f64>| {
                    let response = ui
                        .horizontal(|ui| {
                            ui.add_sized([82., 20.], egui::Label::new(label));
                            controls::numeric(ui, value, range)
                        })
                        .inner;
                    #[cfg(test)]
                    {
                        inputs[input_index] = Some(response.rect);
                        input_index += 1;
                    }
                    response.changed()
                };
                #[cfg(test)]
                let mut number = number;
                number(
                    ui,
                    "Move X",
                    &mut editor.values.offset[0],
                    -100000. ..=100000.,
                );
                number(
                    ui,
                    "Move Y",
                    &mut editor.values.offset[1],
                    -100000. ..=100000.,
                );
                let old = editor.values.scale;
                if number(ui, "Scale X", &mut editor.values.scale[0], 0.05..=20.)
                    && editor.proportional
                {
                    editor.values.scale[1] =
                        (old[1] * editor.values.scale[0] / old[0]).clamp(0.05, 20.);
                }
                let old = editor.values.scale;
                if number(ui, "Scale Y", &mut editor.values.scale[1], 0.05..=20.)
                    && editor.proportional
                {
                    editor.values.scale[0] =
                        (old[0] * editor.values.scale[1] / old[1]).clamp(0.05, 20.);
                }
                number(ui, "Angle", &mut editor.values.angle, -100000. ..=100000.);
                number(
                    ui,
                    "Skew X",
                    &mut editor.values.skew[0],
                    -crate::transform::SKEW_LIMIT..=crate::transform::SKEW_LIMIT,
                );
                number(
                    ui,
                    "Skew Y",
                    &mut editor.values.skew[1],
                    -crate::transform::SKEW_LIMIT..=crate::transform::SKEW_LIMIT,
                );
                ui.horizontal(|ui| {
                    ui.checkbox(&mut editor.proportional, "Proportional");
                    ui.checkbox(&mut editor.snap, "Snap");
                });
                ui.horizontal(|ui| {
                    ui.checkbox(&mut editor.values.mirror[0], "Flip H");
                    ui.checkbox(&mut editor.values.mirror[1], "Flip V");
                });
                if editor.selected {
                    ui.checkbox(&mut editor.whole, "Whole target (keep selection)");
                }
                let mut reference = editor.reference_point();
                let old_reference = reference;
                number(ui, "Reference X", &mut reference[0], -100000. ..=100000.);
                number(ui, "Reference Y", &mut reference[1], -100000. ..=100000.);
                #[cfg(test)]
                {
                    editor.inputs = inputs;
                }
                if reference != old_reference {
                    if let Err(error) = editor.set_reference(reference) {
                        editor.error = Some(error);
                    }
                    ctx.request_repaint();
                }
                if ui.button("Reset transform").clicked() {
                    if editor.reference.is_some() {
                        editor.pivot = editor.reference_point();
                    }
                    editor.reference = None;
                    editor.values = Parameters::default();
                    editor.drag = None;
                    editor.changed();
                }
                if editor.key() != before {
                    editor.changed();
                }
                let commands = editor.commands();
                if let Some(error) = commands.as_ref().err().or(editor.error.as_ref()) {
                    ui.colored_label(ACCENT, error);
                }
                let rendered = editor
                    .key()
                    .ok()
                    .is_some_and(|key| editor.rendered.as_ref() == Some(&key));
                if !rendered {
                    ui.label("Rendering shared-engine preview...");
                }
                ui.label("Affine only. Perspective and Warp are not available.");
                ui.horizontal(|ui| {
                    let ready = commands.is_ok()
                        && rendered
                        && editor.drag.is_none()
                        && !self.busy
                        && drawn_generation == self.canvas_generation;
                    apply = ui.add_enabled(ready, dialogs::primary("Apply")).clicked()
                        || (ready
                            && !typing
                            && !ctx.wants_keyboard_input()
                            && Self::free_transform_keyboard(ctx)
                            && ctx.input(|i| i.key_pressed(egui::Key::Enter)));
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if !open || cancel {
            self.free_transform = Some(editor);
            self.cancel_free_transform();
            self.message = "Free Transform cancelled.".into();
        } else if apply {
            let commands = editor.commands().unwrap();
            let unchanged = commands.is_empty();
            let result = if unchanged {
                Ok(json!({}))
            } else {
                self.shared.lock().unwrap().edit(
                    "human",
                    &commands,
                    Some(editor.revision),
                    None,
                    "Free Transform",
                )
            };
            match result {
                Ok(_) => {
                    self.free_transform = Some(editor);
                    self.cancel_free_transform();
                    self.message = if unchanged {
                        "Free Transform unchanged."
                    } else {
                        "Free Transform applied; one Undo restores the starting artwork."
                    }
                    .into();
                }
                Err(error) => {
                    editor.error = Some(error.clone());
                    self.message = error;
                    self.free_transform = Some(editor);
                }
            }
        } else {
            self.transient = editor.commands().unwrap_or_default();
            if editor.key() != before {
                self.last_preview = None;
                ctx.request_repaint();
            }
            self.free_transform = Some(editor);
        }
    }
}

#[cfg(test)]
mod tests;
