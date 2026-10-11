use super::*;
use crate::source::Content;
use std::sync::Mutex;
#[test]
fn pen_click_drag_close_and_point_delete_render_native_previews_with_one_undo() {
    for depth in [8, 16] {
        for zoom in [0.65, 1.35] {
            let ctx = egui::Context::default();
            let mut e = crate::engine::Engine::new();
            e.doc = Document::new_depth(64, 48, depth).unwrap();
            for y in 0..48 {
                for x in 0..64 {
                    e.doc.layers[0]
                        .pixels
                        .set16(x, y, [10001, 20003, 40007, 65535]);
                }
            }
            let background = e.doc.layers[0].pixels.clone();
            let shared = Arc::new(Mutex::new(e));
            let connection = crate::server::Connection {
                port: 0,
                token: String::new(),
                state_dir: Default::default(),
                instance_lock: None,
                availability: Default::default(),
            };
            let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
            app.select_tool(Tool::Pen);
            app.zoom = zoom;
            let mut time = 0.;
            let mut draw = |app: &mut PeerBrush, events| {
                time += 0.05;
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300., 950.))),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            draw(&mut app, vec![]);
            draw(&mut app, vec![]);
            let rect = app.view_rect.unwrap();
            let scale = rect.width() / 64.;
            let at = |p: [f32; 2]| rect.min + Vec2::new(p[0], p[1]) * scale;
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            for (index, (point, end)) in [
                ([12., 34.], [12., 34.]),
                ([32., 12.], [48., 12.]),
                ([52., 34.], [52., 34.]),
            ]
            .into_iter()
            .enumerate()
            {
                let before = shared.lock().unwrap().doc.clone();
                let start = at(point);
                let end = at(end);
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                assert_eq!(app.transient.len(), 1);
                let preview =
                    crate::engine::Engine::preview_edits(before.clone(), &app.transient).unwrap();
                if index > 0 {
                    assert_ne!(preview.export_png().unwrap(), before.export_png().unwrap());
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        draw(&mut app, vec![]);
                        if app.canvas_pixels.as_ref().is_some_and(|(w, h, p)| {
                            preview.preview(None, (*w).max(*h), None, false).unwrap().2 == *p
                        }) {
                            break;
                        }
                        assert!(
                            std::time::Instant::now() < deadline,
                            "Actual path preview missing: {}",
                            app.message
                        );
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
                assert_eq!(shared.lock().unwrap().doc.revision, before.revision);
                assert_eq!(shared.lock().unwrap().undo.len(), index);
                draw(&mut app, vec![button(end, false)]);
                let e = shared.lock().unwrap();
                assert_eq!(e.undo.len(), index + 1, "{}", app.message);
                assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
                assert_eq!(e.doc.layers[1].pixels, background);
            }
            let start = at([12., 34.]);
            draw(
                &mut app,
                vec![egui::Event::PointerMoved(start), button(start, true)],
            );
            draw(&mut app, vec![button(start, false)]);
            {
                let e = shared.lock().unwrap();
                let Content::Bezier { nodes, closed, .. } =
                    &e.doc.layers[0].source.as_ref().unwrap().content
                else {
                    panic!("Path missing")
                };
                assert_eq!(nodes.len(), 3);
                assert!(*closed);
                assert!(nodes[1].smooth);
                assert_eq!(e.undo.len(), 4);
            }
            draw(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Delete,
                    physical_key: Some(egui::Key::Delete),
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
            let mut e = shared.lock().unwrap();
            assert_eq!(
                e.doc.layers.len(),
                2,
                "Delete must remove a point rather than its layer"
            );
            let Content::Bezier { nodes, .. } = &e.doc.layers[0].source.as_ref().unwrap().content
            else {
                panic!("Path missing")
            };
            assert_eq!(nodes.len(), 2);
            assert_eq!(e.undo.len(), 5);
            e.undo("human").unwrap();
            assert_eq!(e.doc.layers[1].pixels, background);
        }
    }
}
#[test]
fn pen_escape_or_shared_change_cancels_uncommitted_paths() {
    for depth in [8, 16] {
        let ctx = egui::Context::default();
        let mut e = crate::engine::Engine::new();
        e.doc = Document::new_depth(64, 48, depth).unwrap();
        let before = e.doc.clone();
        let shared = Arc::new(Mutex::new(e));
        let connection = crate::server::Connection {
            port: 0,
            token: String::new(),
            state_dir: Default::default(),
            instance_lock: None,
            availability: Default::default(),
        };
        let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
        app.select_tool(Tool::Pen);
        let mut time = 0.;
        let mut draw = |app: &mut PeerBrush, events| {
            time += 0.05;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300., 950.))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            )
        };
        draw(&mut app, vec![]);
        let start = app.view_rect.unwrap().center();
        draw(
            &mut app,
            vec![
                egui::Event::PointerMoved(start),
                egui::Event::PointerButton {
                    pos: start,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        assert_eq!(app.transient.len(), 1);
        draw(
            &mut app,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(app.transient.is_empty());
        assert!(shared.lock().unwrap().undo.is_empty());
        assert_eq!(
            shared.lock().unwrap().doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
        draw(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        draw(
            &mut app,
            vec![egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            }],
        );
        {
            let mut e = shared.lock().unwrap();
            let id = e.doc.layers[0].id.clone();
            e.edit(
                "human",
                &[json!({"op":"layer.update","layer":id,"name":"Human update"})],
                None,
                None,
                "Human update",
            )
            .unwrap();
        }
        draw(&mut app, vec![]);
        assert!(app.transient.is_empty());
        assert_eq!(shared.lock().unwrap().doc.layers.len(), 1);
        assert_eq!(shared.lock().unwrap().undo.len(), 1);
    }
}

#[test]
fn pen_selects_points_without_history_and_drags_existing_handles_through_native_preview() {
    for depth in [8, 16] {
        let ctx = egui::Context::default();
        let mut e = crate::engine::Engine::new();
        e.doc = Document::new_depth(64, 48, depth).unwrap();
        e.edit("human",&[json!({"op":"path.create","nodes":[{"point":[16,36],"out_handle":[24,8],"in_handle":[8,64],"smooth":true},{"point":[48,36],"in_handle":[40,8]}],"stroke_width":3.})],None,None,"Path").unwrap();
        let before = e.doc.clone();
        e.undo.clear();
        let id = e.doc.layers[0].id.clone();
        let shared = Arc::new(Mutex::new(e));
        let connection = crate::server::Connection {
            port: 0,
            token: String::new(),
            state_dir: Default::default(),
            instance_lock: None,
            availability: Default::default(),
        };
        let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
        app.selected = id.clone();
        app.selection_layers = [id].into_iter().collect();
        app.select_tool(Tool::Pen);
        let mut time = 0.;
        let mut draw = |app: &mut PeerBrush, events| {
            time += 0.05;
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300., 950.))),
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ctx| app.draw(ctx),
            )
        };
        draw(&mut app, vec![]);
        draw(&mut app, vec![]);
        let rect = app.view_rect.unwrap();
        let scale = rect.width() / 64.;
        let at = |p: [f32; 2]| rect.min + Vec2::new(p[0], p[1]) * scale;
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let p = at([16., 36.]);
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(p), button(p, true)],
        );
        draw(&mut app, vec![button(p, false)]);
        assert!(shared.lock().unwrap().undo.is_empty());
        let start = at([24., 8.]);
        let end = at([32., 16.]);
        draw(
            &mut app,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        draw(&mut app, vec![egui::Event::PointerMoved(end)]);
        assert_eq!(app.transient[0]["op"], "path.handle.move");
        let preview = crate::engine::Engine::preview_edits(before.clone(), &app.transient).unwrap();
        assert_ne!(preview.export_png().unwrap(), before.export_png().unwrap());
        assert!(shared.lock().unwrap().undo.is_empty());
        draw(&mut app, vec![button(end, false)]);
        let mut e = shared.lock().unwrap();
        assert_eq!(e.undo.len(), 1);
        assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
        let Content::Bezier { nodes, .. } = &e.doc.layers[0].source.as_ref().unwrap().content
        else {
            panic!("Path absent")
        };
        assert_eq!(nodes[0].out_handle, Some([32., 16.]));
        e.undo("human").unwrap();
        assert_eq!(e.doc.layers[0].source, before.layers[0].source);
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
    }
}
