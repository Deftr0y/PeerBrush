use super::*;
const TOOLS: &[(&str, &str, usize)] = &[
    ("rectangle", "Rectangular marquee", 12),
    ("ellipse", "Elliptical marquee", 11),
    ("row", "Single row", 21),
    ("column", "Single column", 22),
    ("lasso", "Freehand lasso", 16),
    ("polygon", "Polygonal lasso", 17),
    ("magnetic", "Magnetic lasso", 18),
    ("wand", "Magic Wand", 19),
    ("quick", "Quick Selection", 20),
    ("object", "Object region", 23),
];
impl PeerBrush {
    pub(super) fn selection_toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_centered(|ui| {
            if self.tool == Tool::MagicWand {
                ui.label("Magic Wand");
            } else {
                egui::ComboBox::from_id_salt("selection tool")
                    .width(150.)
                    .selected_text(
                        TOOLS
                            .iter()
                            .find(|t| t.0 == self.selection_kind)
                            .map_or("Marquee", |t| t.1),
                    )
                    .show_ui(ui, |ui| {
                        for &(kind, label, cell) in TOOLS {
                            ui.horizontal(|ui| {
                                let (r, _) =
                                    ui.allocate_exact_size(Vec2::splat(22.), egui::Sense::hover());
                                icons::paint_generated(ui.painter(), r, cell, true);
                                if ui
                                    .selectable_value(&mut self.selection_kind, kind.into(), label)
                                    .changed()
                                {
                                    self.selection_path.clear();
                                    self.points.clear();
                                    self.drag_start = None;
                                }
                            });
                        }
                    });
            }
            egui::ComboBox::from_id_salt("selection operation")
                .width(85.)
                .selected_text(&self.selection_mode)
                .show_ui(ui, |ui| {
                    for mode in ["replace", "add", "subtract", "intersect"] {
                        ui.selectable_value(&mut self.selection_mode, mode.into(), mode);
                    }
                });
            controls::label(ui, "Feather");
            controls::range(
                ui,
                "selection feather",
                &mut self.selection_feather,
                0.0..=64.0,
                95.,
                " px",
                1,
                false,
            );
            if ["wand", "quick", "object"].contains(&self.selection_kind.as_str()) {
                controls::label(ui, "Tolerance");
                controls::range(
                    ui,
                    "selection tolerance",
                    &mut self.selection_tolerance,
                    0.0..=255.0,
                    85.,
                    "",
                    0,
                    false,
                );
                if self.selection_kind == "wand" {
                    ui.checkbox(&mut self.selection_contiguous, "Contiguous");
                }
                ui.checkbox(&mut self.selection_merged, "Merged")
                    .on_hover_text(
                        "Sample the rendered composite; disable to sample the active layer",
                    );
            }
            if ["quick", "magnetic"].contains(&self.selection_kind.as_str()) {
                controls::label(
                    ui,
                    if self.selection_kind == "magnetic" {
                        "Snap width"
                    } else {
                        "Radius"
                    },
                );
                controls::range(
                    ui,
                    "selection width",
                    &mut self.selection_width,
                    2.0..=64.0,
                    90.,
                    " px",
                    0,
                    false,
                );
            }
            if ["polygon", "magnetic"].contains(&self.selection_kind.as_str()) {
                ui.label(
                    RichText::new("Enter / double-click to close")
                        .size(11.)
                        .color(MUTED),
                );
            }
        });
    }
    fn selection_mode_for(&self, ui: &egui::Ui) -> String {
        ui.input(|i| match (i.modifiers.shift, i.modifiers.alt) {
            (true, true) => "intersect",
            (true, false) => "add",
            (false, true) => "subtract",
            _ => self.selection_mode.as_str(),
        })
        .to_owned()
    }
    fn select_command(&self, doc: &Document, points: &[[f32; 2]], mode: &str) -> Value {
        let a = points.first().copied().unwrap_or([0., 0.]);
        let b = points.last().copied().unwrap_or(a);
        let mut area = [
            a[0].min(b[0]).floor() as i32,
            a[1].min(b[1]).floor() as i32,
            a[0].max(b[0]).ceil() as i32,
            a[1].max(b[1]).ceil() as i32,
        ];
        if self.selection_kind == "row" {
            area = [
                0,
                a[1].floor() as i32,
                doc.width as i32,
                a[1].floor() as i32 + 1,
            ];
        }
        if self.selection_kind == "column" {
            area = [
                a[0].floor() as i32,
                0,
                a[0].floor() as i32 + 1,
                doc.height as i32,
            ];
        }
        let polygon = ["lasso", "polygon", "magnetic"]
            .contains(&self.selection_kind.as_str())
            .then_some(points);
        json!({"op":"selection","kind":self.selection_kind,"mode":mode,"rect":area,"polygon":polygon,"point":a,"points":points,"radius":self.selection_width,"feather":self.selection_feather,"tolerance":self.selection_tolerance,"contiguous":self.selection_contiguous,"sample_merged":self.selection_merged,"layer":self.selected})
    }
    pub(super) fn selection_canvas(
        &mut self,
        ui: &egui::Ui,
        doc: &Document,
        response: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
        scale: f32,
    ) {
        let to_doc = |p: Pos2| [(p.x - rect.min.x) / scale, (p.y - rect.min.y) / scale];
        let to_screen = |p: [f32; 2]| rect.min + Vec2::new(p[0] * scale, p[1] * scale);
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selection_path.clear();
            self.points.clear();
            self.drag_start = None;
            return;
        }
        let path_tool = ["polygon", "magnetic"].contains(&self.selection_kind.as_str());
        if path_tool {
            if ui.input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete))
            {
                self.selection_path.pop();
            }
            if response.clicked() || response.double_clicked() {
                if let Some(pos) = response
                    .interact_pointer_pos()
                    .filter(|p| rect.contains(*p))
                {
                    if self.selection_path.is_empty() {
                        self.selection_gesture_mode = Some(self.selection_mode_for(ui));
                    }
                    let mut p = to_doc(pos);
                    if self.selection_kind == "magnetic" {
                        p = crate::selection::magnetic_point(doc, p, self.selection_width);
                    }
                    if self.selection_path.len() < 8192 && self.selection_path.last() != Some(&p) {
                        self.selection_path.push(p);
                    }
                }
            }
            if self.selection_kind == "magnetic"
                && !self.selection_path.is_empty()
                && response.hovered()
            {
                if let Some(pos) = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| rect.contains(*p))
                {
                    let p = to_doc(pos);
                    if self
                        .selection_path
                        .last()
                        .is_some_and(|a| (a[0] - p[0]).hypot(a[1] - p[1]) > 3.0)
                        && self.selection_path.len() < 8192
                    {
                        self.selection_path.push(crate::selection::magnetic_point(
                            doc,
                            p,
                            self.selection_width,
                        ));
                    }
                }
            }
            if response.double_clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if self.selection_path.len() >= 3 {
                    let command = self.select_command(
                        doc,
                        &self.selection_path,
                        self.selection_gesture_mode
                            .as_deref()
                            .unwrap_or(&self.selection_mode),
                    );
                    self.edit(vec![command], "Lasso selection");
                    self.selection_path.clear();
                }
            }
            if !self.selection_path.is_empty() {
                let mut points = self
                    .selection_path
                    .iter()
                    .copied()
                    .map(to_screen)
                    .collect::<Vec<_>>();
                if let Some(p) = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| rect.contains(*p))
                {
                    points.push(p);
                }
                painter.add(egui::Shape::line(points, Stroke::new(1.0_f32, ACCENT)));
            }
            return;
        }
        if response.drag_started() {
            if let Some(pos) = ui
                .input(|i| i.pointer.press_origin())
                .filter(|p| rect.contains(*p))
            {
                let p = to_doc(pos);
                self.drag_start = Some(p);
                self.points = vec![p];
                self.selection_gesture_mode = Some(self.selection_mode_for(ui));
            }
        }
        if response.dragged() && self.drag_start.is_some() {
            if let Some(pos) = response.interact_pointer_pos() {
                let p = to_doc(pos);
                let old = *self.points.last().unwrap();
                if (old[0] - p[0]).hypot(old[1] - p[1]) >= 0.5 && self.points.len() < 8192 {
                    if self.selection_kind == "quick" {
                        let n = ((old[0] - p[0]).hypot(old[1] - p[1])
                            / (self.selection_width * 0.3).max(1.))
                        .ceil() as usize;
                        for j in 1..=n.min(8192 - self.points.len()) {
                            let t = j as f32 / n as f32;
                            self.points
                                .push([old[0] + (p[0] - old[0]) * t, old[1] + (p[1] - old[1]) * t]);
                        }
                    } else {
                        self.points.push(p);
                    }
                }
            }
        }
        if let (Some(a), Some(b)) = (self.drag_start, self.points.last().copied()) {
            if self.selection_kind == "lasso" {
                let points = self
                    .points
                    .iter()
                    .copied()
                    .map(to_screen)
                    .collect::<Vec<_>>();
                polygon(ui, painter, &points);
            } else if self.selection_kind == "ellipse" {
                let r = Rect::from_two_pos(to_screen(a), to_screen(b));
                let points = (0..64)
                    .map(|i| {
                        let angle = i as f32 * std::f32::consts::TAU / 64.;
                        r.center()
                            + Vec2::new(angle.cos() * r.width() / 2., angle.sin() * r.height() / 2.)
                    })
                    .collect::<Vec<_>>();
                polygon(ui, painter, &points);
            } else if self.selection_kind == "quick" {
                painter.circle_stroke(
                    to_screen(b),
                    self.selection_width * scale,
                    Stroke::new(1.0_f32, ACCENT),
                );
            } else {
                outline(ui, painter, Rect::from_two_pos(to_screen(a), to_screen(b)));
            }
        }
        if response.drag_stopped() && self.drag_start.is_some() {
            if self.points.len() > 1 {
                let command = self.select_command(
                    doc,
                    &self.points,
                    self.selection_gesture_mode
                        .as_deref()
                        .unwrap_or(&self.selection_mode),
                );
                self.edit(vec![command], "Select region");
            }
            self.points.clear();
            self.drag_start = None;
        }
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if rect.contains(pos)
                    && ["wand", "quick", "row", "column"].contains(&self.selection_kind.as_str())
                {
                    let command =
                        self.select_command(doc, &[to_doc(pos)], &self.selection_mode_for(ui));
                    self.edit(vec![command], "Select region");
                } else if self.selection_mode_for(ui) == "replace" {
                    self.edit(vec![json!({"op":"selection.clear"})], "Deselect");
                }
            }
        }
    }
}
pub(super) fn outline(ui: &egui::Ui, painter: &egui::Painter, rect: Rect) {
    polygon(
        ui,
        painter,
        &[
            rect.left_top(),
            rect.right_top(),
            rect.right_bottom(),
            rect.left_bottom(),
        ],
    );
}
pub(super) fn polygon(ui: &egui::Ui, painter: &egui::Painter, points: &[Pos2]) {
    if points.len() < 3 {
        return;
    }
    let phase = ui.input(|i| (i.time * 12.0 % 8.0) as f32);
    let clip = painter.clip_rect();
    let mut distance = 0.0;
    for (a, b) in points
        .iter()
        .copied()
        .zip(points.iter().copied().cycle().skip(1))
        .take(points.len())
    {
        let length = a.distance(b);
        if length < 0.1 {
            continue;
        }
        let direction = (b - a) / length;
        let mut lo = 0.0_f32;
        let mut hi = length;
        for (origin, delta, min, max) in [
            (a.x, direction.x, clip.left(), clip.right()),
            (a.y, direction.y, clip.top(), clip.bottom()),
        ] {
            if delta.abs() < 0.001 {
                if origin < min || origin > max {
                    hi = -1.0;
                }
            } else {
                let t0 = (min - origin) / delta;
                let t1 = (max - origin) / delta;
                lo = lo.max(t0.min(t1));
                hi = hi.min(t0.max(t1));
            }
        }
        if hi >= lo {
            painter.line_segment(
                [a + direction * lo, a + direction * hi],
                Stroke::new(1.5_f32, Color32::BLACK),
            );
            let offset = distance + phase;
            let mut t = ((lo + offset) / 8.0).floor() * 8.0 - offset;
            while t < hi {
                let from = t.max(lo);
                let to = (t + 4.0).min(hi);
                if to > from {
                    painter.line_segment(
                        [a + direction * from, a + direction * to],
                        Stroke::new(1.5_f32, Color32::WHITE),
                    );
                }
                t += 8.0;
            }
        }
        distance = (distance + length) % 8.0;
    }
    ui.ctx().request_repaint_after(Duration::from_millis(60));
}
