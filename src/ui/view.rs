use super::*;

pub(super) fn zoom_limits(fit: f32, pixels_per_point: f32) -> std::ops::RangeInclusive<f32> {
    // Keep actual-size zoom available even when a tiny canvas has a very large fit scale.
    0.01_f32.min(1. / (fit * pixels_per_point))..=8192.
}

/// Presentation only. Artwork, shared previews, history and saves use document coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct View {
    pub angle: f32,
    pub mirror_h: bool,
    pub mirror_v: bool,
}

pub(super) struct Projection {
    view: View,
    basis: Option<(f32, f32)>,
    rect: Rect,
    scale: f32,
    center: Pos2,
    half: Vec2,
}

impl View {
    fn basis(self) -> (f32, f32) {
        let (sin, cos) = self.angle.to_radians().sin_cos();
        let exact = |v: f32| {
            if v.abs() < 1e-6 {
                0.
            } else if (v.abs() - 1.).abs() < 1e-6 {
                v.signum()
            } else {
                v
            }
        };
        (exact(sin), exact(cos))
    }
    pub fn vector(self, v: Vec2) -> Vec2 {
        self.rotate(v, self.basis())
    }
    fn rotate(self, v: Vec2, (sin, cos): (f32, f32)) -> Vec2 {
        let x = v.x * cos - v.y * sin;
        let y = v.x * sin + v.y * cos;
        Vec2::new(
            if self.mirror_h { -x } else { x },
            if self.mirror_v { -y } else { y },
        )
    }
    pub fn inverse(self, v: Vec2) -> Vec2 {
        self.unrotate(v, self.basis())
    }
    fn unrotate(self, v: Vec2, (sin, cos): (f32, f32)) -> Vec2 {
        let x = if self.mirror_h { -v.x } else { v.x };
        let y = if self.mirror_v { -v.y } else { v.y };
        Vec2::new(x * cos + y * sin, -x * sin + y * cos)
    }
    pub fn extent(self, size: Vec2) -> Vec2 {
        let (sin, cos) = self.basis();
        Vec2::new(
            size.x * cos.abs() + size.y * sin.abs(),
            size.x * sin.abs() + size.y * cos.abs(),
        )
    }
    pub(super) fn fit_scale(self, viewport: Rect, [width, height]: [u32; 2]) -> f32 {
        let extent = self.extent(Vec2::new(width as f32, height as f32));
        ((viewport.width() - 60.) / extent.x)
            .min((viewport.height() - 60.) / extent.y)
            .max(0.01)
    }
    pub fn point(self, p: [f32; 2], rect: Rect, scale: f32) -> Pos2 {
        self.projection(rect, scale).point(p)
    }
    pub fn document(self, p: Pos2, rect: Rect, scale: f32) -> [f32; 2] {
        self.projection(rect, scale).document(p)
    }
    pub fn contains(self, p: Pos2, rect: Rect, scale: f32) -> bool {
        self.projection(rect, scale).contains(p)
    }
    pub(super) fn projection(self, rect: Rect, scale: f32) -> Projection {
        Projection {
            view: self,
            basis: (self != Self::default()).then(|| self.basis()),
            rect,
            scale,
            center: rect.center(),
            half: rect.size() * 0.5,
        }
    }
    pub(super) fn visible_region(
        self,
        canvas: Rect,
        viewport: Rect,
        size: [u32; 2],
    ) -> Option<[i32; 4]> {
        let scale = canvas.width() / size[0] as f32;
        if !scale.is_finite() || scale <= 0. || size.contains(&0) {
            return None;
        }
        let projection = self.projection(canvas, scale);
        let oblique = projection.basis.is_some_and(|(sin, cos)| sin != 0. && cos != 0.);
        let viewport = if oblique {
            // Keep distant viewport corners out of the document-space interpolation.
            let intersection = viewport.intersect(Rect::from_points(&projection.corners()));
            if intersection.is_positive() { intersection } else { viewport }
        } else {
            viewport
        };
        let corners = [viewport.left_top(), viewport.right_top(), viewport.right_bottom(), viewport.left_bottom()];
        let points = corners.map(|corner| projection.document(corner));
        let clipped = if oblique { clip_viewport(points, size) } else { None };
        let points = clipped.as_ref().map_or(&points[..], |(points, len)| &points[..*len]);
        let mut low = [f32::INFINITY; 2];
        let mut high = [f32::NEG_INFINITY; 2];
        for point in points {
            for axis in 0..2 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        // Retain neighboring samples for texture filtering at viewport edges.
        let region = [
            (low[0].floor() as i32).saturating_sub(2).clamp(0, size[0] as i32 - 1),
            (low[1].floor() as i32).saturating_sub(2).clamp(0, size[1] as i32 - 1),
            (high[0].ceil() as i32).saturating_add(2).clamp(1, size[0] as i32),
            (high[1].ceil() as i32).saturating_add(2).clamp(1, size[1] as i32),
        ];
        (region != [0, 0, size[0] as i32, size[1] as i32]
            && region[0] < region[2] && region[1] < region[3]).then_some(region)
    }
    pub fn corners(self, rect: Rect, scale: f32) -> [Pos2; 4] {
        self.projection(rect, scale).corners()
    }
    pub fn bounds(self, rect: Rect, scale: f32) -> Rect {
        Rect::from_points(&self.corners(rect, scale))
    }

    pub(super) fn overlaps_viewport(self, canvas: Rect, viewport: Rect) -> bool {
        let projection = self.projection(canvas, 1.);
        if !Rect::from_points(&projection.corners()).intersect(viewport).is_positive() {
            return false;
        }
        // Check both rectangles' axes. The screen-space bounding box alone
        // includes gray corners outside a rotated canvas.
        let Some(basis) = projection.basis else { return true };
        let in_canvas_axes = [viewport.left_top(), viewport.right_top(), viewport.right_bottom(), viewport.left_bottom()]
            .map(|point| canvas.center() + self.unrotate(point - canvas.center(), basis));
        Rect::from_points(&in_canvas_axes).intersect(canvas).is_positive()
    }
}

// Intersect the viewport polygon with the canvas before taking its bounding box.
// Intersecting two quadrilaterals yields at most eight vertices.
fn clip_viewport(corners: [[f32; 2]; 4], [width, height]: [u32; 2]) -> Option<([[f32; 2]; 8], usize)> {
    let mut points = [[0.; 2]; 8];
    let mut next = points;
    points[..4].copy_from_slice(&corners);
    let mut len = 4;
    for (axis, limit, sign) in [(0, 0., 1.), (0, width as f32, -1.), (1, 0., 1.), (1, height as f32, -1.)] {
        let mut count = 0;
        for i in 0..len {
            let p = points[i];
            let q = points[(i + 1) % len];
            let dp = (p[axis] - limit) * sign;
            let dq = (q[axis] - limit) * sign;
            if dp >= 0. {
                *next.get_mut(count)? = p;
                count += 1;
            }
            if (dp < 0. && dq > 0.) || (dp > 0. && dq < 0.) {
                let other = 1 - axis;
                let point = next.get_mut(count)?;
                point[axis] = limit;
                point[other] = p[other] + (q[other] - p[other]) * (dp / (dp - dq));
                count += 1;
            }
        }
        std::mem::swap(&mut points, &mut next);
        len = count;
    }
    (len != 0).then_some((points, len))
}

impl Projection {
    pub(super) fn scale(&self) -> f32 {
        self.scale
    }
    pub(super) fn vector(&self, v: Vec2) -> Vec2 {
        self.basis.map_or(v, |basis| self.view.rotate(v, basis))
    }
    pub(super) fn region_corners(&self, [left, top, right, bottom]: [i32; 4]) -> [Pos2; 4] {
        [[left, top], [right, top], [right, bottom], [left, bottom]]
            .map(|[x, y]| self.point([x as f32, y as f32]))
    }
    pub(super) fn point(&self, [x, y]: [f32; 2]) -> Pos2 {
        let offset = Vec2::new(x, y) * self.scale;
        match self.basis {
            None => self.rect.min + offset,
            Some(basis) => self.center + self.view.rotate(offset - self.half, basis),
        }
    }
    pub(super) fn document(&self, point: Pos2) -> [f32; 2] {
        let v = match self.basis {
            None => (point - self.rect.min) / self.scale,
            Some(basis) => (self.view.unrotate(point - self.center, basis) + self.half) / self.scale,
        };
        // Both paths snap the same roundoff near pixel boundaries.
        [v.x, v.y].map(|value| {
            if (value - value.round()).abs() < 0.001 { value.round() } else { value }
        })
    }
    pub(super) fn contains(&self, point: Pos2) -> bool {
        let [x, y] = self.document(point);
        x >= 0. && y >= 0. && x <= self.rect.width() / self.scale && y <= self.rect.height() / self.scale
    }
    pub(super) fn corners(&self) -> [Pos2; 4] {
        let w = self.rect.width() / self.scale;
        let h = self.rect.height() / self.scale;
        [[0., 0.], [w, 0.], [w, h], [0., h]].map(|p| self.point(p))
    }
}

pub(super) fn paint_checker(
    painter: &egui::Painter,
    positions: [Pos2; 4],
    texture: egui::TextureId,
) {
    let mut mesh = egui::epaint::Mesh::with_texture(texture);
    for pos in positions {
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv: Pos2::new(pos.x / 32., pos.y / 32.),
            color: Color32::WHITE,
        });
    }
    mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
    painter.add(egui::Shape::mesh(mesh));
}
pub(super) fn checker(ctx: &egui::Context) -> TextureHandle {
    let id = egui::Id::new("canvas view checker");
    if let Some(texture) = ctx.data(|d| d.get_temp::<TextureHandle>(id)) {
        return texture;
    }
    let pixels = (0..32 * 32)
        .map(|i| {
            if (i % 32 / 16 + i / 32 / 16) % 2 == 0 {
                Color32::from_rgb(66, 63, 68)
            } else {
                Color32::from_rgb(52, 50, 55)
            }
        })
        .collect();
    let texture = ctx.load_texture(
        "canvas checker",
        egui::ColorImage {
            size: [32, 32],
            pixels,
        },
        egui::TextureOptions {
            wrap_mode: egui::TextureWrapMode::Repeat,
            ..egui::TextureOptions::NEAREST
        },
    );
    ctx.data_mut(|d| d.insert_temp(id, texture.clone()));
    texture
}
impl PeerBrush {
    pub(super) fn displayed_canvas_size(&self, doc: DocumentView<'_>) -> [u32; 2] {
        self.geometry.as_ref().filter(|editor| editor.observes(doc)).map(|editor| editor.size())
            .or_else(|| self.native_proposal_size(doc))
            .unwrap_or([doc.width(), doc.height()])
    }

    pub(super) fn canvas_view_window(&mut self, ctx: &egui::Context, doc: DocumentView<'_>) {
        if !self.show_view {
            return;
        }
        let mut open = true;
        egui::Window::new("Canvas view")
            .open(&mut open)
            .resizable(false)
            .default_width(300.)
            .show(ctx, |ui| self.canvas_view_controls(ui, doc));
        self.show_view = open;
    }
    pub(super) fn reset_canvas_view(&mut self) {
        self.view = View::default();
        self.fit_canvas_view();
    }

    fn fit_canvas_view(&mut self) {
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        // Zoom one is the full-canvas fit; do not reframe an active selection.
        self.frame_pending = false;
    }

    fn zoom_canvas_at_center(&mut self, zoom: f32) {
        self.pan *= zoom / self.zoom;
        self.zoom = zoom;
        self.frame_pending = false;
    }

    pub(super) fn change_canvas_orientation(&mut self, doc: DocumentView<'_>, next: View) {
        if next == self.view {
            return;
        }
        let mut pan = next.vector(self.view.inverse(self.pan));
        let centered_at_fit = self.zoom == 1. && self.pan == Vec2::ZERO;
        if let Some(viewport) = self.canvas_viewport.filter(|_| !centered_at_fit) {
            let size = self.displayed_canvas_size(doc);
            // Maintain magnification and the viewport center outside the centered fit view.
            let old_scale = self.view.fit_scale(viewport, size) * self.zoom;
            let fit = next.fit_scale(viewport, size);
            let limits = zoom_limits(fit, self.edit_context.pixels_per_point());
            self.zoom = (old_scale / fit).clamp(*limits.start(), *limits.end());
            pan *= fit * self.zoom / old_scale;
        }
        self.pan = pan;
        self.view = next;
    }

    pub(super) fn canvas_view_gesture_active(&self) -> bool {
        self.live_gesture.is_some()
            || !self.points.is_empty()
            || self.gizmo_handle != 0
            || self.drag_start.is_some()
            || self.pen.in_gesture()
            || self.selection_gesture_source.is_some()
            || self.primitive_drag.is_some()
            || self.fill_press.is_some()
            || self.sample_press.is_some()
            || self.workspace_stroke.is_some()
            || self.size_drag.is_some()
            || self.crop_dragging()
            || self.source_editor.as_ref().is_some_and(|editor| editor.moving_text_field())
            || self.free_transform.as_ref().is_some_and(|editor| editor.is_dragging())
    }

    fn canvas_view_controls(&mut self, ui: &mut egui::Ui, doc: DocumentView<'_>) {
        if self.canvas_view_gesture_active() {
            ui.label(
                RichText::new("Finish the current gesture to change the view")
                    .small()
                    .color(MUTED),
            );
            ui.disable();
        }
        let pixels_per_point = ui.ctx().pixels_per_point();
        let fit = self.canvas_viewport.map(|viewport| {
            self.view.fit_scale(viewport, self.displayed_canvas_size(doc))
        });
        if let Some(fit) = fit {
            let fit_percent = fit * pixels_per_point * 100.;
            let limits = zoom_limits(fit, pixels_per_point);
            ui.horizontal(|ui| {
                let mut percent = self.zoom * fit_percent;
                controls::label(ui, "Zoom");
                let zoom = controls::range(
                    ui, "canvas zoom", &mut percent,
                    fit_percent * *limits.start()..=fit_percent * *limits.end(), 160., "%", 2, true,
                ).on_hover_text("Drag to zoom about the viewport center. Double-click to enter a percentage.");
                zoom.widget_info(|| {
                    egui::WidgetInfo::slider(zoom.enabled(), f64::from(percent), "Canvas zoom (%)")
                });
                icons::paint_focus(ui, &zoom);
                if zoom.changed() {
                    self.zoom_canvas_at_center((percent / fit_percent).clamp(*limits.start(), *limits.end()));
                    ui.ctx().request_repaint();
                }
            });
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Fit canvas").on_hover_text("Center and fit the entire canvas").clicked() {
                self.fit_canvas_view();
                ui.ctx().request_repaint();
            }
            if let Some(fit) = fit {
                if ui.button("100%")
                    .on_hover_text("Show one document pixel per display pixel at the viewport center")
                    .clicked()
                {
                    self.zoom_canvas_at_center(1. / (fit * pixels_per_point));
                    ui.ctx().request_repaint();
                }
            }
            let frame = self.shortcuts.hint("Frame canvas", "Frame selection or canvas");
            if icons::small_button(ui, Icon::Frame, &frame).clicked() {
                self.frame_pending = true;
                ui.ctx().request_repaint();
            }
        });
        let mut orientation = self.view;
        ui.horizontal(|ui| {
            controls::label(ui, "Rotation");
            for (action, icon, label, delta) in [
                ("Rotate view left", Icon::QuarterTurnLeft, "Rotate left 15°", -15.),
                ("Rotate view right", Icon::QuarterTurnRight, "Rotate right 15°", 15.),
            ] {
                let hint = self.shortcuts.hint(action, label);
                if icons::small_button(ui, icon, &hint).clicked() {
                    orientation.angle = (orientation.angle + delta + 180.).rem_euclid(360.) - 180.;
                }
            }
            let mut angle = f64::from(orientation.angle);
            if controls::angle(ui, "view rotation", &mut angle, true, "Canvas rotation",
                "Drag the dial to rotate the view. Shift snaps to 15°. Double-click to type an angle.")
                .changed()
            {
                orientation.angle = angle as f32;
            }
        });
        ui.horizontal(|ui| {
            controls::label(ui, "Mirror view");
            icons::toggle(ui, Icon::MirrorHorizontal, &mut orientation.mirror_h, "Mirror view horizontally");
            icons::toggle(ui, Icon::MirrorVertical, &mut orientation.mirror_v, "Mirror view vertically");
        });
        if orientation != self.view {
            self.change_canvas_orientation(doc, orientation);
            ui.ctx().request_repaint();
        }
        if ui.button("Reset canvas view").clicked() {
            self.reset_canvas_view();
            ui.ctx().request_repaint();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn visible_canvas_regions_follow_rotation_and_mirroring() {
        use super::*;
        let canvas = Rect::from_center_size(Pos2::ZERO, Vec2::splat(16000.));
        let viewport = Rect::from_min_max(Pos2::ZERO, Pos2::new(100., 200.));
        for (view, expected) in [
            (View::default(), [7998, 7998, 8102, 8202]),
            (View { angle: 90., ..View::default() }, [7998, 7898, 8202, 8002]),
            (View { mirror_h: true, ..View::default() }, [7898, 7998, 8002, 8202]),
        ] {
            assert_eq!(view.visible_region(canvas, viewport, [16000; 2]), Some(expected));
            assert_eq!(view.visible_region(canvas, canvas, [16000; 2]), None);
        }

        let corner = Rect::from_min_max(Pos2::new(-6000., -11400.), Pos2::new(6000., -11200.));
        let rotated = View { angle: 45., ..View::default() };
        assert_eq!(rotated.visible_region(canvas, corner, [16000; 2]), Some([0, 0, 163, 163]));
        let mirrored = View { mirror_h: true, ..rotated };
        assert_eq!(mirrored.visible_region(canvas, corner, [16000; 2]), Some([0, 0, 163, 163]));
    }
    use super::*;
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let time = ctx.input(|i| i.time) + 0.1;
        ctx.run(
            egui::RawInput {
                time: Some(time),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                events,
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        )
    }
    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }

    #[test]
    fn canvas_navigation_keeps_a_workspace_marquee_in_one_projection() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        app.shortcuts = shortcuts::Shortcuts::load(Path::new(""));
        app.select_tool(Tool::Selection);
        app.selection_kind = "rectangle".into();
        app.zoom = 0.75;
        frame(&mut app, &ctx, vec![]);
        let rect = app.view_rect.unwrap();
        let start = rect.left_center() - Vec2::new(12., 0.);
        assert!(app.canvas_input_region.unwrap().0.contains(start));
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(start), button(start, true)]);
        assert!(app.selection_gesture_source.is_some());
        let end = rect.center() + Vec2::splat(40.);
        frame(&mut app, &ctx, vec![
            egui::Event::PointerMoved(end),
            egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: Vec2::new(0., 90.),
                modifiers: Default::default(),
            },
            egui::Event::Key {
                key: egui::Key::F,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            },
        ]);
        assert_eq!(app.view_rect, Some(rect));
        assert!(!app.frame_pending);
        frame(&mut app, &ctx, vec![button(end, false)]);
        assert_eq!(app.view_rect, Some(rect));
    }

    #[test]
    fn rotated_and_mirrored_input_previews_and_commits_native_color_and_masks_once() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let mut reference = None;
                for view in [
                    View::default(),
                    View {
                        angle: 90.,
                        mirror_h: true,
                        mirror_v: false,
                    },
                    View {
                        angle: 37.,
                        mirror_h: false,
                        mirror_v: true,
                    },
                    View {
                        angle: -71.,
                        mirror_h: true,
                        mirror_v: true,
                    },
                ] {
                    let (mut app, ctx) = super::super::tests::fixture();
                    app.recovery.open = false;
                    app.shortcuts = shortcuts::Shortcuts::load(Path::new(""));
                    {
                        let mut e = app.shared.lock().unwrap();
                        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(64, 48, depth).unwrap();
                        for y in 0..48 {
                            for x in 0..64 {
                                e.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                                    x,
                                    y,
                                    [10001 + x as u16 * 137, 20003 + y as u16 * 157, 30007, 65535],
                                );
                            }
                        }
                        let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                        e.edit(
                            "human",
                            &[json!({"op":"mask.add","layer":layer,"value":183})],
                            None,
                            None,
                            "Mask",
                        )
                        .unwrap();
                        e.resident_state_mut().expect("resident fixture").undo.clear();
                    }
                    frame(&mut app, &ctx, vec![]);
                    app.select_tool(Tool::Brush);
                    app.mask = mask;
                    app.brush.tip = crate::brush::TipKind::Grain;
                    app.brush.seed = 71;
                    app.brush.grain = 2.75;
                    app.brush.density = 0.42;
                    app.brush.spacing = 0.2;
                    app.brush.hardness = 0.6;
                    app.radius = 7.;
                    app.brush.opacity = 0.7;
                    app.view = view;
                    app.frame_pending = true;
                    frame(&mut app, &ctx, vec![]);
                    frame(&mut app, &ctx, vec![]);
                    app.zoom *= 4. / (app.view_rect.unwrap().width() / 64.);
                    frame(&mut app, &ctx, vec![]);
                    let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    let rect = app.view_rect.unwrap();
                    let scale = rect.width() / 64.;
                    let start = view.point([16., 24.], rect, scale);
                    let end = view.point([48., 24.], rect, scale);
                    frame(
                        &mut app,
                        &ctx,
                        vec![egui::Event::PointerMoved(start), button(start, true)],
                    );
                    frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
                    assert_eq!(app.points.len(), 2, "{view:?}");
                    for (actual, expected) in app.points.iter().zip([[16., 24.], [48., 24.]]) {
                        for axis in 0..2 {
                            assert!((actual[axis] - expected[axis]).abs() < 0.001, "{view:?}");
                        }
                    }
                    let color = if mask {
                        [app.mask_value, app.mask_value, app.mask_value, app.color[3]]
                    } else {
                        app.color
                    };
                    let preview = crate::engine::Engine::preview_edits(
                        before.clone(),
                        &[app.stroke_command(&app.points, color)],
                    )
                    .unwrap();
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        frame(&mut app, &ctx, vec![]);
                        if app.canvas_pixels.as_ref().is_some_and(|(w, h, p)| {
                            preview.preview(None, (*w).max(*h), None, false).unwrap().2 == *p
                        }) {
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "Shared rendered preview missing"
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                    frame(&mut app, &ctx, vec![button(end, false)]);
                    assert!(
                        app.live_gesture.is_none(),
                        "Completed stroke must unlock view and preference updates"
                    );
                    let mut e = app.shared.lock().unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                    let png = e.resident_state().expect("resident fixture").document.export_png().unwrap();
                    assert_eq!(png, preview.export_png().unwrap());
                    if let Some(expected) = &reference {
                        assert_eq!(&png, expected, "{view:?}");
                    } else {
                        reference = Some(png);
                    }
                    let reopened =
                        crate::psd::decode(&crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap()).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.export_png().unwrap(), e.resident_state().expect("resident fixture").document.export_png().unwrap());
                    e.undo("human").unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                }
            }
        }
    }
    #[test]
    fn wheel_navigation_draws_current_mesh_and_keeps_the_cursor_anchor_at_native_depth() {
        for depth in [8, 16] {
            for angle in [0., 37.] {
                let (mut app, ctx) = super::super::tests::fixture();
                app.recovery.open = false;
                app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = Document::new_depth(96, 60, depth).unwrap();
                app.view.angle = angle;
                app.view.mirror_h = true;
                // Keep asynchronous preview arrival out of the mesh/input assertion.
                app.pending = true;
                app.set_canvas(&ctx, 1, 1, vec![233, 84, 32, 255]);
                let texture = app.texture.as_ref().unwrap().id();
                let before = crate::psd::encode(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
                frame(&mut app, &ctx, vec![]);
                let original = app.view_rect.unwrap();
                let anchor = original.center() + Vec2::new(40., -10.);
                let source = app.view.document(anchor, original, original.width() / 96.);
                let output = frame(
                    &mut app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(anchor),
                        egui::Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Point,
                            delta: Vec2::new(0., 90.),
                            modifiers: Default::default(),
                        },
                    ],
                );
                let rect = app.view_rect.unwrap();
                assert!(rect.width() > original.width());
                let scale = rect.width() / 96.;
                let actual = app.view.document(anchor, rect, scale);
                assert!((actual[0] - source[0]).abs() < 0.001);
                assert!((actual[1] - source[1]).abs() < 0.001);
                let mesh = output
                    .shapes
                    .iter()
                    .find_map(|s| match &s.shape {
                        egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh),
                        _ => None,
                    })
                    .unwrap();
                assert_eq!(
                    mesh.vertices.iter().map(|v| v.pos).collect::<Vec<_>>(),
                    app.view.corners(rect, scale)
                );
                assert_eq!(
                    crate::psd::encode(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                    before
                );
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            }
        }
    }
    #[test]
    fn canvas_orientation_keeps_the_document_point_at_viewport_center() {
        let (mut app, _) = super::super::tests::fixture();
        let doc = Document::new_depth(96, 60, 16).unwrap();
        let viewport = Rect::from_min_size(Pos2::new(12., 20.), Vec2::new(620., 430.));
        app.canvas_viewport = Some(viewport);
        app.zoom = 2.5;
        app.pan = Vec2::new(73., -61.);
        let center_point = |app: &PeerBrush| {
            let scale = app.view.fit_scale(viewport, [96, 60]) * app.zoom;
            let rect = Rect::from_center_size(viewport.center() + app.pan, Vec2::new(96., 60.) * scale);
            app.view.document(viewport.center(), rect, scale)
        };
        let original = center_point(&app);
        for view in [
            View { angle: 45., ..View::default() },
            View { angle: 45., mirror_h: true, mirror_v: false },
            View { angle: -90., mirror_h: true, mirror_v: true },
            View::default(),
        ] {
            app.change_canvas_orientation((&doc).into(), view);
            let current = center_point(&app);
            assert!((current[0] - original[0]).abs() < 0.001);
            assert!((current[1] - original[1]).abs() < 0.001);
        }
    }
    #[test]
    fn canvas_mesh_uv_and_navigation_preserve_document_sources_and_coordinates() {
        let rect = Rect::from_min_size(Pos2::new(70., 90.), Vec2::new(288., 180.));
        let scale = 3.;
        for angle in [-180., -90., -37., 0., 41., 90., 180.] {
            for h in [false, true] {
                for v in [false, true] {
                    let view = View {
                        angle,
                        mirror_h: h,
                        mirror_v: v,
                    };
                    for p in [[0., 0.], [96., 60.], [17., 24.], [11.375, 42.75]] {
                        let actual = view.document(view.point(p, rect, scale), rect, scale);
                        for axis in 0..2 {
                            assert!((actual[axis] - p[axis]).abs() < 0.001);
                        }
                    }
                    assert!(!view.contains(view.point([-2., 10.], rect, scale), rect, scale));
                    let ctx = egui::Context::default();
                    let texture = egui::TextureId::User(17);
                    let output = ctx.run(Default::default(), |ctx| {
                        paint_checker(
                            &ctx.layer_painter(egui::LayerId::background()),
                            view.corners(rect, scale),
                            texture,
                        )
                    });
                    let mesh = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh),
                            _ => None,
                        })
                        .unwrap();
                    assert_eq!(
                        mesh.vertices.iter().map(|v| v.pos).collect::<Vec<_>>(),
                        view.corners(rect, scale)
                    );
                    assert_eq!(
                        mesh.vertices.iter().map(|v| v.uv).collect::<Vec<_>>(),
                        view.corners(rect, scale).map(|p| Pos2::new(p.x / 32., p.y / 32.))
                    );
                }
            }
        }
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let before = crate::psd::encode(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
        app.view = View {
            angle: 37.,
            mirror_h: true,
            mirror_v: true,
        };
        app.zoom = 2.;
        app.pan = Vec2::new(13., -27.);
        let visible = frame(&mut app, &ctx, vec![]);
        let checker_id = checker(&ctx).id();
        let draws_checker = |output: &egui::FullOutput| output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.texture_id == checker_id)
        });
        assert!(draws_checker(&visible));
        app.frame_pending = false;
        app.pan = Vec2::splat(1_000_000.);
        assert!(!draws_checker(&frame(&mut app, &ctx, vec![])));
        assert_eq!(
            crate::psd::encode(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
            before
        );
        assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
        app.reset_canvas_view();
        assert_eq!(app.view, View::default());
        assert_eq!(app.zoom, 1.);
        assert_eq!(app.pan, Vec2::ZERO);
        assert!(!app.frame_pending);
    }
    #[test]
    fn rotated_shapes_selection_paths_and_move_handles_use_document_coordinates() {
        for depth in [8, 16] {
            for tool in [
                Tool::Rectangle,
                Tool::Gradient,
                Tool::Selection,
                Tool::Pen,
                Tool::Move,
            ] {
                let (mut app, ctx) = super::super::tests::fixture();
                app.recovery.open = false;
                app.shortcuts = shortcuts::Shortcuts::load(Path::new(""));
                {
                    let mut e = app.shared.lock().unwrap();
                    e.resident_state_mut().expect("resident fixture").document = Document::new_depth(64, 48, depth).unwrap();
                    for y in 0..48 {
                        for x in 0..64 {
                            e.resident_state_mut().expect("resident fixture").document.layers[0]
                                .pixels
                                .set16(x, y, [12001, 23003, 34007, 65535]);
                        }
                    }
                    e.resident_state_mut().expect("resident fixture").undo.clear();
                }
                frame(&mut app, &ctx, vec![]);
                app.select_tool(tool);
                app.view = View {
                    angle: 37.,
                    mirror_h: true,
                    mirror_v: false,
                };
                app.frame_pending = true;
                frame(&mut app, &ctx, vec![]);
                frame(&mut app, &ctx, vec![]);
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let rect = app.view_rect.unwrap();
                let scale = rect.width() / 64.;
                let (a, b) = if tool == Tool::Move {
                    ([32., 24.], [40., 30.])
                } else if tool == Tool::Pen {
                    ([16., 16.], [16., 16.])
                } else {
                    ([16., 16.], [48., 32.])
                };
                let start = app.view.point(a, rect, scale);
                let end = app.view.point(b, rect, scale);
                frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
                let commands = app.transient.clone();
                if matches!(tool, Tool::Move | Tool::Rectangle | Tool::Gradient) {
                    assert!(
                        !commands.is_empty(),
                        "{} missing rendered commands",
                        tool.label()
                    );
                    let preview =
                        crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        frame(&mut app, &ctx, vec![]);
                        if app.canvas_pixels.as_ref().is_some_and(|(w, h, p)| {
                            preview.preview(None, (*w).max(*h), None, false).unwrap().2 == *p
                        }) {
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "{} rendered preview missing",
                            tool.label()
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                frame(&mut app, &ctx, vec![button(end, false)]);
                assert!(
                    app.live_gesture.is_none(),
                    "Completed gesture must unlock the view"
                );
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}: {}", tool.label(), app.message);
                assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, depth);
                if tool == Tool::Selection {
                    assert_eq!(e.resident_state().expect("resident fixture").document.selection, Some([16, 16, 48, 32]));
                }
                if tool == Tool::Pen {
                    let nodes = e
                        .resident_state().expect("resident fixture").document
                        .layers
                        .iter()
                        .find_map(|l| match l.source.as_deref() {
                            Some(crate::source::Source {
                                content: crate::source::Content::Bezier { nodes, .. },
                                ..
                            }) => Some(nodes),
                            _ => None,
                        })
                        .expect("Native editable path missing");
                    assert_eq!(nodes[0].point, [16., 16.]);
                }
                if tool == Tool::Move {
                    assert_eq!((e.resident_state().expect("resident fixture").document.layers[0].x, e.resident_state().expect("resident fixture").document.layers[0].y), (8, 6));
                }
                if !commands.is_empty()
                    && matches!(tool, Tool::Move | Tool::Rectangle | Tool::Gradient)
                {
                    let preview =
                        crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                    assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), preview.export_png().unwrap());
                }
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
            }
        }
    }
}
