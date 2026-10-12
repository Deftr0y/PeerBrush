use super::*;
pub(super) struct GestureSource {
    document: String,
    revision: u64,
    layer: String,
}
const TOOLS: &[(&str, &str, Icon)] = &[
    ("rectangle", "Rectangular marquee", Icon::Selection),
    ("ellipse", "Elliptical marquee", Icon::EllipticalSelection),
    ("row", "Single row", Icon::RowSelection),
    ("column", "Single column", Icon::ColumnSelection),
    ("lasso", "Freehand lasso", Icon::Lasso),
    ("polygon", "Polygonal lasso", Icon::PolygonLasso),
    ("magnetic", "Magnetic lasso", Icon::MagneticLasso),
    ("wand", "Magic Wand", Icon::Wand),
    ("quick", "Quick Selection", Icon::QuickSelection),
    ("object", "Object region", Icon::ObjectSelection),
];
impl PeerBrush {
    pub(super) fn selection_gesture_stale(&self, doc: &Document) -> bool {
        self.selection_gesture_source
            .as_ref()
            .is_some_and(|source| {
                source.document != doc.id
                    || source.revision != doc.revision
                    || source.layer != self.selected
            })
    }
    pub(super) fn selection_source(&self, doc: &Document) -> GestureSource {
        GestureSource {
            document: doc.id.clone(),
            revision: doc.revision,
            layer: self.selected.clone(),
        }
    }
    fn cancel_selection_draft(&mut self, primary_down: bool) {
        self.selection_pointer_cancelled = primary_down;
        self.canvas_press_document = None;
        self.selection_path.clear();
        self.selection_gesture_mode = None;
        self.selection_gesture_source = None;
        self.points.clear();
        self.drag_start = None;
        self.drag_revision = None;
    }
    pub(super) fn deselect_area(&mut self, doc: &Document, tool: bool) {
        self.finish_parameter(true);
        self.cancel_geometry();
        self.cancel_source();
        self.cancel_filters();
        self.cancel_refinement();
        self.select_tool(if tool { Tool::None } else { self.tool });
        if doc.selection.is_some() {
            self.edit(vec![json!({"op":"selection.clear","document_id":doc.id,"source_revision":doc.revision})], "Deselect");
        }
    }
    fn wand_update_command(&self, doc: &Document) -> Value {
        json!({"op":"selection.wand_update", "document_id":doc.id,
            "source_revision":doc.revision, "tolerance":self.selection_tolerance,
            "feather":self.selection_feather, "contiguous":self.selection_contiguous,
            "sample_merged":self.selection_merged,"color_similarity":self.selection_color_similarity})
    }
    pub(super) fn selection_toolbar(&mut self, ui: &mut egui::Ui, doc: &Document) -> [Rect; 3] {
        let mut controls = [Rect::NOTHING; 3];
        let source = crate::selection::wand_source(doc);
        if self.selection_live_wand && self.parameter_gesture.is_none() {
            let display = self.animation.document(doc, ui.input(|i| i.time), false);
            if let Some(s) = crate::selection::wand_source(&display) {
                self.selection_tolerance = s.command["tolerance"].as_f64().unwrap_or(24.) as f32;
                self.selection_feather = s.command["feather"].as_f64().unwrap_or(0.) as f32;
                self.selection_contiguous = s.command["contiguous"].as_bool().unwrap_or(true);
                self.selection_merged = s.command["sample_merged"].as_bool().unwrap_or(false);
                self.selection_color_similarity =
                    s.command["color_similarity"].as_bool().unwrap_or(false);
            }
        }
        let live = self.selection_kind == "wand" && self.selection_live_wand && source.is_some();
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
                        for &(kind, label, icon) in TOOLS {
                            ui.horizontal(|ui| {
                                let (r, _) =
                                    ui.allocate_exact_size(Vec2::splat(22.), egui::Sense::hover());
                                icons::paint(ui.painter(), r, icon, ui.is_enabled());
                                if ui
                                    .selectable_value(&mut self.selection_kind, kind.into(), label)
                                    .changed()
                                {
                                    self.selection_path.clear();
                                    self.selection_gesture_source = None;
                                    self.selection_gesture_mode = None;
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
            let feather = controls::range(
                ui,
                "selection feather",
                &mut self.selection_feather,
                0.0..=64.0,
                95.,
                " px",
                1,
                false,
            );
            controls[0] = feather.rect;
            if live && (feather.changed() || feather.double_clicked()) {
                self.layer_parameter(self.wand_update_command(doc), "Wand feather", &feather);
            }
            if ["wand", "quick", "object"].contains(&self.selection_kind.as_str()) {
                controls::label(ui, if self.selection_color_similarity { "Similarity" } else { "Tolerance" });
                let tolerance = controls::range(
                    ui,
                    "selection tolerance",
                    &mut self.selection_tolerance,
                    0.0..=255.0,
                    85.,
                    "",
                    0,
                    false,
                );
                controls[1] = tolerance.rect;
                if live && (tolerance.changed() || tolerance.double_clicked()) {
                    self.layer_parameter(self.wand_update_command(doc), "Wand tolerance", &tolerance);
                }
                let mut sampling_changed = false;
                sampling_changed |= ui.checkbox(&mut self.selection_color_similarity, "Color similarity")
                    .on_hover_text("Maximum allowed difference in any RGB channel or alpha, from 0 to 255. Off keeps the existing averaged RGB-distance selection.").changed();
                if self.selection_kind == "wand" {
                    sampling_changed |= ui.checkbox(&mut self.selection_contiguous, "Contiguous").changed();
                }
                sampling_changed |= ui.checkbox(&mut self.selection_merged, "All Layers")
                    .on_hover_text(
                        "Sample visible rendered artwork across all layers. Leave off to sample only the active layer.",
                    ).changed();
                if self.selection_kind == "wand" {
                    let toggle = ui.checkbox(&mut self.selection_live_wand, "Live updates")
                        .on_hover_text("Update from the last wand click. After artwork or selection changes, click again. Drag previews selected pixels; release commits once.");
                    controls[2] = toggle.rect;
                    if toggle.changed() && !self.selection_live_wand {
                        self.finish_parameter(true);
                    }
                    if source.is_some() && self.selection_live_wand && (sampling_changed || toggle.changed()) {
                        self.edit(vec![self.wand_update_command(doc)], "Wand selection");
                    }
                }
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
        controls
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
    pub(super) fn select_command(&self, doc: &Document, points: &[[f32; 2]], mode: &str) -> Value {
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
        let source = self.selection_gesture_source.as_ref();
        json!({"op":"selection","document_id":source.map_or(doc.id.as_str(), |s| s.document.as_str()),"source_revision":source.map_or(doc.revision, |s| s.revision),"kind":self.selection_kind,"mode":mode,"rect":area,"polygon":polygon,"point":a,"points":points,"radius":self.selection_width,"feather":self.selection_feather,"tolerance":self.selection_tolerance,"contiguous":self.selection_contiguous,"sample_merged":self.selection_merged,"color_similarity":self.selection_color_similarity,"layer":source.map_or(self.selected.as_str(), |s| s.layer.as_str())})
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
        let view = self.view;
        let to_doc = |p: Pos2| view.document(p, rect, scale);
        let to_screen = |p: [f32; 2]| view.point(p, rect, scale);
        if ui.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        }) {
            // Window focus loss may share a frame with a real or synthesized
            // release. Cancel before response.clicked/drag_stopped can commit
            // an unfinished selection over the existing shared coverage.
            self.cancel_selection_draft(ui.input(|i| i.pointer.primary_down()));
            return;
        }
        if self.selection_gesture_stale(doc) {
            self.cancel_selection_draft(ui.input(|i| i.pointer.primary_down()));
            self.message = "Image changed during selection — draft cancelled".into();
            return;
        }
        if self.selection_pointer_cancelled {
            if !ui.input(|i| i.pointer.primary_down()) {
                self.selection_pointer_cancelled = false;
                self.canvas_press_document = None;
            }
            return;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_selection_draft(ui.input(|i| i.pointer.primary_down()));
            return;
        }
        // Bind even quick-click tools before their release. Paths retain the
        // first anchor's source across clicks; drafts never retarget later work.
        if self.selection_gesture_source.is_none()
            && response.hovered()
            && ui.input(|i| i.pointer.primary_pressed())
            && ui
                .input(|i| i.pointer.interact_pos())
                .is_some_and(|p| view.contains(p, rect, scale))
        {
            self.selection_gesture_source = Some(self.selection_source(doc));
        }
        // The response covers only the canvas workspace, excluding panels and controls.
        if response.clicked()
            && response
                .interact_pointer_pos()
                .is_some_and(|p| !view.contains(p, rect, scale))
        {
            self.deselect_area(doc, false);
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
                    .filter(|p| view.contains(*p, rect, scale))
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
                    .filter(|p| view.contains(*p, rect, scale))
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
                    self.selection_gesture_source = None;
                    self.selection_gesture_mode = None;
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
                    .filter(|p| view.contains(*p, rect, scale))
                {
                    points.push(p);
                }
                painter.add(egui::Shape::line(points, Stroke::new(1.0_f32, ACCENT)));
            }
            if self.selection_path.is_empty() && !ui.input(|i| i.pointer.primary_down()) {
                self.selection_gesture_source = None;
                self.selection_gesture_mode = None;
            }
            return;
        }
        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = ui
                .input(|i| i.pointer.press_origin())
                .filter(|p| view.contains(*p, rect, scale))
            {
                let p = to_doc(pos);
                self.drag_start = Some(p);
                self.points = vec![p];
                self.selection_gesture_mode = Some(self.selection_mode_for(ui));
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) && self.drag_start.is_some() {
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
                let r = Rect::from_two_pos(Pos2::new(a[0], a[1]), Pos2::new(b[0], b[1]));
                let points = (0..64)
                    .map(|i| {
                        let angle = i as f32 * std::f32::consts::TAU / 64.;
                        let p = r.center()
                            + Vec2::new(
                                angle.cos() * r.width() / 2.,
                                angle.sin() * r.height() / 2.,
                            );
                        to_screen([p.x, p.y])
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
                polygon(
                    ui,
                    painter,
                    &[[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]]].map(to_screen),
                );
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) && self.drag_start.is_some() {
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
            self.selection_gesture_source = None;
            self.selection_gesture_mode = None;
        }
        if response.clicked() {
            if let Some(pos) = response.interact_pointer_pos() {
                if view.contains(pos, rect, scale)
                    && ["wand", "quick", "row", "column"].contains(&self.selection_kind.as_str())
                {
                    let command =
                        self.select_command(doc, &[to_doc(pos)], &self.selection_mode_for(ui));
                    self.edit(vec![command], "Select region");
                } else {
                    self.deselect_area(doc, false);
                }
            }
            self.selection_gesture_source = None;
        }
        if self.drag_start.is_none() && !ui.input(|i| i.pointer.primary_down()) {
            self.selection_gesture_source = None;
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
