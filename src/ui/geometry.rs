use super::*;
use crate::raster::CanvasLimits;

pub(super) fn canvas_size<'a>(
    mut size: [u32; 2],
    commands: impl IntoIterator<Item = &'a Value>,
    limits: CanvasLimits,
) -> Result<[u32; 2], String> {
    for command in commands {
        if matches!(command["op"].as_str(), Some("crop" | "canvas.resize" | "image.resize" | "resize")) {
            let frame = crate::geometry::input::Frame::new(size, command, limits)?;
            size = [frame.width, frame.height];
        }
    }
    Ok(size)
}

pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
    native: Option<crate::engine::native::DocumentKey>,
    pub mode: &'static str,
    pub width: u32,
    pub height: u32,
    pub rect: [i32; 4],
    pub anchor: String,
    pub proportional: bool,
    original: [u32; 2],
    pub error: Option<String>,
    apply_error: Option<String>,
    token: egui::Id,
    pending: bool,
    crop_drag: Option<CropDrag>,
    crop_drag_cancelled: bool,
}

struct CropDrag {
    original: [i32; 4],
    edges: Option<[Option<usize>; 2]>,
    press: Pos2,
    canvas: Rect,
    scale: f32,
    gesture: String,
}

const CROP_HANDLES: [([f32; 2], [Option<usize>; 2], &str); 8] = [
    ([0., 0.], [Some(0), Some(1)], "Crop top left"),
    ([0.5, 0.], [None, Some(1)], "Crop top"),
    ([1., 0.], [Some(2), Some(1)], "Crop top right"),
    ([1., 0.5], [Some(2), None], "Crop right"),
    ([1., 1.], [Some(2), Some(3)], "Crop bottom right"),
    ([0.5, 1.], [None, Some(3)], "Crop bottom"),
    ([0., 1.], [Some(0), Some(3)], "Crop bottom left"),
    ([0., 0.5], [Some(0), None], "Crop left"),
];

impl CropDrag {
    fn bounds(&self, view: view::View, pos: Pos2, edge: u32) -> Option<[i32; 4]> {
        let Some(edges) = self.edges else {
            if self.press.distance(pos) <= 3. {
                return None;
            }
            let projection = view.projection(self.canvas, self.scale);
            let [x, y] = projection.document(self.press);
            let [end_x, end_y] = projection.document(pos);
            if x == end_x || y == end_y {
                return None;
            }
            return Some([
                (x.min(end_x).floor() + self.original[0] as f32) as i32,
                (y.min(end_y).floor() + self.original[1] as f32) as i32,
                (x.max(end_x).ceil() + self.original[0] as f32) as i32,
                (y.max(end_y).ceil() + self.original[1] as f32) as i32,
            ].map(|value| value.clamp(-100000, 100000)));
        };
        let mut bounds = self.original;
        let delta = view.inverse(pos - self.press) / self.scale;
        for (axis, offset) in [delta.x, delta.y].into_iter().enumerate() {
            if let Some(index) = edges[axis] {
                let other = self.original[(index + 2) % 4];
                let (min, max) = if index < 2 {
                    ((other - edge as i32).max(-100000), other - 1)
                } else {
                    (other + 1, (other + edge as i32).min(100000))
                };
                bounds[index] = self.original[index]
                    .saturating_add(offset.round() as i32)
                    .clamp(min, max);
            }
        }
        Some(bounds)
    }

    fn view(&self, view: view::View, bounds: [i32; 4]) -> (Rect, f32) {
        let delta = |start: usize, end: usize| {
            (bounds[start] + bounds[end] - self.original[start] - self.original[end]) as f32 * 0.5
        };
        let offset = view.vector(Vec2::new(delta(0, 2), delta(1, 3))) * self.scale;
        let size = Vec2::new((bounds[2] - bounds[0]) as f32, (bounds[3] - bounds[1]) as f32)
            * self.scale;
        (Rect::from_center_size(self.canvas.center() + offset, size), self.scale)
    }
}
impl Editor {
    pub(super) fn observes(&self, doc: DocumentView<'_>) -> bool {
        self.document == doc.id() && self.revision == doc.revision()
            && self.native == doc.native().map(|native| native.key())
    }

    pub(super) fn command(&self) -> Value {
        json!({"op":self.mode,"width":self.width,"height":self.height,"rect":if self.mode=="crop"{Some(self.rect)}else{None},"anchor":self.anchor,"document_id":self.document,"source_revision":self.revision})
    }
    pub(super) fn size(&self) -> [u32; 2] {
        if self.mode == "crop" {
            [
                (self.rect[2] - self.rect[0]).max(1) as u32,
                (self.rect[3] - self.rect[1]).max(1) as u32,
            ]
        } else {
            [self.width, self.height]
        }
    }

    fn keep_image_proportions(&mut self, axis: usize, edge: u32) {
        if self.mode != "image.resize" || !self.proportional {
            return;
        }
        let other = 1 - axis;
        let mut size = [self.width, self.height];
        let ratio = f64::from(self.original[other]) / f64::from(self.original[axis]);
        if (f64::from(size[axis]) * ratio).round() > f64::from(edge) {
            size[axis] = (f64::from(edge) / ratio).floor().max(1.) as u32;
        }
        size[other] = (f64::from(size[axis]) * ratio).round().clamp(1., f64::from(edge)) as u32;
        [self.width, self.height] = size;
    }

    fn resize_summary(&self, ui: &mut egui::Ui, frame: crate::geometry::input::Frame) {
        let size = |[w, h]: [u32; 2]| Vec2::new(w as f32, h as f32);
        let output = size([frame.width, frame.height]);
        let original = size(self.original);
        let source = Rect::from_min_size(Pos2::new(frame.offset[0] as f32, frame.offset[1] as f32), original);
        let boundary = Rect::from_min_size(Pos2::ZERO, output);
        let union = boundary.union(source);
        let (space, summary) = ui.allocate_exact_size(Vec2::new(124., 92.), egui::Sense::hover());
        summary.on_hover_text("Gray outline: original frame. Orange outline: new frame.");
        let fit = space.shrink(6.);
        let scale = (fit.width() / union.width()).min(fit.height() / union.height());
        let project = |rect: Rect| Rect::from_min_max(
            fit.center() + (rect.min - union.center()) * scale,
            fit.center() + (rect.max - union.center()) * scale,
        );
        let result = project(boundary);
        let kept = if frame.image { result } else { project(source).intersect(result) };
        let painter = ui.painter();
        painter.rect_filled(result, 0., Color32::from_gray(43));
        painter.rect_filled(kept, 0., Color32::from_rgb(83, 56, 67));
        if frame.image || source != boundary {
            painter.rect_stroke(project(source), 0., Stroke::new(1., MUTED), egui::StrokeKind::Inside);
        }
        painter.rect_stroke(result, 0., Stroke::new(1., ACCENT), egui::StrokeKind::Inside);
    }
}
impl PeerBrush {
    pub(super) fn open_geometry<'a>(
        &mut self,
        doc: impl Into<DocumentView<'a>>,
        mode: &'static str,
    ) {
        let doc = doc.into();
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; geometry draft preserved".into();
            return;
        }
        self.cancel_filters();
        self.cancel_source();
        self.cancel_refinement();
        self.points.clear();
        self.drag_start = None;
        self.gizmo_handle = 0;
        self.transient.clear();
        self.interrupt_animation();
        self.isolate = false;
        self.geometry = Some(Editor {
            document: doc.id().to_owned(),
            revision: doc.revision(),
            native: doc.native().map(|native| native.key()),
            mode,
            width: doc.width(),
            height: doc.height(),
            rect: doc
                .selection()
                .filter(|b| b[0] < b[2] && b[1] < b[3])
                .unwrap_or([0, 0, doc.width() as i32, doc.height() as i32]),
            anchor: "center".into(),
            proportional: true,
            original: [doc.width(), doc.height()],
            error: None,
            apply_error: None,
            token: egui::Id::new(crate::engine::id()),
            pending: false,
            crop_drag: None,
            crop_drag_cancelled: false,
        });
        self.last_preview = None;
    }
    pub(super) fn geometry_pending(&self) -> bool {
        self.geometry.as_ref().is_some_and(|editor| editor.pending)
    }

    pub(super) fn crop_dragging(&self) -> bool {
        self.crop_preview_key().is_some()
    }

    pub(super) fn crop_preview_key(&self) -> Option<&str> {
        self.geometry.as_ref()?.crop_drag.as_ref().map(|drag| drag.gesture.as_str())
    }

    pub(super) fn crop_canvas_input(
        &mut self,
        ui: &mut egui::Ui,
        doc: DocumentView<'_>,
        response: &egui::Response,
        canvas: Rect,
        scale: f32,
    ) -> (Rect, f32) {
        let Some(editor) = self.geometry.as_mut().filter(|editor| {
            editor.mode == "crop" && !editor.pending && editor.observes(doc) && !doc.read_only()
        }) else { return (canvas, scale) };
        let (press, release, pointer, interrupted, escape) = ui.input(|input| {
            let (press, release) = canvas_button_samples(input, editor.crop_drag.is_some());
            let escape = input.key_pressed(egui::Key::Escape);
            let interrupted = escape || !input.focused
                || input.events.iter().any(|e| matches!(e, egui::Event::WindowFocused(false)))
                || (editor.crop_drag.is_some() && release.is_none() && !input.pointer.primary_down());
            (press, release, input.pointer.interact_pos(), interrupted, escape)
        });
        if interrupted || Self::pointer_contact_cancelled(ui.ctx()) || !ui.is_enabled() {
            if let Some(drag) = editor.crop_drag.take() {
                editor.rect = drag.original;
                editor.error = None;
                editor.apply_error = None;
                editor.crop_drag_cancelled = escape;
                self.last_preview = None;
                ui.ctx().request_repaint();
                return (drag.canvas, drag.scale);
            }
            return (canvas, scale);
        }
        let limits = if doc.native().is_some() {
            CanvasLimits::Native
        } else {
            CanvasLimits::Resident
        };
        let view = self.view;
        if let Some(press) = press.filter(|&pos| canvas_press_hits(ui, response, pos)) {
            let [width, height] = editor.size();
            if editor.rect[0] < editor.rect[2] && editor.rect[1] < editor.rect[3]
                && limits.check(width, height).is_ok()
            {
                let projection = view.projection(canvas, scale);
                let edges = CROP_HANDLES.iter().filter_map(|&([x, y], edges, _)| {
                    let center = projection.point([x * width as f32, y * height as f32]);
                    let distance = center.distance_sq(press);
                    Rect::from_center_size(center, Vec2::splat(16.))
                        .contains(press).then_some((distance, edges))
                }).min_by(|(a, _), (b, _)| a.total_cmp(b)).map(|(_, edges)| edges);
                editor.crop_drag = Some(CropDrag {
                    original: editor.rect, edges, press, canvas, scale, gesture: crate::engine::id(),
                });
            }
        }
        let Some(drag) = &editor.crop_drag else { return (canvas, scale) };
        if let Some(pos) = release.map(|(pos, _)| pos).or(pointer) {
            let next = drag.bounds(view, pos, limits.edge()).filter(|[l, t, r, b]| {
                limits.check((r - l) as u32, (b - t) as u32).is_ok()
            });
            let next = next.unwrap_or(if drag.edges.is_none() { drag.original } else { editor.rect });
            if next != editor.rect {
                editor.rect = next;
                editor.error = None;
                editor.apply_error = None;
            }
        }
        if release.is_some() {
            editor.crop_drag = None;
            // Capture the settled render with the same framing as the next frame.
            ui.ctx().request_repaint();
        } else {
            return drag.view(view, editor.rect);
        }
        let size = editor.size();
        let scale = view.fit_scale(response.rect, size) * self.zoom;
        let size = Vec2::new(size[0] as f32, size[1] as f32) * scale;
        (Rect::from_center_size(canvas.center(), size), scale)
    }

    pub(super) fn paint_crop_handles(
        &self, ui: &egui::Ui, doc: DocumentView<'_>, painter: &egui::Painter, projection: &view::Projection,
    ) {
        let Some(editor) = self.geometry.as_ref().filter(|editor| {
            editor.mode == "crop" && !editor.pending && editor.observes(doc)
                && editor.rect[0] < editor.rect[2] && editor.rect[1] < editor.rect[3]
        }) else {
            return;
        };
        let [width, height] = editor.size();
        for ([x, y], _, label) in CROP_HANDLES {
            let point = projection.point([x * width as f32, y * height as f32]);
            let hit = Rect::from_center_size(point, Vec2::splat(16.));
            ui.interact(hit, ui.id().with((editor.token, label)), egui::Sense::hover())
                .on_hover_cursor(egui::CursorIcon::Grab).on_hover_text(label)
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, ui.is_enabled(), label));
            painter.rect_filled(hit.shrink(4.), 1., ACCENT);
        }
        if editor.crop_drag.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
        }
    }
    fn apply_geometry(&mut self) {
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; geometry draft preserved".into();
            return;
        }
        let Some(editor) = self.geometry.as_mut() else {
            return;
        };
        let command = editor.command();
        let document = editor.document.clone();
        let revision = editor.revision;
        let token = editor.token;
        let native = editor.native;
        let label = match editor.mode {
            "crop" => "Crop canvas",
            "canvas.resize" => "Resize canvas",
            _ => "Resize image",
        };
        editor.apply_error = None;
        editor.pending = true;
        self.apply_action(
            vec![command],
            label,
            Some((&document, revision)),
            edit_job::Origin::Geometry(token, native),
        );
    }
    pub(super) fn geometry_finished(&mut self, token: egui::Id, result: Result<(), String>) {
        let Some(editor) = self
            .geometry
            .as_mut()
            .filter(|editor| editor.pending && editor.token == token)
        else {
            return;
        };
        editor.pending = false;
        match result {
            Ok(()) => {
                self.cancel_geometry();
                self.frame_pending = true;
            }
            Err(error) => editor.apply_error = Some(error),
        }
    }
    pub(super) fn cancel_geometry(&mut self) {
        if self.geometry_pending() {
            self.cancel_pending_edit();
            return;
        }
        self.geometry = None;
        self.transient.retain(|c| {
            !matches!(
                c["op"].as_str(),
                Some("crop" | "canvas.resize" | "image.resize")
            )
        });
        self.last_preview = None;
    }
    pub(super) fn geometry_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        if self.geometry_pending() {
            egui::Window::new("Applying geometry")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Applying canvas changes…");
                    if ui.button("Cancel apply").clicked() {
                        self.cancel_pending_edit();
                    }
                });
            return;
        }
        let Some(mut editor) = self.geometry.take() else {
            return;
        };
        if !editor.observes(doc) {
            self.cancel_geometry();
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let before = editor.command();
        let limits = if matches!(doc, DocumentView::Native(_)) {
            CanvasLimits::Native
        } else {
            CanvasLimits::Resident
        };
        let edge = limits.edge();
        egui::Window::new(match editor.mode {
            "crop" => "Crop",
            "canvas.resize" => "Canvas size",
            _ => "Image size",
        })
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(295.)
        .show(ctx, |ui| {
            if editor.crop_drag.is_some() {
                ui.disable();
            }
            if editor.mode == "crop" {
                for (i, label) in ["Left", "Top", "Right", "Bottom"].iter().enumerate() {
                    ui.horizontal(|ui| {
                        let name = ui.add_sized([70., 20.], egui::Label::new(*label));
                        controls::numeric(ui, &mut editor.rect[i], -100000..=100000)
                            .labelled_by(name.id);
                        ui.label("px");
                    });
                }
                if ui
                    .add_enabled(
                        doc.selection().is_some(),
                        egui::Button::new("Use selection bounds"),
                    )
                    .clicked()
                {
                    editor.rect = doc.selection().unwrap();
                }
                ui.label("Drag to draw a crop box, or adjust its handles. Artwork outside the crop is retained.");
            } else {
                ui.horizontal(|ui| {
                    let label = ui.label("Width");
                    if controls::numeric(ui, &mut editor.width, 1..=edge).labelled_by(label.id).changed() {
                        editor.keep_image_proportions(0, edge);
                    }
                    ui.label("px");
                    if editor.mode == "image.resize" {
                        icons::toggle(
                            ui,
                            Icon::Connect,
                            &mut editor.proportional,
                            "Keep width and height proportional",
                        );
                    }
                });
                ui.horizontal(|ui| {
                    let label = ui.label("Height");
                    if controls::numeric(ui, &mut editor.height, 1..=edge).labelled_by(label.id).changed() {
                        editor.keep_image_proportions(1, edge);
                    }
                    ui.label("px");
                });
                if editor.mode == "canvas.resize" {
                    let anchors = [
                        "top_left", "top", "top_right", "left", "center", "right",
                        "bottom_left", "bottom", "bottom_right",
                    ];
                    let frame = ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label("Anchor");
                            let selected = anchors.iter().position(|&anchor| anchor == editor.anchor);
                            if let Some(index) = controls::anchor_grid(ui, "canvas anchors", selected, "") {
                                editor.anchor = anchors[index].into();
                            }
                        });
                        let frame = crate::geometry::input::Frame::new(editor.original, &editor.command(), limits).ok();
                        if let Some(frame) = frame {
                            editor.resize_summary(ui, frame);
                        }
                        frame
                    }).inner;
                    if let Some(frame) = frame {
                        let [x, y] = frame.offset;
                        ui.label(format!("Artwork offset  {x:+} px X  {y:+} px Y"));
                    }
                } else if let Ok(frame) = crate::geometry::input::Frame::new(editor.original, &editor.command(), limits) {
                    let [x, y] = frame.scale;
                    ui.horizontal(|ui| {
                        editor.resize_summary(ui, frame);
                        ui.vertical(|ui| {
                            ui.label(format!("Width  {:.1}%", x * 100.));
                            ui.label(format!("Height  {:.1}%", y * 100.));
                            ui.label("Resamples layers and masks");
                        });
                    });
                }
            }
            let [w, h] = editor.size();
            ui.label(format!("Result {w} × {h} · {} bit", doc.depth()));
            let invalid = limits.check(w, h).err().or_else(|| {
                (editor.mode == "crop"
                    && (editor.rect[0] >= editor.rect[2] || editor.rect[1] >= editor.rect[3]))
                    .then(|| "Choose a positive crop rectangle".into())
            });
            if let Some(error) = invalid
                .as_ref()
                .or(editor.error.as_ref())
                .or(editor.apply_error.as_ref())
            {
                ui.colored_label(ACCENT, error);
            }
            ui.horizontal(|ui| {
                apply = ui
                    .add_enabled(
                        !self.busy && invalid.is_none() && editor.error.is_none(),
                        egui::Button::new("Apply"),
                    )
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        let command = editor.command();
        if command != before {
            editor.error = None;
            editor.apply_error = None;
        }
        let cancelled_drag = std::mem::take(&mut editor.crop_drag_cancelled);
        if !open || cancel || (!cancelled_drag && ctx.input(|i| i.key_pressed(egui::Key::Escape))) {
            self.cancel_geometry();
        } else if apply {
            self.geometry = Some(editor);
            self.apply_geometry();
        } else {
            self.transient = vec![command];
            self.geometry = Some(editor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{text_tests, workspace_tests as workspace};
    use super::*;

    #[test]
    fn crop_drag_keeps_the_opposite_edge_fixed_in_a_rotated_and_mirrored_view() {
        let view = view::View { angle: 45., mirror_h: true, mirror_v: false };
        let original = [10, 20, 110, 100];
        let scale = 2.;
        let canvas = Rect::from_min_size(Pos2::new(25., 60.), Vec2::new(200., 160.));
        let projection = view.projection(canvas, scale);
        let press = projection.point([100., 80.]);
        let drag = CropDrag {
            original, edges: Some([Some(2), Some(3)]), press, canvas, scale, gesture: String::new(),
        };
        let end = press + view.vector(Vec2::new(-35., 10.)) * scale;
        let bounds = drag.bounds(view, end, CanvasLimits::Resident.edge()).unwrap();
        assert_eq!(bounds, [10, 20, 75, 110]);
        let (canvas, scale) = drag.view(view, bounds);
        let result = view.projection(canvas, scale);
        assert!(result.point([0., 0.]).distance(projection.point([0., 0.])) < 0.001);
        assert!(result.point([65., 90.]).distance(end) < 0.001);

        let press = projection.point([-8., -6.]);
        let drag = CropDrag { original, edges: None, press, canvas: drag.canvas, scale, gesture: String::new() };
        let end = projection.point([27.25, 38.25]);
        let bounds = drag.bounds(view, end, CanvasLimits::Resident.edge()).unwrap();
        assert_eq!(bounds, [2, 14, 38, 59]);
        let (redrawn, _) = drag.view(view, bounds);
        assert!(view.point([0., 0.], redrawn, scale).distance(press) < 0.001);
        assert_eq!(drag.bounds(view, press, CanvasLimits::Resident.edge()), None);
    }

    #[test]
    fn accessible_canvas_anchor_changes_the_preview_without_committing() {
        use egui::accesskit::{Action, ActionRequest, Role, Toggled};

        let (mut app, ctx) = workspace::fixture(16);
        ctx.enable_accesskit();
        let (doc, history) = {
            let engine = app.shared.lock().unwrap();
            (engine.resident_state().unwrap().document.clone(), engine.history_lengths())
        };
        app.open_geometry(&doc, "canvas.resize");
        let draw = |app: &mut PeerBrush, events| {
            ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000., 700.))),
                events,
                ..Default::default()
            }, |ctx| app.geometry_window(ctx, &doc))
                .platform_output.accesskit_update.unwrap()
        };
        let tree = draw(&mut app, vec![]);
        let (target, node) = tree.nodes.iter()
            .find(|(_, node)| node.label() == Some("Bottom right"))
            .unwrap();
        assert_eq!(node.role(), Role::RadioButton);
        assert_eq!(node.toggled(), Some(Toggled::False));
        assert!(node.supports_action(Action::Click));
        draw(&mut app, vec![egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::Click, target: *target, data: None,
        })]);
        assert_eq!(app.geometry.as_ref().unwrap().anchor, "bottom_right");
        assert_eq!(app.transient[0]["anchor"], "bottom_right");
        assert_eq!(app.shared.lock().unwrap().history_lengths(), history);
    }

    #[test]
    fn geometry_draft_refuses_a_replacement_native_generation_at_the_same_revision() {
        use crate::engine::{authority::DocumentAuthority, native::NativeDocument};

        let (mut app, _) = workspace::fixture(16);
        let original = Document::new_depth(32, 24, 16).unwrap();
        let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let make = || -> NativeDocument { crate::tree::presentation_tests::native(&original, &pool) };
        let first = make();
        let replacement = make();
        assert_ne!(first.key(), replacement.key());
        app.doc_snapshot = first.clone().into();
        app.open_geometry(&first, "canvas.resize");
        app.geometry.as_mut().unwrap().width += 1;
        let history = app.shared.lock().unwrap().history_lengths();
        app.shared.lock().unwrap().authority = DocumentAuthority::from_native(replacement.clone(), 4).unwrap();
        assert!(!app.geometry.as_ref().unwrap().observes((&replacement).into()));
        assert_eq!(app.displayed_canvas_size((&replacement).into()), [original.width, original.height]);

        // A frame already displaying the replacement cannot submit the older draft.
        app.doc_snapshot = replacement.clone().into();
        app.apply_geometry();
        assert!(!app.edit_worker.pending());
        assert!(app.geometry.as_ref().unwrap().apply_error.is_some());
        let engine = app.shared.lock().unwrap();
        assert_eq!(engine.history_lengths(), history);
        assert_eq!(engine.native_document().unwrap().key(), replacement.key());
    }

    #[test]
    fn geometry_apply_keeps_drafts_and_frame_state_until_guarded_completion() {
        for mode in ["crop", "canvas.resize", "image.resize"] {
            for outcome in ["immediate", "commit", "cancel", "reserved", "replacement"] {
                let (mut app, ctx) = workspace::fixture(16);
                let doc = app
                    .shared
                    .lock()
                    .unwrap()
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .clone();
                app.open_geometry(&doc, mode);
                let editor = app.geometry.as_mut().unwrap();
                editor.width = 16;
                editor.height = 12;
                editor.rect = [2, 3, 20, 18];
                let command = editor.command();
                let token = editor.token;
                let expected =
                    crate::engine::Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
                let shared = app.shared.clone();
                if outcome == "reserved" {
                    shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "agent",
                            "Protected geometry",
                            vec![crate::engine::Scope::layer(&doc.layers[0].id)],
                        )
                        .unwrap();
                }
                let mut guard = shared.lock().unwrap();
                if outcome == "replacement" {
                    guard
                        .resident_state_mut()
                        .expect("resident fixture")
                        .document
                        .id = crate::engine::id();
                }
                let before = serde_json::to_value(
                    &guard.resident_state().expect("resident fixture").document,
                )
                .unwrap();
                let history = guard.history_lengths();
                app.frame_pending = false;
                if outcome == "immediate" {
                    drop(guard);
                    app.apply_geometry();
                } else {
                    app.geometry.as_mut().unwrap().pending = true;
                    app.submit_edit_job(
                        edit_job::Request {
                            project: app.project_id.clone(),
                            document: doc.id.clone(),
                            revision: doc.revision,
                            label: "Geometry".into(),
                            selection_edit: false,
                            origin: edit_job::Origin::Geometry(token, None),
                        },
                        edit_job::Commands::resident(vec![command.clone()]),
                        &ctx,
                    )
                    .unwrap();
                    app.geometry_finished(egui::Id::new("other geometry"), Ok(()));
                    app.open_geometry(&doc, "crop");
                    app.apply_geometry();
                    assert!(app.geometry_pending());
                    assert_eq!(app.geometry.as_ref().unwrap().command(), command);
                    assert!(!app.frame_pending);
                    if outcome == "cancel" {
                        app.cancel_geometry();
                        assert!(app.geometry_pending());
                    }
                    drop(guard);
                    text_tests::wait_edit(&mut app);
                }
                let engine = shared.lock().unwrap();
                if matches!(outcome, "immediate" | "commit") {
                    assert!(app.geometry.is_none());
                    assert!(app.frame_pending);
                    assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
                    assert_eq!(
                        [
                            engine
                                .resident_state()
                                .expect("resident fixture")
                                .document
                                .width,
                            engine
                                .resident_state()
                                .expect("resident fixture")
                                .document
                                .height
                        ],
                        [expected.width, expected.height]
                    );
                    assert_eq!(
                        serde_json::to_value(
                            &engine
                                .resident_state()
                                .expect("resident fixture")
                                .document
                                .layers
                        )
                        .unwrap(),
                        serde_json::to_value(&expected.layers).unwrap()
                    );
                    assert_eq!(
                        engine
                            .resident_state()
                            .expect("resident fixture")
                            .document
                            .bit_depth,
                        16
                    );
                } else {
                    let editor = app.geometry.as_ref().unwrap();
                    assert!(!editor.pending);
                    assert!(editor.apply_error.is_some());
                    assert_eq!(editor.command(), command);
                    assert!(!app.frame_pending);
                    assert_eq!(engine.history_lengths(), history);
                    assert_eq!(
                        serde_json::to_value(
                            &engine.resident_state().expect("resident fixture").document
                        )
                        .unwrap(),
                        before
                    );
                    assert!(editor.error.is_none());
                    drop(engine);
                    if outcome == "cancel" {
                        app.apply_geometry();
                        assert!(
                            app.geometry.is_none(),
                            "An unchanged cancelled draft can be retried"
                        );
                        assert_eq!(
                            shared.lock().unwrap().history_lengths(),
                            [history[0] + 1, 0]
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn pending_editor_remains_visible_through_lifecycle_gate() {
        let (mut app, ctx) = workspace::fixture(16);
        let doc = app
            .shared
            .lock()
            .unwrap()
            .resident_state()
            .expect("resident fixture")
            .document
            .clone();
        app.open_geometry(&doc, "canvas.resize");
        let editor = app.geometry.as_mut().unwrap();
        editor.pending = true;
        let token = editor.token;
        let (release, held) = mpsc::channel();
        app.edit_worker
            .submit(
                edit_job::Request {
                    project: app.project_id.clone(),
                    document: doc.id.clone(),
                    revision: doc.revision,
                    label: "Apply editor".into(),
                    selection_edit: false,
                    origin: edit_job::Origin::Geometry(token, None),
                },
                edit_job::Commands::resident(vec![]),
                move |_, _| {
                    held.recv().unwrap();
                    Err("Apply cancelled".into())
                },
                || {},
            )
            .unwrap();
        let output = workspace::draw(&mut app, &ctx, Vec2::new(1360., 900.), vec![]);
        workspace::label(&output, "Applying canvas changes…");
        workspace::label(&output, "Cancel apply");
        assert!(app.geometry_pending());
        release.send(()).unwrap();
        text_tests::wait_edit(&mut app);
        assert!(app.geometry.is_some());
        assert!(!app.geometry_pending());
    }
}
