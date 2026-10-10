#[cfg(test)]
mod tests {
    use super::super::*;
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                events,
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        )
    }
    #[test]
    fn duplicate_popup_focuses_existing_without_losing_native_view_selection_or_history() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.recovery.open = false;
            let dir = std::env::temp_dir().join(format!("pb-open-popup-{}", crate::engine::id()));
            std::fs::create_dir(&dir).unwrap();
            let path = dir.join("Artwork.psd");
            {
                let mut e = app.shared.lock().unwrap();
                e.doc = Document::new_depth(64, 64, depth).unwrap();
            }
            let root = app.shared.clone();
            server::save(&root, &path).unwrap();
            let layer = root.lock().unwrap().doc.layers[0].id.clone();
            server::dispatch(&root,"edit",&json!({"actor":"human","commands":[{"op":"layer.update","layer":layer,"name":"Unsaved work"},{"op":"selection","kind":"rectangle","rect":[3,4,30,40]}]})).unwrap();
            frame(&mut app, &ctx, vec![]);
            app.select_content(&layer);
            app.zoom = 1.75;
            app.pan = Vec2::new(17., -29.);
            let original = app.project_id.clone();
            let before = root.lock().unwrap().state();
            let undo = root.lock().unwrap().undo.len();
            let second = crate::workspace::new_project(&root, 32, 32, depth, None).unwrap();
            let other = second.lock().unwrap().project_id.clone();
            app.switch_project(&other);
            assert!(server::dispatch(
                &root,
                "projects",
                &json!({"action":"open","path":path,"actor":"human"})
            )
            .is_err());
            let mut out = frame(&mut app, &ctx, vec![]);
            for _ in 0..3 {
                out = frame(&mut app, &ctx, vec![]);
            }
            assert_eq!(app.project_id, original);
            assert_eq!(app.zoom, 1.75);
            assert_eq!(app.pan, Vec2::new(17., -29.));
            assert_eq!(app.selected, layer);
            assert_eq!(root.lock().unwrap().state(), before);
            assert_eq!(root.lock().unwrap().undo.len(), undo);
            assert_eq!(crate::workspace::handles(&root).len(), 2);
            assert!(app
                .open_file_warning
                .as_ref()
                .unwrap()
                .contains("Artwork.psd"));
            let pos = out
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == "OK" => {
                        Some(t.pos + t.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("Visible duplicate warning OK button");
            for pressed in [true, false] {
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(pos),
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        },
                    ],
                );
            }
            assert!(app.open_file_warning.is_none());
            drop(app);
            drop(root);
            drop(second);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
