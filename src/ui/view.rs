use super::*;

/// Presentation only. Artwork, shared previews, history and saves use document coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct View {
    pub angle: f32,
    pub mirror_h: bool,
    pub mirror_v: bool,
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
        let (sin, cos) = self.basis();
        let x = v.x * cos - v.y * sin;
        let y = v.x * sin + v.y * cos;
        Vec2::new(
            if self.mirror_h { -x } else { x },
            if self.mirror_v { -y } else { y },
        )
    }
    pub fn inverse(self, v: Vec2) -> Vec2 {
        let (sin, cos) = self.basis();
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
    pub fn point(self, p: [f32; 2], rect: Rect, scale: f32) -> Pos2 {
        if self == Self::default() {
            return rect.min + Vec2::new(p[0], p[1]) * scale;
        }
        rect.center() + self.vector(Vec2::new(p[0], p[1]) * scale - rect.size() * 0.5)
    }
    pub fn document(self, p: Pos2, rect: Rect, scale: f32) -> [f32; 2] {
        let v = if self == Self::default() {
            (p - rect.min) / scale
        } else {
            (self.inverse(p - rect.center()) + rect.size() * 0.5) / scale
        };
        // Use the same roundoff correction with and without a transformed view.
        let boundary = |value: f32| {
            if (value - value.round()).abs() < 0.001 {
                value.round()
            } else {
                value
            }
        };
        [boundary(v.x), boundary(v.y)]
    }
    pub fn contains(self, p: Pos2, rect: Rect, scale: f32) -> bool {
        let p = self.document(p, rect, scale);
        p[0] >= 0. && p[1] >= 0. && p[0] <= rect.width() / scale && p[1] <= rect.height() / scale
    }
    pub fn corners(self, rect: Rect, scale: f32) -> [Pos2; 4] {
        let w = rect.width() / scale;
        let h = rect.height() / scale;
        [[0., 0.], [w, 0.], [w, h], [0., h]].map(|p| self.point(p, rect, scale))
    }
    pub fn bounds(self, rect: Rect, scale: f32) -> Rect {
        let points = self.corners(rect, scale);
        let min = points
            .iter()
            .fold(Pos2::new(f32::INFINITY, f32::INFINITY), |a, b| a.min(*b));
        let max = points
            .iter()
            .fold(Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY), |a, b| {
                a.max(*b)
            });
        Rect::from_min_max(min, max)
    }
    pub fn image(
        self,
        painter: &egui::Painter,
        rect: Rect,
        scale: f32,
        texture: egui::TextureId,
        checker: bool,
    ) {
        let positions = self.corners(rect, scale);
        let uv = if checker {
            positions.map(|p| Pos2::new(p.x / 32., p.y / 32.))
        } else {
            [
                Pos2::new(0., 0.),
                Pos2::new(1., 0.),
                Pos2::new(1., 1.),
                Pos2::new(0., 1.),
            ]
        };
        let mut mesh = egui::epaint::Mesh::with_texture(texture);
        for (pos, uv) in positions.into_iter().zip(uv) {
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv,
                color: Color32::WHITE,
            });
        }
        mesh.indices.extend_from_slice(&[0, 1, 2, 0, 2, 3]);
        painter.add(egui::Shape::mesh(mesh));
    }
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
    pub(super) fn canvas_view_window(&mut self, ctx: &egui::Context) {
        if !self.show_view {
            return;
        }
        let mut open = true;
        egui::Window::new("Canvas view")
            .open(&mut open)
            .resizable(false)
            .default_width(300.)
            .show(ctx, |ui| self.canvas_view_menu(ui));
        self.show_view = open;
    }
    pub(super) fn reset_canvas_view(&mut self) {
        self.view = View::default();
        self.zoom = 1.;
        self.pan = Vec2::ZERO;
        // Zoom one is the full-canvas fit; do not reframe an active selection.
        self.frame_pending = false;
    }
    pub(super) fn canvas_view_menu(&mut self, ui: &mut egui::Ui) {
        if !self.show_view && ui.small_button("Canvas view controls…").clicked() {
            self.show_view = true;
            ui.close_menu();
        }
        ui.label(RichText::new("Canvas view").small().color(MUTED));
        ui.label(
            RichText::new("View changes leave your artwork unchanged")
                .small()
                .color(MUTED),
        );
        if self.live_gesture.is_some()
            || !self.points.is_empty()
            || self.gizmo_handle != 0
            || self.drag_start.is_some()
            || self.pen.in_gesture()
        {
            ui.label(
                RichText::new("Finish the current gesture to change the view")
                    .small()
                    .color(MUTED),
            );
            ui.disable();
        }
        ui.horizontal(|ui| {
            controls::label(ui, "Rotation");
            controls::range(
                ui,
                "view rotation",
                &mut self.view.angle,
                -180.0..=180.0,
                160.,
                "°",
                0,
                false,
            );
        });
        ui.horizontal(|ui| {
            for (label, delta) in [("-15°", -15.), ("+15°", 15.)] {
                if ui.small_button(label).clicked() {
                    self.view.angle = (self.view.angle + delta + 180.).rem_euclid(360.) - 180.;
                }
            }
        });
        ui.checkbox(&mut self.view.mirror_h, "Mirror view horizontally");
        ui.checkbox(&mut self.view.mirror_v, "Mirror view vertically");
        if ui.button("Keyboard and pen shortcuts…").clicked() {
            self.shortcuts.open = true;
            ui.close_menu();
        }
        if ui.button("Reset canvas view").clicked() {
            self.reset_canvas_view();
            ui.close_menu();
        }
    }
}

#[cfg(test)]
mod tests {
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
                        e.doc = Document::new_depth(64, 48, depth).unwrap();
                        for y in 0..48 {
                            for x in 0..64 {
                                e.doc.layers[0].pixels.set16(
                                    x,
                                    y,
                                    [10001 + x as u16 * 137, 20003 + y as u16 * 157, 30007, 65535],
                                );
                            }
                        }
                        let layer = e.doc.layers[0].id.clone();
                        e.edit(
                            "human",
                            &[json!({"op":"mask.add","layer":layer,"value":183})],
                            None,
                            None,
                            "Mask",
                        )
                        .unwrap();
                        e.undo.clear();
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
                    let before = app.shared.lock().unwrap().doc.clone();
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
                        &[app.stroke_command(app.points.clone(), color)],
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
                    assert!(app.shared.lock().unwrap().undo.is_empty());
                    frame(&mut app, &ctx, vec![button(end, false)]);
                    assert!(
                        app.live_gesture.is_none(),
                        "Completed stroke must unlock view and preference updates"
                    );
                    let mut e = app.shared.lock().unwrap();
                    assert_eq!(e.undo.len(), 1, "{}", app.message);
                    let png = e.doc.export_png().unwrap();
                    assert_eq!(png, preview.export_png().unwrap());
                    if let Some(expected) = &reference {
                        assert_eq!(&png, expected, "{view:?}");
                    } else {
                        reference = Some(png);
                    }
                    let reopened =
                        crate::psd::decode(&crate::psd::encode(&e.doc).unwrap()).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
                    e.undo("human").unwrap();
                    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                    assert_eq!(
                        e.doc.layers[0].pixels.rgba16(),
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
                app.shared.lock().unwrap().doc = Document::new_depth(96, 60, depth).unwrap();
                app.view.angle = angle;
                app.view.mirror_h = true;
                // Keep asynchronous preview arrival out of the mesh/input assertion.
                app.pending = true;
                app.set_canvas(&ctx, 1, 1, vec![233, 84, 32, 255]);
                let texture = app.texture.as_ref().unwrap().id();
                let before = crate::psd::encode(&app.shared.lock().unwrap().doc).unwrap();
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
                    crate::psd::encode(&app.shared.lock().unwrap().doc).unwrap(),
                    before
                );
                assert!(app.shared.lock().unwrap().undo.is_empty());
            }
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
                        view.image(
                            &ctx.layer_painter(egui::LayerId::background()),
                            rect,
                            scale,
                            texture,
                            false,
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
                        vec![
                            Pos2::ZERO,
                            Pos2::new(1., 0.),
                            Pos2::new(1., 1.),
                            Pos2::new(0., 1.)
                        ]
                    );
                }
            }
        }
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let before = crate::psd::encode(&app.shared.lock().unwrap().doc).unwrap();
        app.view = View {
            angle: 37.,
            mirror_h: true,
            mirror_v: true,
        };
        app.zoom = 2.;
        app.pan = Vec2::new(13., -27.);
        frame(&mut app, &ctx, vec![]);
        assert_eq!(
            crate::psd::encode(&app.shared.lock().unwrap().doc).unwrap(),
            before
        );
        assert!(app.shared.lock().unwrap().undo.is_empty());
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
                    e.doc = Document::new_depth(64, 48, depth).unwrap();
                    for y in 0..48 {
                        for x in 0..64 {
                            e.doc.layers[0]
                                .pixels
                                .set16(x, y, [12001, 23003, 34007, 65535]);
                        }
                    }
                    e.undo.clear();
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
                let before = app.shared.lock().unwrap().doc.clone();
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
                assert_eq!(e.undo.len(), 1, "{}: {}", tool.label(), app.message);
                assert_eq!(e.doc.bit_depth, depth);
                if tool == Tool::Selection {
                    assert_eq!(e.doc.selection, Some([16, 16, 48, 32]));
                }
                if tool == Tool::Pen {
                    let nodes = e
                        .doc
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
                    assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (8, 6));
                }
                if !commands.is_empty()
                    && matches!(tool, Tool::Move | Tool::Rectangle | Tool::Gradient)
                {
                    let preview =
                        crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                    assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
                }
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                assert_eq!(
                    e.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
            }
        }
    }
}
