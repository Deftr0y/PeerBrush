//! One source-bound transform preview; the shared engine owns every pixel and guard.
use super::*;

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
#[derive(Clone, Copy)]
enum Handle {
    Move,
    Rotate,
    Scale(bool, bool),
    Skew(usize),
    Projective(usize, bool),
    Warp(usize),
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
    projective_mode: Option<ProjectiveMode>,
    warp_mode: bool,
    warp_domains: [Result<[f64; 4], String>; 2],
    warp_preset: Option<&'static str>,
    warp_amount: f64,
    context: egui::menu::MenuRootManager,
    context_frame: bool,
    proportional: bool,
    snap: bool,
    prior_tool: Tool,
    gesture: String,
    drag: Option<Drag>,
    pub error: Option<String>,
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
    pub(super) fn mode(&self) -> Option<Tool> {
        (!self.skew_mode && self.projective_mode.is_none() && !self.warp_mode).then_some(self.mode)
    }
    pub(super) fn set_mode(&mut self, mode: Tool) {
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
            && !ui.ctx().wants_keyboard_input()
            && !ui.input(|input| {
                input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::Ime(_)))
            })
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
            for (mode, label) in [
                (Tool::Move, "Move"),
                (Tool::Scale, "Scale"),
                (Tool::Rotate, "Rotate"),
            ] {
                if ui
                    .selectable_label(self.mode() == Some(mode), label)
                    .clicked()
                {
                    self.set_mode(mode);
                    ui.close_menu();
                }
            }
            if ui.selectable_label(self.skew_mode, "Skew").clicked() {
                self.skew_mode = true;
                self.projective_mode = None;
                self.warp_mode = false;
                self.drag = None;
                ui.close_menu();
            }
            for (mode, label) in [
                (ProjectiveMode::Distort, "Distort"),
                (ProjectiveMode::Perspective, "Perspective"),
            ] {
                if ui
                    .selectable_label(self.projective_mode == Some(mode), label)
                    .clicked()
                {
                    self.projective_mode = Some(mode);
                    self.skew_mode = false;
                    self.warp_mode = false;
                    self.drag = None;
                    ui.close_menu();
                }
            }
            if ui.selectable_label(self.warp_mode, "Grid Warp").clicked() {
                self.warp_mode = true;
                self.skew_mode = false;
                self.projective_mode = None;
                self.drag = None;
                ui.close_menu();
            }
            ui.separator();
            for (axis, label) in [(0, "Flip Horizontal"), (1, "Flip Vertical")] {
                if ui
                    .selectable_label(self.values.mirror[axis], label)
                    .clicked()
                {
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
    fn affine(&self) -> bool {
        self.values.scale != [1.; 2]
            || self.values.angle.rem_euclid(360.) != 0.
            || self.values.skew != [0.; 2]
            || self.values.mirror != [false; 2]
            || self.values.projective.is_some()
            || self.values.warp.is_some()
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
    fn changed(&mut self) {
        self.error = None;
        self.rendered = None;
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
                        let response = controls::numeric(ui, value, -100000. ..=100000.);
                        #[cfg(test)]
                        {
                            self.projective_inputs[index * 2 + axis] = Some(response.rect);
                        }
                        #[cfg(not(test))]
                        let _ = axis;
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
        mut point: [f64; 2],
        shift: bool,
    ) -> Result<(), String> {
        if point == origin {
            return Ok(());
        }
        if self.snap {
            point = point.map(f64::round);
        }
        if shift {
            let axis = usize::from((point[0] - origin[0]).abs() >= (point[1] - origin[1]).abs());
            point[axis] = origin[axis];
        }
        let inverse = self.post_map()?.inverse()?;
        let a = inverse.point(origin)?;
        let b = inverse.point(point)?;
        let mut grid = self.grid()?;
        for axis in 0..2 {
            grid.points[index][axis] += b[axis] - a[axis];
        }
        self.set_grid(grid);
        self.warp_preset = None;
        Ok(())
    }
    fn warp_controls(&mut self, ui: &mut egui::Ui) {
        if !self.warp_mode {
            return;
        }
        ui.label("3 by 3 grid with straight segments between points.");
        ui.label("Drag nodes; Shift constrains document X/Y. Presets use this grid.");
        let mut preset_changed = false;
        ui.horizontal(|ui| {
            for (name, label) in [("arc", "Arc"), ("bulge", "Bulge"), ("flag", "Flag")] {
                if ui
                    .selectable_label(self.warp_preset == Some(name), label)
                    .clicked()
                {
                    self.warp_preset = Some(name);
                    preset_changed = true;
                }
            }
            if self.warp_preset.is_none() {
                ui.label("Custom");
            }
        });
        ui.horizontal(|ui| {
            ui.add_sized([82., 20.], egui::Label::new("Amount"));
            let response = ui
                .add_enabled_ui(self.warp_preset.is_some(), |ui| {
                    controls::numeric(ui, &mut self.warp_amount, -0.25..=0.25)
                })
                .inner;
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
        if ui.button("Reset Warp").clicked() {
            self.values.warp = None;
            self.warp_preset = None;
        }
        ui.label(
            "Reset Warp fits a fresh grid to this target and keeps the other transform settings.",
        );
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
            let edited = ui
                .horizontal(|ui| {
                    ui.add_sized([82., 20.], egui::Label::new(format!("Node {}", index + 1)));
                    let mut edited = false;
                    for (axis, value) in displayed.iter_mut().enumerate() {
                        let response = controls::numeric(ui, value, -100000. ..=100000.);
                        #[cfg(test)]
                        {
                            self.warp_inputs[index * 2 + axis] = Some(response.rect);
                        }
                        #[cfg(not(test))]
                        let _ = axis;
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
        self.values = start.clone();
        match handle {
            Handle::Warp(index) => {
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
        let warp_domains = [false, true].map(|whole| {
            let commands = roots
                .iter()
                .map(|id| {
                    json!({"op":"transform","layer":id,
                "mask":self.mask,"step":if roots.len()==1 {self.mask_step.clone()}else{None},
                "selection_only":doc.selection.is_some()&&!whole})
                })
                .collect::<Vec<_>>();
            crate::warp::domain(doc, &commands)
        });
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
            projective_mode: None,
            warp_mode: false,
            warp_domains,
            warp_preset: None,
            warp_amount: 0.15,
            context: Default::default(),
            context_frame: false,
            proportional: true,
            snap: false,
            prior_tool,
            gesture: crate::engine::id(),
            drag: None,
            error: None,
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
        let context_frame = editor.show_context(ui, available);
        let old_reference = editor.reference_point();
        let view = self.view;
        let screen = |p: [f64; 2]| view.point(p.map(|v| v as f32), rect, scale);
        let corners = editor.corners().map(screen);
        let edges = editor.edge_points().map(screen);
        let middle = edges[0];
        let center = screen(editor.reference_point());
        let rotation = middle + (middle - center).normalized() * 26.;
        let points = [
            corners[0], corners[1], corners[2], corners[3], edges[0], edges[1], edges[2], edges[3],
        ];
        let navigating = ui.input(|i| {
            context_frame
                || i.modifiers.alt
                || i.key_down(egui::Key::Space)
                || i.pointer.middle_down()
                || i.pointer.secondary_down()
        });
        if navigating {
            editor.drag = None;
        }
        let grid_points = (editor.warp_mode || editor.values.warp.is_some())
            .then(|| editor.grid_points())
            .transpose()
            .ok()
            .flatten()
            .map(|points| points.map(screen));
        let outline = editor.outline().into_iter().map(screen).collect::<Vec<_>>();
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let hit = pointer
            .filter(|p| {
                !navigating
                    && available.contains(*p)
                    && ui.ctx().layer_id_at(*p) == Some(ui.layer_id())
            })
            .and_then(|p| {
                // Center node wins over an overlapping reference; reference fields remain available.
                if editor.warp_mode {
                    if let Some(index) = grid_points
                        .as_ref()
                        .and_then(|points| points.iter().position(|node| p.distance(*node) < 9.))
                    {
                        return Some(Handle::Warp(index));
                    }
                }
                if p.distance(center) < 9. {
                    return Some(Handle::Pivot);
                }
                if p.distance(rotation) < 10. {
                    return Some(Handle::Rotate);
                }
                if let Some(mode) = editor.projective_mode {
                    if let Some(index) = corners.iter().position(|point| p.distance(*point) < 9.) {
                        return Some(Handle::Projective(
                            index,
                            mode == ProjectiveMode::Perspective,
                        ));
                    }
                }
                if editor.skew_mode {
                    return points[4..]
                        .iter()
                        .position(|point| p.distance(*point) < 9.)
                        .map(|index| Handle::Skew(index % 2));
                }
                if !editor.warp_mode
                    && editor.projective_mode.is_none()
                    && editor.mode == Tool::Scale
                {
                    return points
                        .iter()
                        .position(|point| p.distance(*point) < 9.)
                        .map(|index| {
                            Handle::Scale(index < 4 || index % 2 == 1, index < 4 || index % 2 == 0)
                        });
                }
                if !editor.warp_mode
                    && editor.projective_mode.is_none()
                    && editor.mode == Tool::Rotate
                {
                    return points
                        .iter()
                        .any(|point| p.distance(*point) < 10.)
                        .then_some(Handle::Rotate);
                }
                Rect::from_points(&outline)
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
        let edges = editor.edge_points().map(screen);
        let middle = edges[0];
        let center = screen(editor.reference_point());
        let rotation = middle + (middle - center).normalized() * 26.;
        painter.add(egui::Shape::closed_line(
            editor.outline().into_iter().map(screen).collect(),
            Stroke::new(1.25, ACCENT),
        ));
        let mut handles = if editor.skew_mode || editor.warp_mode {
            vec![]
        } else {
            corners.to_vec()
        };
        if (!editor.warp_mode && editor.projective_mode.is_none() && editor.mode == Tool::Scale)
            || editor.skew_mode
        {
            for i in 0..4 {
                handles.push(edges[i]);
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
        if editor.warp_mode || editor.values.warp.is_some() {
            if let Ok(nodes) = editor.grid_points() {
                let nodes = nodes.map(screen);
                for line in [
                    [0, 1, 2],
                    [3, 4, 5],
                    [6, 7, 8],
                    [0, 3, 6],
                    [1, 4, 7],
                    [2, 5, 8],
                ] {
                    painter.add(egui::Shape::line(
                        line.map(|i| nodes[i]).to_vec(),
                        Stroke::new(1., ACCENT),
                    ));
                }
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
        // Canvas and deferred shortcuts have finished; keep popup dismissal guarded
        // through Apply, then start the next draw without the frame latch.
        let context_frame = std::mem::take(&mut editor.context_frame);
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
            .default_height(ctx.screen_rect().height())
            .show(ctx, |ui| {
                let response = egui::ScrollArea::vertical()
                    .id_salt("free transform controls")
                    .auto_shrink([true, true])
                    .drag_to_scroll(false)
                    .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (mode, label) in [
                        (Tool::Move, "Move"),
                        (Tool::Rotate, "Rotate"),
                        (Tool::Scale, "Scale"),
                    ] {
                        if ui.selectable_label(editor.mode() == Some(mode), label).clicked() {
                            editor.set_mode(mode);
                        }
                    }
                    if ui.selectable_label(editor.skew_mode, "Skew").clicked() {
                        editor.skew_mode = true;
                        editor.projective_mode = None;
                        editor.warp_mode = false;
                        editor.drag = None;
                    }
                });
                ui.horizontal(|ui| {
                    for (mode, label) in [(ProjectiveMode::Distort, "Distort"),
                                           (ProjectiveMode::Perspective, "Perspective")] {
                        if ui.selectable_label(editor.projective_mode == Some(mode), label).clicked() {
                            editor.projective_mode = Some(mode);
                            editor.warp_mode = false;
                            editor.skew_mode = false;
                            editor.drag = None;
                        }
                    }
                    if ui.selectable_label(editor.warp_mode,"Grid Warp").clicked() {
                        editor.warp_mode=true;editor.skew_mode=false;editor.projective_mode=None;editor.drag=None;
                    }
                });
                if editor.warp_mode {
                    ui.label("Drag a grid point or choose a preset.");
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
                editor.projective_controls(ui);
                editor.warp_controls(ui);
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
                ui.horizontal(|ui| {
                    let ready = commands.is_ok()
                        && editor.context.is_none()
                        && !context_frame
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
                #[cfg(test)]
                {
                    editor.scroll_rect = Some(response.inner_rect);
                    editor.scroll_offset = response.state.offset;
                }
                #[cfg(not(test))]
                let _ = response;
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
