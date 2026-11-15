//! One source-bound transform preview; the shared engine owns every pixel and guard.
use super::*;

fn mode_choice(ui: &mut egui::Ui, menu: bool, icon: Icon, label: &str, selected: bool) -> bool {
    let min_width = if menu { ui.available_width() } else { 0. };
    let clicked = icons::labeled_choice(ui, icon, label, Some(selected), min_width).clicked();
    if clicked && menu {
        ui.close_menu();
    }
    clicked
}

fn rotation_handle([top_left, top_right, _, bottom_left]: [Pos2; 4], middle: Pos2) -> Pos2 {
    let top = top_right - top_left;
    let mut outward = Vec2::new(top.y, -top.x).normalized();
    if outward.dot(bottom_left - top_left) > 0. {
        outward = -outward;
    }
    middle + outward * 26.
}

fn near_transform_edge(point: Pos2, corners: [Pos2; 4]) -> bool {
    (0..4).any(|i| {
        let start = corners[i];
        let edge = corners[(i + 1) % 4] - start;
        let length_sq = edge.length_sq();
        let along = if length_sq > 0. {
            ((point - start).dot(edge) / length_sq).clamp(0., 1.)
        } else {
            0.
        };
        point.distance_sq(start + edge * along) < 22. * 22.
    })
}

fn paint_warp_grid(painter: &egui::Painter, nodes: [Pos2; 9], stroke: Stroke) {
    for line in [[0, 1, 2], [3, 4, 5], [6, 7, 8], [0, 3, 6], [1, 4, 7], [2, 5, 8]] {
        painter.add(egui::Shape::line(line.map(|i| nodes[i]).to_vec(), stroke));
    }
}

fn warp_preset_button(ui: &mut egui::Ui, name: &str, label: &str, selected: bool, amount: f64) -> egui::Response {
    let response = ui
        .add_sized([65., 56.], egui::Button::new("").frame(false).selected(selected))
        .on_hover_text(label);
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, response.enabled(), selected, label)
    });
    let visuals = ui.style().interact_selectable(&response, selected);
    if selected || response.hovered() {
        ui.painter().rect_filled(response.rect, 3., visuals.weak_bg_fill);
    }
    let color = visuals.text_color();
    let top = response.rect.center_top();
    if let Ok(grid) = crate::warp::Grid::preset([0., 0., 1., 1.], name, amount) {
        let nodes = grid.points.map(|[x, y]| {
            top + Vec2::new((x as f32 - 0.5) * 30., 9. + y as f32 * 20.)
        });
        paint_warp_grid(ui.painter(), nodes, Stroke::new(1., color));
    }
    ui.painter().text(
        top + Vec2::new(0., 43.), egui::Align2::CENTER_CENTER, label,
        egui::TextStyle::Button.resolve(ui.style()), color,
    );
    icons::paint_focus(ui, &response);
    response
}

#[derive(Clone, PartialEq)]
struct Quad {
    rect: [f64; 4],
    corners: [[f64; 2]; 4],
}
#[derive(Clone, Copy, PartialEq)]
enum ProjectiveMode {
    Distort,
    Perspective,
}
#[derive(Clone, PartialEq)]
struct Parameters {
    offset: [f64; 2],
    scale: [f64; 2],
    angle: f64,
    skew: [f64; 2],
    mirror: [bool; 2],
    projective: Option<Quad>,
    warp: Option<crate::warp::Grid>,
}
impl Default for Parameters {
    fn default() -> Self {
        Self {
            offset: [0.; 2],
            scale: [1.; 2],
            angle: 0.,
            skew: [0.; 2],
            mirror: [false; 2],
            projective: None,
            warp: None,
        }
    }
}
#[derive(Clone, Copy, serde::Serialize)]
enum Handle {
    Move,
    Rotate,
    Scale(bool, bool),
    Skew(usize),
    SkewShortcut(usize),
    Projective(usize, bool),
    DistortEdge(usize),
    PerspectiveShortcut(usize),
    Warp(usize),
    Pivot,
}

fn transform_modifier(modifiers: egui::Modifiers) -> bool {
    if cfg!(target_os = "macos") {
        modifiers.mac_cmd
    } else {
        modifiers.ctrl
    }
}

fn modifier_handle(index: usize, modifiers: egui::Modifiers, affine_only: bool) -> Option<Handle> {
    if !transform_modifier(modifiers) {
        return None;
    }
    match (index < 4, modifiers.alt, modifiers.shift) {
        (true, true, true) if !affine_only => Some(Handle::PerspectiveShortcut(index)),
        (false, false, true) => Some(Handle::SkewShortcut(index % 2)),
        (true, false, false) if !affine_only => Some(Handle::Projective(index, false)),
        (false, false, false) if !affine_only => Some(Handle::DistortEdge(index - 4)),
        _ => None,
    }
}

struct Drag {
    origin: [f64; 2],
    start: Parameters,
    handle: Handle,
    warp_preset: Option<&'static str>,
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
    native_source: Option<crate::engine::native::DocumentKey>,
    draft: Option<source::TransformDraft>,
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
    projective_mode: Option<ProjectiveMode>,
    warp_mode: bool,
    warp_domains: [Result<[f64; 4], String>; 2],
    warp_preset: Option<&'static str>,
    warp_amount: f64,
    context: egui::menu::MenuRootManager,
    context_frame: bool,
    context_secondary_typing: bool,
    text_focus: Option<egui::Id>,
    proportional: bool,
    snap: bool,
    prior_tool: Tool,
    gesture: String,
    pending: bool,
    canvas_apply: bool,
    geometry_pending: bool,
    drag: Option<Drag>,
    pub error: Option<String>,
    apply_error: Option<String>,
    pub rendered: Option<String>,
    #[cfg(test)]
    inputs: [Option<Rect>; 9],
    #[cfg(test)]
    projective_inputs: [Option<Rect>; 8],
    #[cfg(test)]
    warp_inputs: [Option<Rect>; 18],
    #[cfg(test)]
    warp_amount_input: Option<Rect>,
    #[cfg(test)]
    scroll_rect: Option<Rect>,
    #[cfg(test)]
    scroll_offset: Vec2,
}
impl Editor {
    pub(super) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    pub(super) fn observes(&self, doc: DocumentView<'_>) -> bool {
        self.document == doc.id()
            && self.revision == doc.revision()
            && self.native_source == doc.native().map(|native| native.key())
    }
    pub(super) fn edits_text_draft(&self) -> bool {
        self.draft.is_some()
    }

    pub(super) fn pointer_preview_key(&self) -> Option<impl serde::Serialize + '_> {
        let drag = self.drag.as_ref()?;
        (!self.pending && !self.geometry_pending && !matches!(drag.handle, Handle::Pivot)).then_some((
            &self.gesture,
            drag.origin,
            drag.handle,
            &self.roots,
            self.mask,
            &self.step,
            self.whole,
        ))
    }

    pub(super) fn mode(&self) -> Option<Tool> {
        (!self.skew_mode && self.projective_mode.is_none() && !self.warp_mode).then_some(self.mode)
    }

    fn scale_handles(&self) -> bool {
        matches!(self.mode(), Some(Tool::Move | Tool::Scale))
    }

    pub(super) fn set_mode(&mut self, mode: Tool) {
        if self.pending || self.geometry_pending {
            return;
        }
        self.mode = mode;
        self.skew_mode = false;
        self.projective_mode = None;
        self.warp_mode = false;
        self.drag = None;
    }
    pub(super) fn live_key(&self) -> String {
        format!(
            "free-transform:{}:{}:{}",
            self.gesture, self.document, self.revision
        )
    }
    fn show_context(&mut self, ui: &mut egui::Ui, available: Rect) -> bool {
        let was_open = self.context.is_some();
        let typing = ui.ctx().wants_keyboard_input()
            || ui.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Ime(_)))
            });
        // An outside press can end a numeric field before this gesture's release.
        if ui.input(|input| input.pointer.secondary_pressed()) {
            self.context_secondary_typing = typing;
        } else if ui
            .input(|input| input.pointer.secondary_down() || input.pointer.secondary_released())
        {
            self.context_secondary_typing |= typing;
        }
        if self.context.as_ref().is_some_and(|root| {
            ui.input(|input| {
                input.pointer.any_pressed()
                    && input
                        .pointer
                        .interact_pos()
                        .is_some_and(|p| !root.menu_state.read().area_contains(p))
            })
        }) {
            *self.context = None;
        }
        let navigating = ui.input(|input| {
            input.modifiers.alt || input.key_down(egui::Key::Space) || input.pointer.middle_down()
        });
        if navigating {
            *self.context = None;
        }
        let response = ui.interact(
            available,
            ui.id().with(("free transform context", &self.gesture)),
            egui::Sense::hover(),
        );
        if !navigating
            && !ui.input(|input| input.pointer.primary_down())
            && self.drag.is_none()
            && !typing
            && !self.context_secondary_typing
            && ui
                .ctx()
                .memory(|memory| !memory.any_popup_open() && memory.top_modal_layer().is_none())
            && ui.input(|input| input.pointer.button_clicked(egui::PointerButton::Secondary))
        {
            if let Some(at) = ui.input(|input| input.pointer.interact_pos()).filter(|at| {
                available.contains(*at) && ui.ctx().layer_id_at(*at) == Some(ui.layer_id())
            }) {
                *self.context = Some(egui::menu::MenuRoot::new(at, response.id));
                self.drag = None;
            }
        }
        let block_canvas = self.context_frame || was_open || self.context.is_some();
        self.context_frame = block_canvas;
        let mut context = std::mem::take(&mut self.context);
        context.show(&response, |ui| {
            self.affine_modes(ui, true);
            self.nonlinear_modes(ui, true);
            ui.separator();
            for (axis, icon, label) in [
                (0, Icon::MirrorHorizontal, "Flip Horizontal"),
                (1, Icon::MirrorVertical, "Flip Vertical"),
            ] {
                if icons::menu_choice(ui, icon, label, Some(self.values.mirror[axis])).clicked() {
                    self.values.mirror[axis] = !self.values.mirror[axis];
                    self.drag = None;
                    ui.close_menu();
                }
            }
        });
        self.context = context;
        if was_open != self.context.is_some() {
            ui.ctx().request_repaint();
        }
        block_canvas
    }

    fn affine_modes(&mut self, ui: &mut egui::Ui, menu: bool) {
        for (mode, icon, label) in [
            (Tool::Move, Icon::Move, "Move"),
            (Tool::Rotate, Icon::Rotate, "Rotate"),
            (Tool::Scale, Icon::Scale, "Scale"),
        ] {
            if mode_choice(ui, menu, icon, label, self.mode() == Some(mode)) {
                self.set_mode(mode);
            }
        }
        if mode_choice(ui, menu, Icon::Skew, "Skew", self.skew_mode) {
            self.skew_mode = true;
            self.projective_mode = None;
            self.warp_mode = false;
            self.drag = None;
        }
    }

    fn nonlinear_modes(&mut self, ui: &mut egui::Ui, menu: bool) {
        if self.draft.is_some() {
            return;
        }
        for (mode, icon, label) in [
            (ProjectiveMode::Distort, Icon::Distort, "Distort"),
            (ProjectiveMode::Perspective, Icon::Perspective, "Perspective"),
        ] {
            if mode_choice(ui, menu, icon, label, self.projective_mode == Some(mode)) {
                self.projective_mode = Some(mode);
                self.skew_mode = false;
                self.warp_mode = false;
                self.drag = None;
            }
        }
        let (icon, label) = if self.warp_mode && !menu {
            (Icon::Scale, "Back to Transform")
        } else {
            (Icon::GridWarp, "Grid Warp")
        };
        if mode_choice(ui, menu, icon, label, self.warp_mode && menu) {
            if self.warp_mode && !menu {
                self.set_mode(self.mode);
            } else {
                self.warp_mode = true;
                self.skew_mode = false;
                self.projective_mode = None;
                self.drag = None;
            }
        }
    }
    fn affine(&self) -> bool {
        self.values.scale != [1.; 2]
            || self.values.angle.rem_euclid(360.) != 0.
            || self.values.skew != [0.; 2]
            || self.values.mirror != [false; 2]
            || self.values.projective.is_some()
            || self.values.warp.is_some()
    }
    fn raw(&self, id: &str, snap: bool) -> Value {
        let fractional = self.edits_text_draft()
            && self.values.offset.iter().any(|offset| offset.fract() != 0.);
        let mut command = json!({"op":if self.affine() || fractional {"transform"}else{"move"},"layer":id,
            "dx":self.values.offset[0],"dy":self.values.offset[1],
            "angle":self.values.angle,"scale_x":self.values.scale[0],"scale_y":self.values.scale[1],
            "skew_x":self.values.skew[0],"skew_y":self.values.skew[1],
            "mirror_x":self.values.mirror[0],"mirror_y":self.values.mirror[1],"pivot":self.pivot,
            "snap":snap,"mask":self.mask,"step":if self.roots.len()==1 {self.step.clone()}else{None},
            "selection_only":self.selected&&!self.whole,"document_id":self.document,"source_revision":self.revision});
        if fractional {
            command["fractional_translation"] = json!(true);
        }
        command
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
    fn affine_command(&self, id: &str, snap: bool) -> Result<Value, String> {
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
    fn projection_command(&self, id: &str, snap: bool) -> Result<Value, String> {
        let mut command = self.affine_command(id, snap)?;
        command.as_object_mut().unwrap().remove("warp");
        if let Some(quad) = &self.values.projective {
            command["projective"] = json!({"rect":quad.rect,"quad":quad.corners});
        } else {
            command.as_object_mut().unwrap().remove("projective");
        }
        crate::transform::resolve_command(&command)
    }
    fn command(&self, id: &str, snap: bool) -> Result<Value, String> {
        let mut command = self.projection_command(id, snap)?;
        if let Some(grid) = &self.values.warp {
            command["warp"] = json!(grid);
        }
        crate::transform::resolve_command(&command)
    }
    fn warp_domain(&self) -> Result<[f64; 4], String> {
        self.warp_domains[usize::from(self.whole || !self.selected)].clone()
    }
    fn grid(&self) -> Result<crate::warp::Grid, String> {
        self.values
            .warp
            .clone()
            .map_or_else(|| crate::warp::Grid::identity(self.warp_domain()?), Ok)
    }
    fn set_grid(&mut self, grid: crate::warp::Grid) {
        self.values.warp = (!grid.is_identity()).then_some(grid);
    }
    fn post_map(&self) -> Result<crate::projective::Map, String> {
        crate::warp::CommandMap::new(&self.projection_command("", self.snap)?, self.pivot)
            .map(|map| map.after)
    }
    fn grid_points(&self) -> Result<[[f64; 2]; 9], String> {
        let mut points = self.grid()?.points;
        let map = self.post_map()?;
        for point in &mut points {
            *point = map.point(*point)?;
        }
        Ok(points)
    }
    fn nearest_warp_node(points: &[Pos2; 9], pointer: Pos2) -> Option<usize> {
        points
            .iter()
            .enumerate()
            .map(|(index, &point)| (index, point.distance_sq(pointer)))
            .filter(|&(_, distance)| distance < 9. * 9.)
            .min_by(|(_, a), (_, b)| a.total_cmp(b))
            .map(|(index, _)| index)
    }
    fn outline(&self) -> Vec<[f64; 2]> {
        if self.values.warp.is_some() && self.projective_mode.is_none() {
            let points = Self::rectangle(self.target_bounds()).map(|p| p.map(|v| v as f32));
            if let Ok(points) = self
                .command("", self.snap)
                .and_then(|c| crate::warp::CommandMap::new(&c, self.pivot))
                .and_then(|map| map.polygon(&points))
            {
                return points.into_iter().map(|p| p.map(f64::from)).collect();
            }
        }
        self.corners().to_vec()
    }
    fn edge_points(&self) -> [[f64; 2]; 4] {
        let corners = self.corners();
        let mut points = std::array::from_fn(|i| {
            [
                (corners[i][0] + corners[(i + 1) % 4][0]) * 0.5,
                (corners[i][1] + corners[(i + 1) % 4][1]) * 0.5,
            ]
        });
        if self.values.warp.is_some() && self.projective_mode.is_none() {
            if let Ok(map) = self
                .command("", self.snap)
                .and_then(|c| crate::warp::CommandMap::new(&c, self.pivot))
            {
                let source = Self::rectangle(self.target_bounds());
                for (i, point) in points.iter_mut().enumerate() {
                    *point = map
                        .point([
                            (source[i][0] + source[(i + 1) % 4][0]) * 0.5,
                            (source[i][1] + source[(i + 1) % 4][1]) * 0.5,
                        ])
                        .unwrap_or(*point);
                }
            }
        }
        points
    }
    fn target_bounds(&self) -> [f64; 4] {
        if self.selected && !self.whole {
            self.bounds
        } else {
            self.whole_bounds
        }
    }
    fn rectangle([l, t, r, b]: [f64; 4]) -> [[f64; 2]; 4] {
        [[l, t], [r, t], [r, b], [l, b]]
    }
    fn quad(&self) -> Result<Quad, String> {
        let rect = self.target_bounds();
        if let Some(quad) = &self.values.projective {
            if quad.rect == rect {
                return Ok(quad.clone());
            }
            let command =
                json!({"op":"transform","projective":{"rect":quad.rect,"quad":quad.corners}});
            if let Some(specification) = crate::projective::parse(&command)? {
                let mut corners = Self::rectangle(rect);
                for point in &mut corners {
                    *point = specification.map.point(*point)?;
                }
                return Ok(Quad { rect, corners });
            }
        }
        Ok(Quad {
            rect,
            corners: Self::rectangle(rect),
        })
    }
    fn set_quad(&mut self, quad: Quad) {
        self.values.projective = (quad.corners != Self::rectangle(quad.rect)).then_some(quad);
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
    fn reference_anchors(&self) -> [[f64; 2]; 9] {
        let [a, b, c, d] = self.corners();
        let between = |a: [f64; 2], b: [f64; 2]| [(a[0] + b[0]) / 2., (a[1] + b[1]) / 2.];
        let top = between(a, b);
        let bottom = between(d, c);
        [a, top, b, between(a, d), between(top, bottom), between(b, c), d, bottom, c]
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
        if self.geometry_pending {
            return Err("Preparing transform geometry".into());
        }
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
        if self.draft.is_some() {
            return self.command("", self.snap).map(|command| command.to_string());
        }
        self.commands()
            .map(|commands| serde_json::to_string(&commands).unwrap())
    }
    fn point(&self, p: [f64; 2]) -> [f64; 2] {
        let Ok(c) = self.command("", self.snap) else {
            return p;
        };
        if self.values.warp.is_some() {
            return crate::warp::CommandMap::new(&c, self.pivot)
                .and_then(|map| map.point(p))
                .unwrap_or(p);
        }
        if self.values.projective.is_some() {
            return crate::projective::combined(&c)
                .and_then(|map| map.ok_or_else(|| "Missing projective map".into()))
                .and_then(|map| map.point(p))
                .unwrap_or(p);
        }
        Self::matrix(&c).map_or(p, |[a, b, c, d, tx, ty]| {
            [a * p[0] + c * p[1] + tx, b * p[0] + d * p[1] + ty]
        })
    }
    fn corners(&self) -> [[f64; 2]; 4] {
        if let Some(draft) = &self.draft {
            return draft.corners.map(|point| self.point(point));
        }
        if self.values.warp.is_some() {
            if self.projective_mode.is_some() {
                // P controls are the A(quad) reference plane, not warped subject corners.
                if let Ok((quad, map)) = self.quad().and_then(|quad| {
                    self.affine_command("", self.snap)
                        .and_then(|c| crate::projective::affine(&c, self.pivot))
                        .map(|map| (quad, map))
                }) {
                    let mut points = quad.corners;
                    if points.iter_mut().all(|p| match map.point(*p) {
                        Ok(point) => {
                            *p = point;
                            true
                        }
                        Err(_) => false,
                    }) {
                        return points;
                    }
                }
            } else {
                return Self::rectangle(self.target_bounds()).map(|p| self.point(p));
            }
        }
        if let Some(quad) = &self.values.projective {
            if quad.rect == self.target_bounds() {
                if let Ok(map) = self
                    .affine_command("", self.snap)
                    .and_then(|command| crate::projective::affine(&command, self.pivot))
                {
                    let mut corners = quad.corners;
                    if corners.iter_mut().all(|p| match map.point(*p) {
                        Ok(point) => {
                            *p = point;
                            true
                        }
                        Err(_) => false,
                    }) {
                        return corners;
                    }
                }
            }
        }
        let [l, t, r, b] = if self.selected && !self.whole {
            self.bounds
        } else {
            self.whole_bounds
        };
        [[l, t], [r, t], [r, b], [l, b]].map(|p| self.point(p))
    }
    fn projective_handles(&self) -> Result<[[f64; 2]; 4], String> {
        let map = crate::projective::affine(&self.affine_command("", self.snap)?, self.pivot)?;
        let mut points = self.quad()?.corners;
        for point in &mut points {
            *point = map.point(*point)?;
        }
        Ok(points)
    }

    fn changed(&mut self) {
        self.error = None;
        self.apply_error = None;
        self.rendered = None;
    }
    fn ready_to_apply(&self, pending_preview: bool, busy: bool, context_frame: bool) -> bool {
        self.key().ok().as_ref().is_some_and(|key| self.rendered.as_ref() == Some(key))
            && self.error.is_none()
            && self.context.is_none()
            && !context_frame
            && !pending_preview
            && self.drag.is_none()
            && !busy
    }
    fn drag_corner(
        &mut self,
        index: usize,
        origin: [f64; 2],
        p: [f64; 2],
        perspective: bool,
        shift: bool,
    ) -> Result<(), String> {
        if p == origin {
            return Ok(());
        }
        let mut quad = self.quad()?;
        let command = self.affine_command("", self.snap)?;
        let inverse = crate::projective::affine(&command, self.pivot)?.inverse()?;
        let a = inverse.point(origin)?;
        let b = inverse.point(if self.snap { p.map(f64::round) } else { p })?;
        let mut delta = [b[0] - a[0], b[1] - a[1]];
        if perspective {
            let other = if shift { 3 - index } else { index ^ 1 };
            let edge = [
                quad.corners[index][0] - quad.corners[other][0],
                quad.corners[index][1] - quad.corners[other][1],
            ];
            let squared = edge[0] * edge[0] + edge[1] * edge[1];
            if squared <= 1e-12 {
                return Err("Perspective edge has no extent".into());
            }
            let amount = (delta[0] * edge[0] + delta[1] * edge[1]) / squared;
            delta = edge.map(|v| v * amount);
            for axis in 0..2 {
                quad.corners[other][axis] -= delta[axis];
            }
        } else if shift {
            delta[if delta[0].abs() >= delta[1].abs() {
                1
            } else {
                0
            }] = 0.;
        }
        for axis in 0..2 {
            quad.corners[index][axis] += delta[axis];
        }
        self.set_quad(quad);
        Ok(())
    }
    fn drag_projective_edge(&mut self, index: usize, origin: [f64; 2], p: [f64; 2]) -> Result<(), String> {
        if p == origin {
            return Ok(());
        }
        let inverse = crate::projective::affine(&self.affine_command("", self.snap)?, self.pivot)?.inverse()?;
        let a = inverse.point(origin)?;
        let b = inverse.point(if self.snap { p.map(f64::round) } else { p })?;
        let mut quad = self.quad()?;
        for corner in [index, (index + 1) % 4] {
            for axis in 0..2 {
                quad.corners[corner][axis] += b[axis] - a[axis];
            }
        }
        self.set_quad(quad);
        Ok(())
    }
    fn projective_controls(&mut self, ui: &mut egui::Ui) {
        if self.projective_mode.is_none() {
            return;
        }
        let state = self.quad().and_then(|quad| {
            let command = self.affine_command("", self.snap)?;
            let map = crate::projective::affine(&command, self.pivot)?;
            Ok((quad, map, map.inverse()?))
        });
        let (mut quad, map, inverse) = match state {
            Ok(state) => state,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        ui.label("Corner positions: document X / Y");
        let mut changed = false;
        for (index, label) in ["Top left", "Top right", "Bottom right", "Bottom left"]
            .into_iter()
            .enumerate()
        {
            let mut point = match map.point(quad.corners[index]) {
                Ok(point) => point,
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            };
            let edited = ui
                .horizontal(|ui| {
                    ui.add_sized([82., 20.], egui::Label::new(label));
                    let mut edited = false;
                    for (axis, value) in point.iter_mut().enumerate() {
                        let direction = ["X", "Y"][axis];
                        let response = controls::numeric(ui, value, -100000. ..=100000.)
                            .on_hover_text(format!("{label} {direction} in document pixels"));
                        response.widget_info(|| egui::WidgetInfo {
                            label: Some(format!("{label} {direction}")),
                            ..egui::WidgetInfo::drag_value(response.enabled(), *value)
                        });
                        #[cfg(test)]
                        {
                            self.projective_inputs[index * 2 + axis] = Some(response.rect);
                        }
                        edited |= response.changed();
                    }
                    edited
                })
                .inner;
            if edited {
                match inverse.point(point) {
                    Ok(point) => {
                        quad.corners[index] = point;
                        changed = true;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        if changed {
            self.set_quad(quad);
        }
    }
    fn drag_node(
        &mut self,
        index: usize,
        origin: [f64; 2],
        point: [f64; 2],
        shift: bool,
    ) -> Result<(), String> {
        if point == origin {
            return Ok(());
        }
        let map = self.post_map()?;
        let mut grid = self.grid()?;
        let anchor = map.point(grid.points[index])?;
        // Apply the pointer displacement at the node in displayed coordinates.
        // A projective inverse does not preserve displacement away from it.
        let delta = [point[0] - origin[0], point[1] - origin[1]];
        let mut target = [anchor[0] + delta[0], anchor[1] + delta[1]];
        if self.snap {
            target = target.map(f64::round);
        }
        if shift {
            let axis = usize::from(delta[0].abs() >= delta[1].abs());
            target[axis] = anchor[axis];
        }
        grid.points[index] = map.inverse()?.point(target)?;
        self.set_grid(grid);
        self.warp_preset = None;
        Ok(())
    }
    fn warp_controls(&mut self, ui: &mut egui::Ui) {
        if !self.warp_mode {
            return;
        }
        let mut preset_changed = false;
        ui.horizontal(|ui| {
            for (name, label) in [("arc", "Arc"), ("bulge", "Bulge"), ("flag", "Flag")] {
                if warp_preset_button(ui, name, label, self.warp_preset == Some(name), self.warp_amount)
                    .clicked()
                {
                    self.warp_preset = Some(name);
                    preset_changed = true;
                }
            }
        });
        ui.horizontal(|ui| {
            let amount = ui.add_sized([82., 20.], egui::Label::new("Amount"));
            let response = ui
                .add_enabled_ui(self.warp_preset.is_some(), |ui| {
                    controls::numeric(ui, &mut self.warp_amount, -0.25..=0.25)
                })
                .inner.labelled_by(amount.id)
                .on_hover_text("Warp amount relative to the grid. A negative value reverses the preset.");
            #[cfg(test)]
            {
                self.warp_amount_input = Some(response.rect);
            }
            preset_changed |= response.changed();
        });
        if preset_changed {
            if let Some(name) = self.warp_preset {
                let rect = self
                    .values
                    .warp
                    .as_ref()
                    .map(|grid| Ok(grid.rect))
                    .unwrap_or_else(|| self.warp_domain());
                match rect.and_then(|rect| crate::warp::Grid::preset(rect, name, self.warp_amount))
                {
                    Ok(grid) => self.set_grid(grid),
                    Err(error) => self.error = Some(error),
                }
            }
        }
        if ui.button("Reset Warp")
            .on_hover_text("Fit a fresh grid to this target and keep the other transform settings")
            .clicked()
        {
            self.values.warp = None;
            self.warp_preset = None;
        }
        let state = self.grid().and_then(|grid| {
            self.post_map()
                .and_then(|map| map.inverse().map(|inverse| (grid, map, inverse)))
        });
        let (mut grid, map, inverse) = match state {
            Ok(state) => state,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        if self.warp_domain().is_ok_and(|b| {
            b[0] < grid.rect[0] || b[1] < grid.rect[1] || b[2] > grid.rect[2] || b[3] > grid.rect[3]
        }) {
            ui.label("This grid may not cover Whole target. Return to selection or Reset Warp.");
        }
        ui.label("Node positions: document X / Y");
        let mut changed = false;
        for (index, point) in grid.points.iter_mut().enumerate() {
            let mut displayed = match map.point(*point) {
                Ok(point) => point,
                Err(error) => {
                    self.error = Some(error);
                    return;
                }
            };
            let label = format!("Node {}", index + 1);
            let edited = ui
                .horizontal(|ui| {
                    ui.add_sized([82., 20.], egui::Label::new(&label));
                    let mut edited = false;
                    for (axis, value) in displayed.iter_mut().enumerate() {
                        let direction = ["X", "Y"][axis];
                        let response = controls::numeric(ui, value, -100000. ..=100000.)
                            .on_hover_text(format!("{label} {direction} in document pixels"));
                        response.widget_info(|| egui::WidgetInfo {
                            label: Some(format!("{label} {direction}")),
                            ..egui::WidgetInfo::drag_value(response.enabled(), *value)
                        });
                        #[cfg(test)]
                        {
                            self.warp_inputs[index * 2 + axis] = Some(response.rect);
                        }
                        edited |= response.changed();
                    }
                    edited
                })
                .inner;
            if edited {
                match inverse.point(displayed) {
                    Ok(p) => {
                        *point = p;
                        changed = true;
                    }
                    Err(error) => self.error = Some(error),
                }
            }
        }
        if changed {
            self.set_grid(grid);
            self.warp_preset = None;
        }
    }
    fn drag_to(&mut self, p: [f64; 2], shift: bool) {
        let Some(drag) = &self.drag else {
            return;
        };
        let start = drag.start.clone();
        let origin = drag.origin;
        let handle = drag.handle;
        let warp_preset = drag.warp_preset;
        self.values = start.clone();
        match handle {
            Handle::Warp(index) => {
                self.warp_preset = warp_preset;
                if let Err(error) = self.drag_node(index, origin, p, shift) {
                    self.error = Some(error);
                }
                return;
            }
            Handle::Projective(index, perspective) => {
                if let Err(error) = self.drag_corner(index, origin, p, perspective, shift) {
                    self.error = Some(error);
                }
                return;
            }
            Handle::DistortEdge(index) => {
                if let Err(error) = self.drag_projective_edge(index, origin, p) {
                    self.error = Some(error);
                }
                return;
            }
            Handle::PerspectiveShortcut(index) => {
                if let Err(error) = self.drag_corner(index, origin, p, true, false) {
                    self.error = Some(error);
                }
                return;
            }
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
                if (self.proportional && x && y) ^ shift {
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
            Handle::Skew(axis) | Handle::SkewShortcut(axis) => {
                let snap_with_shift = shift && matches!(handle, Handle::Skew(_));
                if p == origin && !snap_with_shift {
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
            let snap = self.snap
                || (shift && !matches!(handle, Handle::Scale(..) | Handle::SkewShortcut(_)));
            if let Ok(c) = crate::transform::resolve_command(&self.raw("", snap)) {
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
                    if matches!(handle, Handle::Skew(_) | Handle::SkewShortcut(_)) {
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
        if editor.pending {
            return true;
        }
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
    fn free_transform_stale<'a>(&self, doc: impl Into<DocumentView<'a>>) -> bool {
        let doc = doc.into();
        self.free_transform.as_ref().is_some_and(|editor| {
            !editor.observes(doc)
                || match &editor.draft {
                    Some(draft) => !self.source_editor.as_ref().is_some_and(|source| {
                        source.token() == draft.token
                    }),
                    None => editor.roots != crate::tree::roots_view(doc, &self.selection_layers)
                        || editor.mask != self.mask
                        || editor.step != self.mask_step,
                }
        })
    }
    pub(super) fn open_free_transform<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        self.open_free_transform_from(ctx, doc.into(), false);
    }
    pub(super) fn open_free_transform_from<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: DocumentView<'a>,
        text_toolbar: bool,
    ) {
        if self.free_transform.is_some() {
            return;
        }
        let mut draft = self.source_editor.as_ref().and_then(|source| source.transform_draft(doc));
        let canvas_text_focus = draft.is_some() && self.source_editor.as_ref().is_some_and(|source| {
            source.has_canvas_focus(ctx) || source.has_field_handle_focus(ctx)
        });
        if self.interaction_active(self.source_editor.is_some() && draft.is_none())
            || doc.read_only()
            || self.lifecycle_frame
            || self.pen.in_gesture()
            || self.drag_start.is_some()
            || self.size_drag.is_some()
            || self.shortcuts.open
            || (ctx.wants_keyboard_input() && !canvas_text_focus && !text_toolbar)
            || ctx.input(|i| i.pointer.any_down())
        {
            self.message =
                "Finish or cancel the current tool or text edit before Free Transform.".into();
            return;
        }
        let roots = if draft.is_some() {
            Vec::new()
        } else {
            crate::tree::roots_view(doc, &self.selection_layers)
        };
        if draft.is_none() && roots.is_empty() {
            self.message = "Select a layer or folder to transform.".into();
            return;
        }
        let tree = crate::transform::tree_ids_view(doc, &roots);
        if doc
            .layers()
            .any(|layer| tree.iter().any(|id| id == layer.id()) && layer.locked())
        {
            self.message = "Unlock the selected content before Free Transform.".into();
            return;
        }
        let prior_tool = self.tool;
        if let Some(draft) = &mut draft {
            draft.resume_focus = canvas_text_focus || text_toolbar;
        }
        if canvas_text_focus || text_toolbar {
            if let Some(focus) = ctx.memory(|memory| memory.focused()) {
                ctx.memory_mut(|memory| memory.surrender_focus(focus));
            }
        }
        let whole_pixel_copy = self.transform_entire_pixel_copy(doc);
        self.select_tool(prior_tool);
        self.close_proposal();
        self.isolate = false;
        let (geometry, preparation_error) = if draft.is_some() {
            (None, None)
        } else {
            match self.transform_geometry(ctx, doc, &roots) {
                Ok(geometry) => (geometry, None),
                Err(error) => (None, Some(error)),
            }
        };
        let geometry_pending = draft.is_none() && geometry.is_none();
        let whole_bounds = draft.as_ref().map_or_else(
            || geometry.as_ref().map_or([0, 0, doc.width() as i32, doc.height() as i32], |g| g.bounds).map(f64::from),
            |draft| draft.bounds,
        );
        let selection = doc.selection().filter(|_| draft.is_none());
        let whole = selection.is_none() || whole_pixel_copy
            || (prior_tool == Tool::Text && roots.len() == 1 && !self.mask
                && doc.find_layer(&roots[0]).is_some_and(|layer| layer.source_kind() == Some("text")));
        let bounds = selection.map_or(whole_bounds, |bounds| bounds.map(f64::from));
        let b = if whole { whole_bounds } else { bounds };
        let mode = if draft.is_some() { Tool::Scale } else { Tool::Move };
        let warp_domains = geometry.map_or_else(
            || {
                [
                    Err("Preparing transform geometry".into()),
                    Err("Preparing transform geometry".into()),
                ]
            },
            |g| g.warp_domains,
        );
        self.free_transform = Some(Editor {
            document: doc.id().to_owned(),
            revision: doc.revision(),
            native_source: doc.native().map(|native| native.key()),
            draft,
            roots,
            mask: self.mask,
            step: self.mask_step.clone(),
            selected: selection.is_some(),
            whole,
            bounds,
            whole_bounds,
            pivot: [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5],
            reference: None,
            values: Parameters::default(),
            mode,
            skew_mode: false,
            projective_mode: None,
            warp_mode: false,
            warp_domains,
            warp_preset: None,
            warp_amount: 0.15,
            context: Default::default(),
            context_frame: false,
            context_secondary_typing: false,
            text_focus: None,
            proportional: true,
            snap: false,
            prior_tool,
            gesture: crate::engine::id(),
            pending: false,
            canvas_apply: false,
            geometry_pending,
            drag: None,
            error: preparation_error,
            apply_error: None,
            rendered: None,
            #[cfg(test)]
            inputs: [None; 9],
            #[cfg(test)]
            projective_inputs: [None; 8],
            #[cfg(test)]
            warp_inputs: [None; 18],
            #[cfg(test)]
            warp_amount_input: None,
            #[cfg(test)]
            scroll_rect: None,
            #[cfg(test)]
            scroll_offset: Vec2::ZERO,
        });
        self.free_transform_frame = true;
        self.last_preview = None;
        ctx.request_repaint();
    }
    fn transform_geometry(
        &mut self,
        ctx: &egui::Context,
        doc: DocumentView<'_>,
        roots: &[String],
    ) -> Result<Option<transform_bounds::Geometry>, String> {
        match doc {
            DocumentView::Native(native) => self.transform_bounds.get(
                &self.shared,
                &self.project_id,
                native,
                roots,
                self.mask,
                self.mask_step.as_deref(),
                ctx,
            ),
            DocumentView::Resident(resident) => {
                let bounds = crate::transform::tree_bounds(resident, roots).unwrap_or([
                    0,
                    0,
                    doc.width() as i32,
                    doc.height() as i32,
                ]);
                let warp_domains = [false, true].map(|whole| {
                    let commands = roots.iter().map(|id| json!({"op":"transform","layer":id,"mask":self.mask,
                        "step":if roots.len()==1 {self.mask_step.clone()}else{None}, "selection_only":doc.selection().is_some()&&!whole})).collect::<Vec<_>>();
                    crate::warp::domain(doc, &commands, &|| Ok(()))
                });
                Ok(Some(transform_bounds::Geometry {
                    bounds,
                    warp_domains,
                }))
            }
        }
    }
    fn preparing_free_transform(&self) -> bool {
        self.free_transform
            .as_ref()
            .is_some_and(|editor| editor.geometry_pending)
    }
    pub(super) fn free_transform_pending(&self) -> bool {
        self.free_transform
            .as_ref()
            .is_some_and(|editor| editor.pending)
    }
    fn apply_free_transform(&mut self) {
        if self.preparing_free_transform() {
            return;
        }
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; wait for its result".into();
            return;
        }
        let Some(editor) = self.free_transform.as_mut() else {
            return;
        };
        if editor.draft.is_some() {
            self.finish_free_transform(true);
            self.message = "Free Transform applied to the text draft.".into();
            return;
        }
        let commands = match editor.commands() {
            Ok(commands) => commands,
            Err(error) => {
                editor.error = Some(error.clone());
                self.message = error.into();
                return;
            }
        };
        if commands.is_empty() {
            self.cancel_free_transform();
            self.message = "Free Transform unchanged.".into();
            return;
        }
        let document = editor.document.clone();
        let revision = editor.revision;
        let source = editor.native_source;
        let token = egui::Id::new(&editor.gesture);
        editor.apply_error = None;
        editor.pending = true;
        self.apply_action(
            commands,
            "Free Transform",
            Some((&document, revision)),
            edit_job::Origin::FreeTransform(token, source),
        );
    }
    pub(super) fn free_transform_finished(&mut self, token: egui::Id, result: Result<(), String>) {
        let Some(editor) = self
            .free_transform
            .as_mut()
            .filter(|editor| editor.pending && egui::Id::new(&editor.gesture) == token)
        else {
            return;
        };
        editor.pending = false;
        match result {
            Ok(()) => self.cancel_free_transform(),
            Err(error) => editor.apply_error = Some(error),
        }
    }
    pub(super) fn cancel_free_transform(&mut self) {
        self.finish_free_transform(false);
    }
    fn finish_free_transform(&mut self, keep_text_transform: bool) {
        if self.free_transform_pending() {
            self.cancel_pending_edit();
            return;
        }
        if let Some(editor) = self.free_transform.take() {
            if let Some(draft) = editor.draft {
                if let Some(source) = self.source_editor.as_mut().filter(|source| source.token() == draft.token) {
                    if !keep_text_transform {
                        source.restore_transform(&draft);
                    }
                    if draft.resume_focus {
                        source.resume_canvas_focus();
                    }
                }
            }
            self.transform_bounds.clear();
            self.select_tool(editor.prior_tool);
            self.free_transform_frame = true;
            self.free_transform_cancelled_pointer = self.edit_context.input(|input| input.pointer.primary_down());
            self.transient.clear();
            self.native_source_preview = None;
            self.canvas_selection = None;
            self.last_preview = None;
            self.edit_context.request_repaint();
        }
    }
    pub(super) fn sync_free_transform<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        if self.free_transform_pending() {
            if self.free_transform.as_ref().is_some_and(|editor| !editor.observes(doc)) {
                self.transient.clear();
            }
            return;
        }
        let lost = ctx.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|e| matches!(e, egui::Event::WindowFocused(false)))
        });
        let other = self.lifecycle.is_some()
            || self.lifecycle_frame
            || self.geometry.is_some()
            || (self.source_editor.is_some()
                && !self.free_transform.as_ref().is_some_and(Editor::edits_text_draft))
            || self.filter_editor.is_some()
            || self.refinement.is_some()
            || self.show_new
            || self.busy
            || self.shortcuts.open
            || !self.jobs.is_empty()
            || self.load_control.is_some()
            || self.pending_import.is_some()
            || self.pending_image_open.is_some();
        let cancelled_contact = self.free_transform.as_ref().is_some_and(|editor| editor.drag.is_some())
            && Self::pointer_contact_cancelled(ctx);
        if self.free_transform.is_some() && (lost || other || cancelled_contact || self.free_transform_stale(doc)) {
            self.cancel_free_transform();
            self.message = if cancelled_contact {
                "Free Transform cancelled after contact ended · current work preserved"
            } else {
                "Free Transform cancelled after focus, target or source changed; current work is preserved."
            }.into();
        }
        if self.preparing_free_transform() {
            let roots = self.free_transform.as_ref().unwrap().roots.clone();
            let result = self.transform_geometry(ctx, doc, &roots);
            let editor = self.free_transform.as_mut().unwrap();
            match result {
                Ok(Some(geometry)) => {
                    editor.whole_bounds = geometry.bounds.map(f64::from);
                    editor.bounds = doc
                        .selection()
                        .map(|b| b.map(f64::from))
                        .unwrap_or(editor.whole_bounds);
                    let b = editor.target_bounds();
                    editor.pivot = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5];
                    editor.warp_domains = geometry.warp_domains;
                    editor.geometry_pending = false;
                    editor.error = None;
                    self.last_preview = None;
                }
                Ok(None) => {}
                Err(error) => editor.error = Some(error),
            }
        }
    }
    pub(super) fn free_transform_keys<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        self.sync_free_transform(ctx, doc);
        let mut fresh_escape = false;
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
                            let fresh = self.selection_shortcut_keys.insert(key);
                            if key == egui::Key::Escape {
                                fresh_escape |= fresh;
                            }
                        } else {
                            self.selection_shortcut_keys.remove(&key);
                        }
                    }
                }
            }
        });
        if self.free_transform_pending() {
            return;
        }
        if self
            .free_transform
            .as_ref()
            .is_some_and(|editor| editor.context.is_some())
        {
            self.free_transform.as_mut().unwrap().context_frame = true;
            if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                *self.free_transform.as_mut().unwrap().context = None;
                ctx.request_repaint();
            }
            return;
        }
        // egui clears ordinary TextEdit focus before draw when Escape arrives.
        // Keep that field's Escape out of the session and behind-field controls.
        if ctx.input(|input| input.key_pressed(egui::Key::Escape))
            && self
                .free_transform
                .as_ref()
                .and_then(|editor| editor.text_focus)
                .is_some_and(|id| ctx.memory(|memory| memory.had_focus_last_frame(id)))
        {
            self.free_transform.as_mut().unwrap().context_frame = true;
            return;
        }
        if ctx.wants_keyboard_input()
            || !Self::free_transform_keyboard(ctx)
            || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))))
        {
            return;
        }
        if fresh_escape && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_free_transform();
            self.message = "Free Transform cancelled.".into();
            return;
        }
        if self.preparing_free_transform() {
            return;
        }
        if let Some(editor) = &mut self.free_transform {
            for (action, tool) in [
                ("Move", Tool::Move),
                ("Rotate artwork", Tool::Rotate),
                ("Scale artwork", Tool::Scale),
            ] {
                if ctx.input(|input| self.shortcuts.pressed_once(action, input)) {
                    editor.set_mode(tool);
                }
            }
        }
    }
    pub(super) fn free_transform_canvas<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        rect: Rect,
        scale: f32,
        available: Rect,
    ) {
        let doc = doc.into();
        self.sync_free_transform(ui.ctx(), doc);
        if Self::pointer_contact_cancelled(ui.ctx()) {
            return;
        }
        if self.preparing_free_transform() {
            return;
        }
        if self.free_transform_pending() {
            return;
        }
        let Some(mut editor) = self.free_transform.take() else {
            return;
        };
        let before = editor.values.clone();
        let context_frame = editor.show_context(ui, available);
        let old_reference = editor.reference_point();
        let view = self.view;
        let screen = |p: [f64; 2]| view.point(p.map(|v| v as f32), rect, scale);
        let corners = editor.corners().map(screen);
        let actions = if editor.edits_text_draft() && editor.drag.is_none() && !context_frame {
            source::text_actions_rect(available, Rect::from_points(&corners))
        } else {
            Rect::NOTHING
        };
        let edges = editor.edge_points().map(screen);
        let middle = edges[0];
        let center = screen(editor.reference_point());
        let rotation = rotation_handle(corners, middle);
        let points = [
            corners[0], corners[1], corners[2], corners[3], edges[0], edges[1], edges[2], edges[3],
        ];
        let (press, release, modifiers, navigating) = ui.input(|i| {
            let mut buttons = primary_button_samples(i);
            let press = if editor.drag.is_none() {
                buttons.find(|sample| sample.0).map(|(_, point, modifiers)| (point, modifiers))
            } else {
                None
            };
            let release = buttons.find(|sample| !sample.0).map(|(_, point, modifiers)| (point, modifiers));
            let modifiers = press.map_or(i.modifiers, |(_, modifiers)| modifiers);
            let navigating = context_frame
                || (modifiers.alt && !transform_modifier(modifiers) && editor.drag.is_none())
                || i.key_down(egui::Key::Space)
                || i.pointer.middle_down()
                || i.pointer.secondary_down();
            (press, release, modifiers, navigating)
        });
        let modifier_active = transform_modifier(modifiers) && !editor.warp_mode;
        let skew_shortcut = modifier_active && modifiers.shift && !modifiers.alt;
        let projective_shortcut = modifier_active
            && !editor.edits_text_draft()
            && modifiers.alt == modifiers.shift;
        let shortcut_active = skew_shortcut || projective_shortcut;
        let projective_points = |editor: &Editor| {
            let [a, b, c, d] = editor.projective_handles().ok()?.map(screen);
            Some([
                a, b, c, d,
                a.lerp(b, 0.5), b.lerp(c, 0.5), c.lerp(d, 0.5), d.lerp(a, 0.5),
            ])
        };
        let projective = projective_shortcut.then(|| projective_points(&editor)).flatten();
        if navigating {
            editor.drag = None;
        }
        let grid_points = (editor.warp_mode || editor.values.warp.is_some())
            .then(|| editor.grid_points())
            .transpose()
            .ok()
            .flatten()
            .map(|points| points.map(screen));
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hit = press.map(|(point, _)| point).or(pointer)
            .filter(|p| {
                !navigating
                    && available.contains(*p)
                    && !actions.contains(*p)
                    && ui.ctx().layer_id_at(*p) == Some(ui.layer_id())
            })
            .and_then(|p| {
                // Center node wins over an overlapping reference; reference fields remain available.
                if editor.warp_mode {
                    if let Some(index) = grid_points
                        .as_ref()
                        .and_then(|points| Editor::nearest_warp_node(points, p))
                    {
                        return Some(Handle::Warp(index));
                    }
                }
                if shortcut_active {
                    if let Some((_, handle)) = (0..8)
                        .filter_map(|index| {
                            let handle = modifier_handle(index, modifiers, editor.edits_text_draft())?;
                            let point = if matches!(handle, Handle::SkewShortcut(_)) {
                                points[index]
                            } else {
                                projective?[index]
                            };
                            let distance = p.distance_sq(point);
                            (distance < 9. * 9.).then_some((distance, handle))
                        })
                        .min_by(|(a, _), (b, _)| a.total_cmp(b))
                    {
                        return Some(handle);
                    }
                }
                if p.distance(center) < 9. {
                    return Some(Handle::Pivot);
                }
                if p.distance(rotation) < 10. {
                    return Some(Handle::Rotate);
                }
                if let Some(mode) = editor.projective_mode.filter(|_| !shortcut_active) {
                    if let Some(index) = corners.iter().position(|point| p.distance(*point) < 9.) {
                        return Some(Handle::Projective(
                            index,
                            mode == ProjectiveMode::Perspective,
                        ));
                    }
                }
                if editor.skew_mode && !shortcut_active {
                    if let Some(index) = points[4..].iter().position(|point| p.distance(*point) < 9.) {
                        return Some(Handle::Skew(index % 2));
                    }
                }
                if editor.scale_handles() && !shortcut_active {
                    if let Some((index, _)) = points
                        .iter()
                        .enumerate()
                        .map(|(index, point)| (index, p.distance_sq(*point)))
                        .filter(|&(_, distance)| distance < 9. * 9.)
                        .min_by(|(_, a), (_, b)| a.total_cmp(b))
                    {
                        return Some(Handle::Scale(
                            index < 4 || index % 2 == 1, index < 4 || index % 2 == 0,
                        ));
                    }
                }
                if !shortcut_active
                    && editor.mode() == Some(Tool::Rotate)
                    && points.iter().any(|point| p.distance(*point) < 10.)
                {
                    return Some(Handle::Rotate);
                }
                let [x, y] = view.document(p, rect, scale);
                let inside = if editor.values.warp.is_some() && editor.projective_mode.is_none() {
                    let outline: Vec<_> = editor.outline().into_iter()
                        .map(|point| point.map(|v| v as f32)).collect();
                    crate::selection::contains(&outline, x, y)
                } else {
                    let polygon = corners.map(|point| [point.x, point.y]);
                    crate::selection::contains(&polygon, p.x, p.y)
                };
                if inside {
                    Some(Handle::Move)
                } else if !shortcut_active && editor.mode().is_some()
                    && editor.values.warp.is_none() && near_transform_edge(p, corners)
                {
                    Some(Handle::Rotate)
                } else {
                    None
                }
            });
        if let (Some(handle), Some((p, _))) = (hit, press) {
            editor.drag = Some(Drag {
                origin: view.document(p, rect, scale).map(f64::from),
                start: editor.values.clone(),
                handle,
                warp_preset: editor.warp_preset,
            });
        }
        let sample = release.or_else(|| pointer.map(|pos| (pos, ui.input(|i| i.modifiers))));
        if let Some((p, modifiers)) = sample.filter(|_| editor.drag.is_some()) {
            editor.drag_to(
                view.document(p, rect, scale).map(f64::from),
                modifiers.shift,
            );
        }
        if release.is_some() {
            editor.drag = None;
        }
        if editor.drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        } else if hit.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
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
        let edges = editor.edge_points().map(screen);
        let middle = edges[0];
        let center = screen(editor.reference_point());
        let rotation = rotation_handle(corners, middle);
        painter.add(egui::Shape::closed_line(
            editor.outline().into_iter().map(screen).collect(),
            Stroke::new(1.25, ACCENT),
        ));
        let mut handles = if projective_shortcut {
            projective_points(&editor).map_or_else(Vec::new, |points| {
                points.into_iter().enumerate()
                    .filter_map(|(index, point)| modifier_handle(index, modifiers, false).map(|_| point))
                    .collect()
            })
        } else if editor.skew_mode || editor.warp_mode || skew_shortcut {
            vec![]
        } else {
            corners.to_vec()
        };
        let show_skew_handles = skew_shortcut || (!shortcut_active && editor.skew_mode);
        if show_skew_handles || (!shortcut_active && editor.scale_handles()) {
            handles.extend(edges);
        }
        for (index, p) in handles.into_iter().enumerate() {
            painter.rect_filled(Rect::from_center_size(p, Vec2::splat(7.)), 1., ACCENT);
            if show_skew_handles {
                painter.text(
                    p + Vec2::new(8., -5.),
                    egui::Align2::LEFT_BOTTOM,
                    if index % 2 == 0 { "X" } else { "Y" },
                    egui::FontId::proportional(11.),
                    ACCENT,
                );
            }
        }
        if editor.warp_mode || editor.values.warp.is_some() {
            if let Ok(nodes) = editor.grid_points() {
                let nodes = nodes.map(screen);
                paint_warp_grid(&painter, nodes, Stroke::new(1., ACCENT));
                if editor.warp_mode {
                    for node in nodes {
                        painter.rect_filled(
                            Rect::from_center_size(node, Vec2::splat(7.)),
                            1.,
                            ACCENT,
                        );
                    }
                }
            }
        }
        painter.circle_stroke(rotation, 5., Stroke::new(1.5, ACCENT));
        painter.line_segment([middle, rotation], Stroke::new(1., ACCENT));
        painter.circle_stroke(center, 5., Stroke::new(1., Color32::WHITE));
        if let Some(draft) = &editor.draft {
            if let Err(error) = self.preview_text_transform(&editor, draft, doc) {
                editor.error = Some(error);
                editor.rendered = None;
            }
            let actions = if actions.is_positive() && before == editor.values {
                actions
            } else {
                Rect::NOTHING
            };
            let ready = editor.ready_to_apply(self.pending, self.busy, context_frame)
                && (doc.native().is_none() || !self.native_canvas_offscreen());
            let (apply, cancel) = source::text_canvas_actions(
                ui, &painter, actions, egui::Id::new((&editor.gesture, "text transform actions")), ready,
                [
                    ("Apply text transform on canvas", "Keep the transform and return to the text draft"),
                    ("Cancel text transform on canvas", "Restore the starting text field · Escape"),
                ],
            );
            editor.canvas_apply = apply;
            if cancel {
                self.free_transform = Some(editor);
                self.cancel_free_transform();
                return;
            }
        } else if let Ok(commands) = editor.commands() {
            self.transient = commands;
        }
        self.free_transform = Some(editor);
    }
    fn preview_text_transform(
        &mut self,
        editor: &Editor,
        draft: &source::TransformDraft,
        doc: DocumentView<'_>,
    ) -> Result<(), String> {
        self.transient.clear();
        self.native_source_preview = None;
        self.native_source_preview_at = 0;
        let source = self.source_editor.as_mut()
            .filter(|source| source.token() == draft.token)
            .ok_or("Text draft changed during Free Transform")?;
        let matrix = Editor::matrix(&editor.command("", editor.snap)?)?;
        source.project_transform(draft, matrix)?;
        match source.edit_command(doc)? {
            source::EditCommand::Resident(command) => {
                if let Some(error) = &source.error {
                    return Err(error.clone());
                }
                self.transient.push(command);
            }
            source::EditCommand::Native(_) => {
                self.native_source_preview = Some(source.owned_command().ok_or_else(|| {
                    source.error.clone().unwrap_or_else(|| "Text draft preview is unavailable".into())
                })?);
            }
        }
        Ok(())
    }
    pub(super) fn free_transform_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
        drawn_generation: u64,
    ) {
        let doc = doc.into();
        self.sync_free_transform(ctx, doc);
        if self.preparing_free_transform() {
            egui::Window::new("Free Transform")
                .id(egui::Id::new("free transform"))
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    if let Some(error) = self
                        .free_transform
                        .as_ref()
                        .and_then(|editor| editor.error.as_ref())
                    {
                        ui.label(error);
                        if ui.button("Retry").clicked() {
                            self.transform_bounds.clear();
                            self.free_transform.as_mut().unwrap().error = None;
                            ctx.request_repaint();
                        }
                    } else {
                        ui.spinner();
                        ui.label("Preparing transform geometry…");
                    }
                    if ui.button("Cancel").clicked() {
                        self.cancel_free_transform();
                    }
                });
            return;
        }
        if self.free_transform_pending() {
            egui::Window::new("Free Transform")
                .id(egui::Id::new("free transform"))
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("Applying Free Transform…");
                    if ui.button("Cancel apply").clicked() {
                        self.cancel_pending_edit();
                    }
                });
            return;
        }
        let Some(mut editor) = self.free_transform.take() else {
            return;
        };
        // Keep a dismissed input's primary press guarded through its release,
        // including the final Apply check after canvas and deferred shortcuts.
        let context_frame = editor.context_frame;
        editor.context_frame &= ctx.input(|input| input.pointer.primary_down());
        let typing = ctx.wants_keyboard_input()
            || ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Ime(_))));
        let before = editor.key();
        let canvas_apply = std::mem::take(&mut editor.canvas_apply);
        let mut open = true;
        let (mut apply, mut cancel) = (false, false);
        egui::Window::new("Free Transform")
            .id(egui::Id::new("free transform"))
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(330.)
            .default_height(ctx.screen_rect().height())
            .show(ctx, |ui| {
                let response = egui::ScrollArea::vertical()
                    .id_salt("free transform controls")
                    .auto_shrink([true, true])
                    .drag_to_scroll(false)
                    .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| editor.affine_modes(ui, false));
                if editor.edits_text_draft() {
                    ui.weak("Distort, Perspective, and Grid Warp require a rasterized layer.");
                } else {
                    ui.horizontal_wrapped(|ui| editor.nonlinear_modes(ui, false));
                }
                if editor.warp_mode {
                    ui.label("Drag a grid point or choose a preset. Shift constrains the drag to one axis.");
                } else if let Some(mode) = editor.projective_mode {
                    ui.label(if mode == ProjectiveMode::Distort {
                        "Drag a corner freely. Shift constrains its axis."
                    } else {
                        "Drag a corner pair along its edge. Shift uses the vertical pair."
                    });
                    ui.label("Release keeps the actual preview pending; Apply commits once.");
                } else if editor.skew_mode {
                    ui.label("Top/bottom: X · Left/right: Y · Shift: 15° snap.")
                        .on_hover_text("Drag an edge handle to shear the actual artwork. An edge on the reference axis stays fixed; drag the opposite edge or move the reference point. Release keeps the preview pending.");
                } else if editor.scale_handles() {
                    let (key, alt) = if cfg!(target_os = "macos") {
                        ("Cmd", "Option")
                    } else {
                        ("Ctrl", "Alt")
                    };
                    let mut help = format!("Shift toggles proportional scaling. {key}+Shift on a side handle skews the target.");
                    if !editor.edits_text_draft() {
                        help.push_str(&format!(" {key}-drag on a handle distorts, and {key}+{alt}+Shift on a corner adjusts perspective."));
                    }
                    ui.label("Drag inside to move, a square to scale, or just outside the outline to rotate.")
                        .on_hover_text(help);
                } else {
                    ui.label("Drag inside to move. Drag a handle or just outside the outline to rotate.");
                }
                #[cfg(test)]
                let mut input_index = 0;
                #[cfg(test)]
                let mut inputs = [None; 9];
                const ANGLE_LABEL: &str = "Angle";
                let number = |ui: &mut egui::Ui,
                              label: &str,
                              value: &mut f64,
                              range: std::ops::RangeInclusive<f64>| {
                    let response = ui
                        .horizontal(|ui| {
                            let name = ui.add_sized([82., 20.], egui::Label::new(label));
                            if label == ANGLE_LABEL {
                                controls::angle_with_limits(
                                    ui,
                                    ("free transform rotation", &editor.gesture),
                                    value,
                                    true,
                                    "Rotation angle",
                                    "Positive angles rotate clockwise around the transform reference point.",
                                    Some(range),
                                )
                            } else {
                                controls::numeric(ui, value, range).labelled_by(name.id)
                            }
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
                number(ui, ANGLE_LABEL, &mut editor.values.angle, -100000. ..=100000.);
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
                    icons::toggle(
                        ui,
                        Icon::Connect,
                        &mut editor.proportional,
                        "Keep horizontal and vertical scale proportional",
                    );
                    icons::toggle(ui, Icon::Snap, &mut editor.snap, "Snap transform")
                        .on_hover_text(format!(
                            "Snap movement to {} px, rotation and skew to {}°, and scale to {}% steps. Distort and Perspective handles and Grid Warp nodes use whole-pixel drag positions.",
                            crate::transform::MOVE_SNAP, crate::transform::ANGLE_SNAP,
                            crate::transform::SCALE_SNAP * 100.,
                        ));
                });
                ui.horizontal(|ui| {
                    icons::toggle(
                        ui,
                        Icon::MirrorHorizontal,
                        &mut editor.values.mirror[0],
                        "Flip horizontally",
                    );
                    icons::toggle(
                        ui,
                        Icon::MirrorVertical,
                        &mut editor.values.mirror[1],
                        "Flip vertically",
                    );
                });
                if editor.selected {
                    ui.horizontal_wrapped(|ui| {
                        for (whole, icon, label, help) in [
                            (false, Icon::Selection, "Selected pixels", "Transform the selected pixels and move the selection with them."),
                            (true, Icon::CanvasBounds, "Whole target", "Transform the full target and leave the selection in place."),
                        ] {
                            if icons::labeled_choice(ui, icon, label, Some(editor.whole == whole), 0.)
                                .on_hover_text(help).clicked()
                            {
                                editor.whole = whole;
                            }
                        }
                    });
                }
                let mut reference = editor.reference_point();
                let old_reference = reference;
                let anchors = editor.reference_anchors();
                let selected = anchors.iter().position(|point| {
                    (point[0] - reference[0]).abs() < 1e-6 && (point[1] - reference[1]).abs() < 1e-6
                });
                let reference_choice = ui.horizontal(|ui| {
                    ui.label("Reference point")
                        .on_hover_text("Choose a reference from the transform bounds or drag it on the canvas.");
                    controls::anchor_grid(ui, "transform reference", selected, "Reference: ")
                }).inner;
                number(ui, "Reference X", &mut reference[0], -100000. ..=100000.);
                number(ui, "Reference Y", &mut reference[1], -100000. ..=100000.);
                #[cfg(test)]
                {
                    editor.inputs = inputs;
                }
                if let Some(index) = reference_choice {
                    reference = anchors[index];
                }
                if reference != old_reference {
                    if let Err(error) = editor.set_reference(reference) {
                        editor.error = Some(error);
                    }
                    ctx.request_repaint();
                }
                if editor.draft.is_none() {
                    editor.projective_controls(ui);
                    editor.warp_controls(ui);
                }
                if ui.button("Reset transform").clicked() {
                    if editor.reference.is_some() {
                        editor.pivot = editor.reference_point();
                    }
                    editor.reference = None;
                    editor.values = Parameters::default();
                    editor.warp_preset = None;
                    editor.drag = None;
                    editor.changed();
                }
                if editor.key() != before {
                    editor.changed();
                }
                let commands = editor.commands();
                if let Some(error) = commands.as_ref().err().or(editor.error.as_ref()).or(editor.apply_error.as_ref()) {
                    ui.colored_label(ACCENT, error);
                }
                let offscreen = doc.native().is_some() && self.native_canvas_offscreen();
                let rendered = !offscreen && editor
                    .key()
                    .ok()
                    .is_some_and(|key| editor.rendered.as_ref() == Some(&key));
                if offscreen {
                    ui.label("Bring the canvas into view to preview and apply");
                } else if !rendered && editor.error.is_none() && commands.is_ok() {
                    ui.label("Waiting for the full-detail preview");
                }
                ui.horizontal(|ui| {
                    let ready = commands.is_ok() && rendered
                        && editor.ready_to_apply(self.pending, self.busy, context_frame)
                        && drawn_generation == self.canvas_generation;
                    apply = ui.add_enabled(ready, dialogs::primary("Apply")).clicked()
                        || (ready && canvas_apply)
                        || (ready
                            && !typing
                            && !ctx.wants_keyboard_input()
                            && Self::free_transform_keyboard(ctx)
                            && ctx.input(|i| i.key_pressed(egui::Key::Enter)));
                    cancel = ui.button("Cancel").clicked();
                });
                    });
                #[cfg(test)]
                {
                    editor.scroll_rect = Some(response.inner_rect);
                    editor.scroll_offset = response.state.offset;
                }
                #[cfg(not(test))]
                let _ = response;
            });
        editor.text_focus = ctx
            .memory(|memory| memory.focused())
            .filter(|id| egui::TextEdit::load_state(ctx, *id).is_some());
        if !open || cancel {
            self.free_transform = Some(editor);
            self.cancel_free_transform();
            self.message = "Free Transform cancelled.".into();
        } else if apply {
            self.free_transform = Some(editor);
            self.apply_free_transform();
        } else {
            if editor.draft.is_none() {
                self.transient = editor.commands().unwrap_or_default();
            }
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
