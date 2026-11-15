use super::*;
use crate::selection::MAX_BRUSH_SEEDS;

pub(super) struct GestureSource {
    engine: std::sync::Weak<std::sync::Mutex<crate::engine::Engine>>,
    project: String,
    document: String,
    revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
    layer: String,
    press: Option<AreaPress>,
    _charge: Option<Arc<crate::psd::memory36::MetadataCharge>>,
    preview_charge: Option<Arc<crate::psd::memory36::MetadataCharge>>,
}

#[derive(Clone, Copy)]
struct AreaPress {
    screen: Pos2,
    document: [f32; 2],
    modifiers: egui::Modifiers,
    last_sample: [f32; 2],
}

fn quick_segment(
    seeds: &mut Vec<[f32; 2]>,
    from: [f32; 2],
    to: [f32; 2],
    size: [f32; 2],
    spacing: f32,
    release: bool,
) {
    let limit = MAX_BRUSH_SEEDS - usize::from(!release);
    if seeds.len() >= limit || !from.into_iter().chain(to).all(f32::is_finite) {
        return;
    }
    // The engine floors coordinates and treats the right and bottom edges as exclusive.
    let [right, bottom] = size.map(f32::next_down);
    let Some((start, end)) = crate::geometry::clipped_segment(from, to, [0., 0., right, bottom]) else {
        return;
    };
    let in_image = |[x, y]: [f32; 2]| [x.clamp(0., right), y.clamp(0., bottom)];
    let (start, end) = (in_image(start), in_image(end));
    let delta = [end[0] - start[0], end[1] - start[1]];
    let count = (delta[0].hypot(delta[1]) / spacing).ceil() as usize;
    for i in 0..=count {
        let t = i as f32 / count.max(1) as f32;
        let seed = in_image(std::array::from_fn(|axis| start[axis] + delta[axis] * t));
        if seeds.last().is_none_or(|last| last.map(f32::floor) != seed.map(f32::floor)) {
            seeds.push(seed);
            if seeds.len() == limit {
                if release {
                    *seeds.last_mut().unwrap() = end;
                }
                break;
            }
        }
    }
}

pub(super) fn motion_before_release(input: &egui::InputState, held: bool) -> impl Iterator<Item = Pos2> + '_ {
    let (start, release) = canvas_contact_range(input, held);
    let end = release.unwrap_or(input.events.len());
    input.events[start..end].iter().filter_map(|event| match event {
        egui::Event::PointerMoved(pos) => Some(*pos),
        _ => None,
    })
}
impl GestureSource {
    pub(super) fn advance_sampled_stroke(&mut self, doc: DocumentView<'_>, previous: u64) {
        if self.document == doc.id() && self.revision == previous {
            self.revision = doc.revision();
            self.native = doc.native().map(|native| native.key());
        }
    }

    pub(super) fn observes(&self, app: &PeerBrush, doc: DocumentView<'_>) -> bool {
        self.engine.as_ptr() == Arc::as_ptr(&app.shared)
            && self.project == app.project_id
            && self.document == doc.id()
            && self.revision == doc.revision()
            && self.native == doc.native().map(|doc| doc.key())
            && self.layer == app.selected
    }
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
    pub(super) fn selection_gesture_stale<'a>(&self, doc: impl Into<DocumentView<'a>>) -> bool {
        let doc = doc.into();
        self.selection_gesture_source
            .as_ref()
            .is_some_and(|source| !source.observes(self, doc))
    }
    pub(super) fn canvas_gesture_source<'a>(
        &self,
        doc: impl Into<DocumentView<'a>>,
    ) -> Result<GestureSource, String> {
        let doc = doc.into();
        let charge = doc
            .native()
            .map(|native| {
                let bytes = [self.project_id.len(), doc.id().len(), self.selected.len()]
                    .into_iter()
                    .try_fold(2048usize, |sum, n| sum.checked_add(n))
                    .filter(|n| *n <= 64 * 1024 * 1024)
                    .ok_or("Canvas gesture source exceeds admission")?;
                crate::psd::memory36::MetadataCharge::reserve(native.budget(), bytes, &|| Ok(()))
            })
            .transpose()?;
        Ok(GestureSource {
            engine: Arc::downgrade(&self.shared),
            project: self.project_id.clone(),
            document: doc.id().to_owned(),
            revision: doc.revision(),
            native: doc.native().map(|doc| doc.key()),
            layer: self.selected.clone(),
            press: None,
            preview_charge: None,
            _charge: charge,
        })
    }
    pub(super) fn cancel_selection_draft(&mut self, primary_down: bool) {
        if self.drag_start.is_some() && self.selection_gesture_source.is_some() {
            // Any in-flight coverage belongs to the gesture that just ended.
            self.preview_epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.last_preview = None;
            self.canvas_preview = None;
            self.canvas_selection = None;
            self.pending = false;
        }
        self.selection_pointer_cancelled = primary_down;
        self.canvas_press_document = None;
        self.clear_selection_path();
        self.selection_gesture_mode = None;
        self.selection_gesture_source = None;
        self.points.clear();
        self.drag_start = None;
        self.drag_revision = None;
        self.live_gesture = None;
    }

    pub(super) fn selection_drag_preview_key(&self) -> Option<&str> {
        if self.drag_start.is_some() && self.selection_gesture_source.is_some()
            && self.transient.len() == 1 && self.transient[0]["op"] == "selection"
        {
            self.live_gesture.as_deref()
        } else {
            None
        }
    }
    pub(super) fn deselect_area<'a>(&mut self, doc: impl Into<DocumentView<'a>>, tool: bool) {
        let doc = doc.into();
        self.finish_parameter(true);
        self.cancel_geometry();
        self.cancel_source();
        self.cancel_filters();
        self.cancel_refinement();
        self.select_tool(if tool { Tool::None } else { self.tool });
        if doc.selection().is_some() {
            self.edit(vec![json!({"op":"selection.clear","document_id":doc.id(),"source_revision":doc.revision()})], "Deselect");
        }
    }
    fn wand_update_command<'a>(&self, doc: impl Into<DocumentView<'a>>) -> Value {
        let doc = doc.into();
        json!({"op":"selection.wand_update", "document_id":doc.id(),
            "source_revision":doc.revision(), "tolerance":self.selection_tolerance,
            "feather":self.selection_feather, "contiguous":self.selection_contiguous,
            "sample_merged":self.selection_merged,"color_similarity":self.selection_color_similarity})
    }
    pub(super) fn selection_toolbar<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
    ) -> [Rect; 3] {
        let doc = doc.into();
        let mut controls = [Rect::NOTHING; 3];
        let source = doc.wand_source();
        if self.selection_kind == "wand" && self.selection_live_wand && self.parameter_gesture.is_none() {
            let numbers = self.animation.controls(doc, ui.input(|i| i.time));
            if let Some(s) = source {
                self.selection_tolerance = numbers.wand_number(s, "tolerance", 24.);
                self.selection_feather = numbers.wand_number(s, "feather", 0.);
                self.selection_contiguous = s.1["contiguous"].as_bool().unwrap_or(true);
                self.selection_merged = s.1["sample_merged"].as_bool().unwrap_or(false);
                self.selection_color_similarity =
                    s.1["color_similarity"].as_bool().unwrap_or(false);
            }
        }
        ui.horizontal_centered(|ui| {
            if self.tool == Tool::MagicWand {
                ui.label("Magic Wand");
            } else {
                let label = TOOLS
                    .iter()
                    .find(|t| t.0 == self.selection_kind)
                    .map_or("Marquee", |t| t.1);
                let menu = egui::ComboBox::from_id_salt("selection tool")
                    .width(150.)
                    .selected_text(label)
                    .show_ui(ui, |ui| {
                        for &(kind, label, icon) in TOOLS {
                            let selected = self.selection_kind == kind;
                            if icons::menu_choice(ui, icon, label, Some(selected)).clicked() && !selected {
                                self.selection_kind = kind.into();
                                self.cancel_selection_draft(ui.input(|input| input.pointer.primary_down()));
                            }
                        }
                    });
                menu.response.widget_info(|| {
                    let mut info = egui::WidgetInfo::labeled(
                        egui::WidgetType::ComboBox,
                        menu.response.enabled(),
                        "Selection tool",
                    );
                    info.current_text_value = Some(label.into());
                    info
                });
            }
            let live = self.selection_kind == "wand" && self.selection_live_wand && source.is_some();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.;
                for (mode, icon, label) in [
                    ("replace", Icon::Selection, "New selection"),
                    ("add", Icon::SelectionAdd, "Add to selection"),
                    ("subtract", Icon::SelectionSubtract, "Subtract from selection"),
                    ("intersect", Icon::SelectionIntersect, "Intersect with selection"),
                ] {
                    if icons::selectable(ui, icon, self.selection_mode == mode, label).clicked() {
                        self.selection_mode = mode.into();
                    }
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
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.;
                    for (similarity, icon, label, help) in [
                        (false, Icon::ColorBalance, "Combined RGB distance",
                            "Compare the combined RGB distance and the alpha difference with the tolerance."),
                        (true, Icon::Hue, "Color similarity by channel",
                            "Require each RGB channel and alpha to be within the tolerance. A large change in any one channel excludes that pixel."),
                    ] {
                        let selected = self.selection_color_similarity == similarity;
                        if icons::selectable(ui, icon, selected, label).on_hover_text(help).clicked()
                            && !selected
                        {
                            self.selection_color_similarity = similarity;
                            sampling_changed = true;
                        }
                    }
                });
                if self.selection_kind == "wand" {
                    controls::label(ui, "Match");
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.;
                        for (contiguous, icon, label, help) in [
                            (true, Icon::ConnectedMatches, "Contiguous",
                                "Select matching pixels connected to where you click. Gaps of nonmatching pixels stop the selection."),
                            (false, Icon::AllMatches, "All matches",
                                "Select matching pixels anywhere in the sampled image, including disconnected areas."),
                        ] {
                            let selected = self.selection_contiguous == contiguous;
                            if icons::selectable(ui, icon, selected, label).on_hover_text(help).clicked()
                                && !selected
                            {
                                self.selection_contiguous = contiguous;
                                sampling_changed = true;
                            }
                        }
                    });
                }
                sampling_changed |= controls::layer_sampling(
                    ui,
                    &mut self.selection_merged,
                    "Sample active layer",
                    "Use colors from the active layer.",
                    "Use the visible rendered composite across all layers.",
                );
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
                let magnetic = self.selection_kind == "magnetic";
                controls::label(ui, if magnetic { "Snap width" } else { "Radius" });
                let width = controls::range(
                    ui,
                    "selection width",
                    &mut self.selection_width,
                    2.0..=64.0,
                    90.,
                    " px",
                    0,
                    false,
                );
                if !magnetic {
                    width.on_hover_text("Grow connected color matches within this radius as you brush.");
                }
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
    pub(super) fn selection_mode_with(&self, modifiers: egui::Modifiers) -> String {
        match (modifiers.shift, modifiers.alt) {
            (true, true) => "intersect",
            (true, false) => "add",
            (false, true) => "subtract",
            _ => self.selection_mode.as_str(),
        }
        .to_owned()
    }
    fn push_selection_point(&mut self, point: [f32; 2], release: bool, size: [f32; 2]) {
        if self.selection_kind == "quick" {
            let Some(press) = self.selection_gesture_source.as_mut().and_then(|source| source.press.as_mut()) else { return; };
            let old = std::mem::replace(&mut press.last_sample, point);
            quick_segment(&mut self.points, old, point, size, (self.selection_width * 0.3).max(1.), release);
            return;
        }
        if self.selection_kind != "lasso" {
            self.points.truncate(1);
            self.points.push(point);
            return;
        }
        let limit = 8192 - usize::from(!release);
        let old = *self.points.last().unwrap();
        let distance = (old[0] - point[0]).hypot(old[1] - point[1]);
        if (distance >= 0.5 || release && distance > 0.) && self.points.len() < limit {
            self.points.push(point);
        }
    }
    pub(super) fn select_command<'a>(
        &self,
        doc: impl Into<DocumentView<'a>>,
        points: &[[f32; 2]],
        mode: &str,
    ) -> Value {
        let doc = doc.into();
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
                doc.width() as i32,
                a[1].floor() as i32 + 1,
            ];
        }
        if self.selection_kind == "column" {
            area = [
                a[0].floor() as i32,
                0,
                a[0].floor() as i32 + 1,
                doc.height() as i32,
            ];
        }
        if ["rectangle", "quick", "object"].contains(&self.selection_kind.as_str()) {
            let width = doc.width() as i32;
            let height = doc.height() as i32;
            area = [
                area[0].clamp(0, width),
                area[1].clamp(0, height),
                area[2].clamp(0, width),
                area[3].clamp(0, height),
            ];
        }
        let polygon = ["lasso", "polygon", "magnetic"]
            .contains(&self.selection_kind.as_str())
            .then_some(points);
        let point = if self.selection_kind == "object" {
            std::array::from_fn(|axis| (area[axis] + area[axis + 2]) as f32 * 0.5)
        } else {
            a
        };
        let source = self.selection_gesture_source.as_ref();
        let contiguous = self.selection_kind == "quick" || self.selection_contiguous;
        let seeds = matches!(self.selection_kind.as_str(), "wand" | "quick").then_some(points);
        json!({"op":"selection","document_id":source.map_or(doc.id(), |s| s.document.as_str()),"source_revision":source.map_or(doc.revision(), |s| s.revision),"kind":self.selection_kind,"mode":mode,"rect":area,"polygon":polygon,"point":point,"points":seeds,"radius":self.selection_width,"feather":self.selection_feather,"tolerance":self.selection_tolerance,"contiguous":contiguous,"sample_merged":self.selection_merged,"color_similarity":self.selection_color_similarity,"layer":source.map_or(self.selected.as_str(), |s| s.layer.as_str())})
    }
    pub(super) fn selection_canvas<'a>(
        &mut self,
        ui: &egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        response: &egui::Response,
        painter: &egui::Painter,
        projection: &view::Projection,
    ) {
        let doc = doc.into();
        let to_doc = |p| projection.document(p);
        let to_screen = |p| projection.point(p);
        if Self::pointer_contact_cancelled(ui.ctx()) || ui.input(|i| {
            !i.focused
                || i.events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        }) {
            // Focus or contact loss can share a frame with a button-up.
            // Discard the draft before processing that frame's release.
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
        let workspace_drag = ["rectangle", "ellipse", "lasso", "object", "quick"]
            .contains(&self.selection_kind.as_str());
        let path_tool = ["polygon", "magnetic"].contains(&self.selection_kind.as_str());
        let wand = self.selection_kind == "wand";
        let line_axis = match self.selection_kind.as_str() {
            "row" => Some((1, doc.height())),
            "column" => Some((0, doc.width())),
            _ => None,
        };
        let workspace_sample = wand || line_axis.is_some();
        let size = [doc.width() as f32, doc.height() as f32];
        let click_reaches_image = |pos| {
            let point = to_doc(pos);
            if let Some((axis, length)) = line_axis {
                (0. ..length as f32).contains(&point[axis])
            } else {
                (0..2).all(|axis| (0. ..size[axis]).contains(&point[axis]))
            }
        };
        let held = (workspace_drag || workspace_sample) && self.selection_gesture_source.is_some();
        let (press, release, space, primary_down, pointer) = ui.input(|input| {
            let (press, release) = canvas_contact(input, held);
            let down = input.pointer.primary_down();
            let pointer = release.map(|(pos, _)| pos)
                .or_else(|| down.then(|| input.pointer.interact_pos()).flatten());
            (press, release, input.key_down(egui::Key::Space), down, pointer)
        });
        if held && !primary_down && release.is_none() {
            self.cancel_selection_draft(false);
            return;
        }
        let press = press.filter(|(pos, _)| {
            !space && canvas_press_hits(ui, response, *pos)
                && (workspace_drag || path_tool || workspace_sample)
        });
        // Bind even quick-click tools before their release. Paths retain the
        // first anchor's source across clicks; drafts never retarget later work.
        if let Some((pos, modifiers)) = press.filter(|_| self.selection_gesture_source.is_none()) {
            match self.canvas_gesture_source(doc) {
                Ok(mut source) => {
                    source.press = workspace_drag.then(|| {
                        let document = to_doc(pos);
                        AreaPress { screen: pos, document, modifiers, last_sample: document }
                    });
                    self.selection_gesture_source = Some(source);
                }
                Err(error) => {
                    self.message = error.into();
                    self.cancel_selection_draft(ui.input(|i| i.pointer.primary_down()));
                    return;
                }
            }
        }
        // The response covers only the canvas workspace, excluding panels and controls.
        // Movement after a release can share its frame. Use the actual button
        // location for clicks rather than the pointer's later hover location.
        let click = canvas_click_sample(ui, response);
        let path_click = click.filter(|_| path_tool && !space && self.selection_gesture_source.is_some());
        if self.selection_kind == "magnetic" && doc.native().is_some() {
            self.native_magnetic_canvas(ui, doc, response, painter, projection, path_click);
            return;
        }
        if path_tool {
            if ui.input(|i| i.key_pressed(egui::Key::Backspace) || i.key_pressed(egui::Key::Delete))
            {
                self.selection_path.pop();
            }
            if let Some((pos, modifiers)) = path_click {
                if self.selection_path.is_empty() {
                    self.selection_gesture_mode = Some(self.selection_mode_with(modifiers));
                }
                let mut point = to_doc(pos);
                if self.selection_kind == "magnetic" && projection.contains(pos) {
                    if let Some(resident) = doc.resident() {
                        point = crate::selection::magnetic_point(resident, point, self.selection_width);
                    }
                }
                if self.selection_path.len() < 8192 && self.selection_path.last() != Some(&point) {
                    self.selection_path.push(point);
                }
            }
            if self.selection_kind == "magnetic"
                && !self.selection_path.is_empty()
                && response.hovered()
            {
                if let Some(pos) = ui
                    .input(|i| i.pointer.hover_pos())
                    .filter(|p| projection.contains(*p))
                {
                    let p = to_doc(pos);
                    if self
                        .selection_path
                        .last()
                        .is_some_and(|a| (a[0] - p[0]).hypot(a[1] - p[1]) > 3.0)
                        && self.selection_path.len() < 8192
                    {
                        if let Some(resident) = doc.resident() {
                            self.selection_path.push(crate::selection::magnetic_point(
                                resident,
                                p,
                                self.selection_width,
                            ));
                        }
                    }
                }
            }
            if (path_click.is_some() && response.double_clicked()) || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                if self.selection_path.len() >= 3 {
                    let command = self.select_command(
                        doc,
                        &self.selection_path,
                        self.selection_gesture_mode
                            .as_deref()
                            .unwrap_or(&self.selection_mode),
                    );
                    self.edit(vec![command], "Lasso selection");
                    self.clear_selection_path();
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
                if let Some(p) = response.hover_pos() {
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
        let quick = self.selection_kind == "quick";
        if workspace_drag && !space && self.drag_start.is_none() {
            let drag_press = self.selection_gesture_source.as_ref().and_then(|source| source.press);
            if let Some(press) = drag_press.filter(|press| {
                let slop = ui.ctx().options(|options| options.input_options.max_click_dist);
                response.drag_started_by(egui::PointerButton::Primary)
                    || pointer.is_some_and(|pos| press.screen.distance(pos) > slop)
                    || ui.input(|input| {
                        motion_before_release(input, held).any(|pos| press.screen.distance(pos) > slop)
                    })
            }) {
                self.drag_start = Some(press.document);
                self.points.clear();
                if !quick {
                    self.points.push(press.document);
                }
                self.live_gesture = Some(crate::engine::id());
                self.selection_gesture_mode = Some(self.selection_mode_with(press.modifiers));
            }
        }
        // The captured release ends this drag even if a later press shares the frame.
        let drag_finished = self.drag_start.is_some() && release.is_some();
        // Ownership was established at the press. The pointer can cross over
        // other widgets before the button-up without handing them the drag.
        if let Some(pos) = pointer.filter(|_| self.drag_start.is_some()) {
            if quick || self.selection_kind == "lasso" {
                ui.input(|input| {
                    for pos in motion_before_release(input, held) {
                        self.push_selection_point(to_doc(pos), false, size);
                    }
                });
            }
            self.push_selection_point(to_doc(pos), drag_finished, size);
        }
        if let Some(start) = self.drag_start {
            let ready = match self.selection_kind.as_str() {
                "quick" => !self.points.is_empty(),
                "lasso" => crate::selection::bounds(&self.points).is_ok_and(|[l, t, r, b]| {
                    l < doc.width() as i32 && t < doc.height() as i32 && r > 0 && b > 0
                }),
                _ => self.points.last().is_some_and(|end| (0..2).all(|axis| {
                    start[axis] != end[axis] && start[axis].min(end[axis]) < size[axis]
                        && start[axis].max(end[axis]) > 0.
                })),
            };
            let commit = drag_finished;
            let preview = !drag_finished && self.transient.is_empty() && !self.edit_worker.pending();
            if ready && (commit || preview) && !self.busy && !doc.read_only() {
                let source = self.selection_gesture_source.as_mut().unwrap();
                if source.preview_charge.is_none() {
                    let bytes = if quick || self.selection_kind == "lasso" { MAX_BRUSH_SEEDS * 256 } else { 4096 };
                    match doc.native().map(|native| crate::psd::memory36::MetadataCharge::reserve(
                        native.budget(), bytes, &|| Ok(()),
                    )).transpose() {
                        Ok(charge) => source.preview_charge = charge,
                        Err(error) => self.message = error.into(),
                    }
                }
                if doc.native().is_none() || source.preview_charge.is_some() {
                    let command = self.select_command(doc, &self.points,
                        self.selection_gesture_mode.as_deref().unwrap_or(&self.selection_mode));
                    if commit {
                        self.apply_action(vec![command], "Select region", Some((doc.id(), doc.revision())),
                            doc.native().map_or(edit_job::Origin::Action, |source| {
                                edit_job::Origin::CanvasGesture(source.key())
                            }));
                    } else {
                        self.transient.push(command);
                    }
                }
            }
        }
        if quick && self.drag_start.is_some() {
            if let Some(press) = self.selection_gesture_source.as_ref().and_then(|source| source.press) {
                painter.circle_stroke(
                    to_screen(press.last_sample), self.selection_width * projection.scale(),
                    Stroke::new(1., ACCENT),
                );
            }
        } else if let (Some(a), Some(b)) = (self.drag_start, self.points.last().copied()) {
            if self.selection_kind == "lasso" {
                polygon(ui, painter, self.points.iter().copied().map(to_screen));
            } else if self.selection_kind == "ellipse" {
                let r = Rect::from_two_pos(Pos2::new(a[0], a[1]), Pos2::new(b[0], b[1]));
                let points = (0..64).map(|i| {
                    let angle = i as f32 * std::f32::consts::TAU / 64.;
                    let p = r.center()
                        + Vec2::new(angle.cos() * r.width() / 2., angle.sin() * r.height() / 2.);
                    to_screen([p.x, p.y])
                });
                polygon(ui, painter, points);
            } else {
                polygon(
                    ui,
                    painter,
                    [[a[0], a[1]], [b[0], a[1]], [b[0], b[1]], [a[0], b[1]]].into_iter().map(to_screen),
                );
            }
        }
        if drag_finished {
            self.cancel_selection_draft(primary_down);
            return;
        }
        // Sampling tools use the release location and modifiers for both clicks and drags.
        let sample = release.filter(|(pos, _)| {
            workspace_sample && self.selection_gesture_source.is_some()
                && click_reaches_image(*pos) && canvas_press_hits(ui, response, *pos)
        });
        if let Some((pos, modifiers)) = sample.or(click).filter(|_| !space) {
            if let Some(source) = self.selection_gesture_source.as_ref().filter(|_| click_reaches_image(pos)
                && ["wand", "quick", "row", "column"].contains(&self.selection_kind.as_str())
            ) {
                let mode = self.selection_mode_with(modifiers);
                let command = self.select_command(doc, &[to_doc(pos)], &mode);
                let native = source.native;
                self.apply_action(
                    vec![command],
                    "Select region",
                    Some((doc.id(), doc.revision())),
                    native.map_or(edit_job::Origin::Action, edit_job::Origin::CanvasGesture),
                );
            } else {
                self.deselect_area(doc, false);
            }
            self.selection_gesture_source = None;
        }
        if self.drag_start.is_none() && (release.is_some() || !primary_down) {
            self.selection_gesture_source = None;
        }
    }
}
impl PeerBrush {
    pub(super) fn paint_selection(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        doc: DocumentView<'_>,
        projection: &view::Projection,
    ) {
        let (bounds, polygon, contours) = self.selection_display(doc);
        if bounds.is_none() && contours.is_none() {
            return;
        }
        let phase = outline_phase(ui);
        let project = |p| projection.point(p);
        let visible = match (bounds, polygon, contours) {
            (_, _, Some(contours)) => {
                let mut visible = false;
                for contour in contours {
                    visible |= paint_polygon(painter, contour.iter().copied().map(&project), phase);
                }
                visible
            }
            (Some(_), Some(polygon), None) => {
                paint_polygon(painter, polygon.iter().copied().map(project), phase)
            }
            (Some(bounds), None, None) => {
                paint_polygon(painter, projection.region_corners(bounds).into_iter(), phase)
            }
            (None, _, None) => false,
        };
        if visible {
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
    }
}

fn polygon(ui: &egui::Ui, painter: &egui::Painter, points: impl ExactSizeIterator<Item = Pos2>) {
    if paint_polygon(painter, points, outline_phase(ui)) {
        ui.ctx().request_repaint_after(Duration::from_millis(60));
    }
}

fn outline_phase(ui: &egui::Ui) -> f32 {
    ui.input(|i| (i.time * 12.0 % 8.0) as f32)
}

fn paint_polygon(
    painter: &egui::Painter,
    mut points: impl ExactSizeIterator<Item = Pos2>,
    phase: f32,
) -> bool {
    let clip = painter.clip_rect();
    if points.len() < 3 || !clip.is_positive() {
        return false;
    }
    let first = points.next().unwrap();
    let mut previous = first;
    let mut distance = 0.0;
    let mut visible = false;
    for b in points.chain(std::iter::once(first)) {
        let a = std::mem::replace(&mut previous, b);
        let length = a.distance(b);
        if length < 0.1 {
            continue;
        }
        let offset = distance + phase;
        distance = (distance + length) % 8.0;
        if !clip.intersects(Rect::from_two_pos(a, b)) {
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
            visible = true;
            painter.line_segment(
                [a + direction * lo, a + direction * hi],
                Stroke::new(1.5_f32, Color32::BLACK),
            );
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
    }
    visible
}

#[cfg(test)]
mod outline_tests {
    use super::*;

    #[test]
    fn quick_selection_samples_only_the_image_and_keeps_the_release_seed() {
        let mut seeds = Vec::new();
        let size = [32., 24.];
        quick_segment(&mut seeds, [-30_000., -2.], [30_000., -2.], size, 1., false);
        assert!(seeds.is_empty());
        quick_segment(&mut seeds, [-30_000., 11.], [30_000., 11.], size, 1., false);
        assert_eq!(seeds.first().unwrap().map(f32::floor), [0., 11.]);
        assert_eq!(seeds.last().unwrap().map(f32::floor), [31., 11.]);
        assert!(seeds.len() <= 32);

        seeds.resize(MAX_BRUSH_SEEDS - 1, [0., 0.]);
        quick_segment(&mut seeds, [0., 11.], [30_000., 11.], size, 1., true);
        assert_eq!(seeds.len(), MAX_BRUSH_SEEDS);
        assert_eq!(seeds.last().unwrap().map(f32::floor), [31., 11.]);
    }

    #[test]
    fn selection_tool_popup_accepts_a_click_on_the_tool_glyph() {
        use egui::accesskit::{Action, ActionRequest, Role};

        let (mut app, ctx) = crate::ui::workspace_tests::fixture(16);
        ctx.enable_accesskit();
        let doc = app.shared.lock().unwrap().resident_state().unwrap().document.clone();
        let draw = |app: &mut PeerBrush, events| {
            ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200., 800.))),
                events,
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.selection_toolbar(ui, &doc));
            }).platform_output.accesskit_update.unwrap()
        };
        let tree = draw(&mut app, vec![]);
        let (combo, _) = tree.nodes.iter().find(|(_, node)| node.role() == Role::ComboBox).unwrap();
        draw(&mut app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Click, target: *combo, data: None,
        })]);
        let tree = draw(&mut app, vec![]);
        let (_, choice) = tree.nodes.iter().find(|(_, node)| node.label() == Some("Elliptical marquee")).unwrap();
        assert_eq!(choice.role(), Role::RadioButton);
        let bounds = choice.bounds().unwrap();
        let pos = egui::pos2(bounds.x0 as f32 + 15., ((bounds.y0 + bounds.y1) / 2.) as f32);
        for pressed in [true, false] {
            draw(&mut app, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton {
                pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default(),
            }]);
        }
        assert_eq!(app.selection_kind, "ellipse");
    }

    #[test]
    fn outline_projects_once_and_keeps_dash_phase_across_clipped_edges() {
        let ctx = egui::Context::default();
        let clip = Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.0));
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background()).with_clip_rect(clip);
            let mut projected = 0;
            let polygon = [[-11., 1.], [-11., 4.], [8., 4.], [8., -10.]];
            assert!(paint_polygon(
                &painter,
                polygon.into_iter().map(|[x, y]| {
                    projected += 1;
                    egui::pos2(x, y)
                }),
                0.0,
            ));
            assert_eq!(projected, polygon.len());
            let outside = polygon.map(|[x, y]| egui::pos2(x - 30., y));
            assert!(!paint_polygon(&painter, outside.into_iter(), 0.0));
        });
        let lines = output.shapes.iter().filter_map(|clipped| match &clipped.shape {
            egui::Shape::LineSegment { points, stroke } => Some((*points, stroke.color)),
            _ => None,
        });
        assert_eq!(
            lines.take(2).collect::<Vec<_>>(),
            [
                ([egui::pos2(0., 4.), egui::pos2(8., 4.)], Color32::BLACK),
                ([egui::pos2(2., 4.), egui::pos2(6., 4.)], Color32::WHITE),
            ]
        );
    }
}
