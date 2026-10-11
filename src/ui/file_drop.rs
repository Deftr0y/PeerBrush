//! File-drop destinations route through the ordinary shared opening/import paths.
use super::*;

impl PeerBrush {
    pub(super) fn file_drop_on_tabs(&self, ctx: &egui::Context) -> bool {
        self.project_drop_rect.is_some_and(|rect| {
            ctx.input(|i| i.pointer.hover_pos().or_else(|| i.pointer.interact_pos()))
                .is_some_and(|pos| rect.contains(pos))
        })
    }
    pub(super) fn drop_project_file(&mut self, paths: Vec<PathBuf>) {
        if paths.len() != 1 {
            self.message = "Drop one file onto the tabs area to open a new project".into();
            return;
        }
        if self.busy || self.pending_image_open.is_some() || self.pending_import.is_some() {
            self.message =
                "Finish the current file operation before opening a dropped project".into();
            return;
        }
        self.finish_parameter(true);
        self.close_proposal();
        self.open_job(paths.into_iter().next(), false);
    }
    pub(super) fn file_drop_feedback(&self, ctx: &egui::Context) {
        if !ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            return;
        }
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("file-drop"),
        ));
        if self.file_drop_on_tabs(ctx) {
            let bounds = self.project_drop_rect.unwrap();
            painter.rect_filled(bounds, 0., Color32::from_rgb(58, 36, 32));
            painter.line_segment(
                [bounds.left_bottom(), bounds.right_bottom()],
                Stroke::new(2., ACCENT),
            );
            painter.text(
                bounds.center(),
                egui::Align2::CENTER_CENTER,
                "Drop to open a new project tab",
                egui::FontId::proportional(14.),
                Color32::WHITE,
            );
        } else {
            let bounds = ctx.screen_rect().shrink(8.);
            painter.rect_stroke(
                bounds,
                8.,
                Stroke::new(3., ACCENT),
                egui::StrokeKind::Inside,
            );
            painter.text(
                bounds.center(),
                egui::Align2::CENTER_CENTER,
                "Drop PSD/PSB to open · Drop images to add layers",
                egui::FontId::proportional(23.),
                Color32::WHITE,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(app: &mut PeerBrush, ctx: &egui::Context, raw: egui::RawInput) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                ..raw
            },
            |ctx| app.draw(ctx),
        )
    }
    #[test]
    fn tab_drop_opens_native_image_and_canvas_drop_still_imports_without_losing_work() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.recovery.open = false;
            let root = app.shared.clone();
            let project = app.project_id.clone();
            let layer = root.lock().unwrap().doc.layers[0].id.clone();
            server::dispatch(&root,"edit",&json!({"actor":"human","commands":[{"op":"paint","layer":layer,"points":[[10,10],[30,30]],"radius":5,"color":[233,84,32,255]},{"op":"selection","kind":"rectangle","rect":[3,4,40,50]}]})).unwrap();
            frame(&mut app, &ctx, Default::default());
            app.zoom = 1.7;
            app.pan = Vec2::new(13., -17.);
            let before = root.lock().unwrap().state();
            let history = root.lock().unwrap().undo.len();
            let path = std::env::temp_dir().join(format!(
                "café שלום مرحبا हिन्दी 漢字 あいう 한글-{}.png",
                crate::engine::id()
            ));
            let bytes = if depth == 16 {
                crate::raster::png16(3, 2, &[12345, 23457, 45679, 32123].repeat(6)).unwrap()
            } else {
                crate::raster::png(3, 2, &[123, 45, 67, 89].repeat(6)).unwrap()
            };
            std::fs::write(&path, &bytes).unwrap();
            let pos = app.project_drop_rect.unwrap().center();
            let output = frame(
                &mut app,
                &ctx,
                egui::RawInput {
                    events: vec![egui::Event::PointerMoved(pos)],
                    hovered_files: vec![egui::HoveredFile {
                        path: Some(path.clone()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            );
            assert!(output.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Drop to open a new project tab")));
            frame(
                &mut app,
                &ctx,
                egui::RawInput {
                    events: vec![egui::Event::PointerMoved(pos)],
                    dropped_files: vec![egui::DroppedFile {
                        path: Some(path.clone()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while crate::workspace::handles(&root).len() < 2 || app.busy {
                assert!(std::time::Instant::now() < deadline, "{}", app.message);
                std::thread::sleep(Duration::from_millis(5));
                frame(&mut app, &ctx, Default::default());
            }
            frame(&mut app, &ctx, Default::default());
            assert_eq!(app.shared.lock().unwrap().doc.bit_depth, depth);
            assert_eq!(
                app.shared.lock().unwrap().doc.layers[0].pixels.get16(0, 0),
                if depth == 16 {
                    [12345, 23457, 45679, 32123]
                } else {
                    [123 * 257, 45 * 257, 67 * 257, 89 * 257]
                }
            );
            assert_eq!(
                app.shared.lock().unwrap().doc.name,
                path.file_name().unwrap().to_str().unwrap()
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(root.lock().unwrap().state(), before);
            assert_eq!(root.lock().unwrap().undo.len(), history);
            crate::workspace::select(&root, &project).unwrap();
            app.switch_project(&project);
            assert_eq!(app.zoom, 1.7);
            assert_eq!(app.pan, Vec2::new(13., -17.));
            let canvas = app.view_rect.unwrap().center();
            frame(
                &mut app,
                &ctx,
                egui::RawInput {
                    events: vec![egui::Event::PointerMoved(canvas)],
                    dropped_files: vec![egui::DroppedFile {
                        path: Some(path.clone()),
                        ..Default::default()
                    }],
                    ..Default::default()
                },
            );
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while root.lock().unwrap().doc.layers.len() < 2 || app.busy {
                assert!(std::time::Instant::now() < deadline, "{}", app.message);
                std::thread::sleep(Duration::from_millis(5));
                frame(&mut app, &ctx, Default::default());
            }
            assert_eq!(crate::workspace::handles(&root).len(), 2);
            assert_eq!(root.lock().unwrap().undo.len(), history + 1);
            root.lock().unwrap().undo("human").unwrap();
            assert_eq!(root.lock().unwrap().doc.layers.len(), 1);
            assert_eq!(root.lock().unwrap().doc.selection, Some([3, 4, 40, 50]));
            std::fs::remove_file(path).unwrap();
        }
    }
    #[test]
    fn ambiguous_multi_file_tab_drop_and_unsupported_single_file_preserve_projects() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let before = app.shared.lock().unwrap().state();
        app.drop_project_file(vec!["One.png".into(), "Two.jpg".into()]);
        assert!(app.message.contains("Drop one file"));
        assert!(!app.busy);
        let path =
            std::env::temp_dir().join(format!("pb-tab-unsupported-{}.pdf", crate::engine::id()));
        std::fs::write(&path, b"%PDF-1.4").unwrap();
        app.drop_project_file(vec![path.clone()]);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while app.busy {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
            frame(&mut app, &ctx, Default::default());
        }
        assert!(app.message.contains("Unsupported"));
        assert_eq!(app.shared.lock().unwrap().state(), before);
        assert_eq!(crate::workspace::handles(&app.shared).len(), 1);
        std::fs::remove_file(path).unwrap();
    }
}
