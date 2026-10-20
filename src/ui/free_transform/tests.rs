use super::super::workspace_tests as workspace;
use super::*;

const SIZE: Vec2 = Vec2::new(1360., 900.);
fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    workspace::draw(app, ctx, SIZE, events)
}
fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}
fn pointer(pos: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}
fn open(app: &mut PeerBrush, ctx: &egui::Context) {
    frame(app, ctx, vec![]);
    workspace::key(app, ctx, SIZE, egui::Key::T, egui::Modifiers::COMMAND);
    assert!(app.free_transform.is_some(), "{}", app.message);
}
fn settle(app: &mut PeerBrush, ctx: &egui::Context) -> egui::FullOutput {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        frame(app, ctx, vec![]);
        let ready = app.free_transform.as_ref().is_some_and(|editor| {
            editor
                .key()
                .ok()
                .is_some_and(|key| editor.rendered.as_ref() == Some(&key))
        });
        if ready && !app.pending && app.canvas_preview == app.last_preview {
            return frame(app, ctx, vec![]);
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Free Transform preview did not finish: {}",
            app.message
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn actual_preview(app: &PeerBrush, output: &egui::FullOutput) {
    let doc = app.shared.lock().unwrap().doc.clone();
    let editor = app.free_transform.as_ref().unwrap();
    let commands = editor.commands().unwrap();
    assert_eq!(commands.len(), editor.roots.len());
    let expected = crate::engine::Engine::preview_edits(doc, &commands).unwrap();
    let (w, h, bytes, _) = expected.preview(None, 1536, None, false).unwrap();
    assert_eq!(app.canvas_pixels.as_ref(), Some(&(w, h, bytes)));
    let texture = app.texture.as_ref().unwrap().id();
    assert!(
        output.shapes.iter().any(
            |shape| matches!(&shape.shape,egui::Shape::Mesh(mesh) if mesh.texture_id==texture)
        ),
        "Shared-engine result must be painted into the actual canvas mesh"
    );
}

#[test]
fn affine_session_uses_actual_controls_pixels_and_one_native_commit_across_modes() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = workspace::source(&app);
        let prior_undo = app.shared.lock().unwrap().undo.len();
        open(&mut app, &ctx);
        settle(&mut app, &ctx);
        // Real unboxed numeric editing; Enter completes the field, not the session.
        let at = app.free_transform.as_ref().unwrap().inputs[0]
            .unwrap()
            .center();
        workspace::click(&mut app, &ctx, SIZE, at);
        workspace::click(&mut app, &ctx, SIZE, at);
        frame(
            &mut app,
            &ctx,
            vec![
                key_event(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text("6".into()),
            ],
        );
        frame(
            &mut app,
            &ctx,
            vec![key_event(egui::Key::Enter, Default::default())],
        );
        assert!(app.free_transform.is_some(), "Numeric Enter must not Apply");
        assert_eq!(workspace::source(&app), before);
        assert_eq!(app.free_transform.as_ref().unwrap().values.offset[0], 6.);
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        workspace::key(&mut app, &ctx, SIZE, egui::Key::E, Default::default());
        assert!(app.free_transform.as_ref().unwrap().mode == Tool::Rotate);
        workspace::key(&mut app, &ctx, SIZE, egui::Key::R, Default::default());
        assert!(app.free_transform.as_ref().unwrap().mode == Tool::Scale);
        let editor = app.free_transform.as_ref().unwrap();
        let corner = editor.corners()[2];
        let pivot = editor.point(editor.pivot);
        let start = app.view.point(
            corner.map(|v| v as f32),
            app.view_rect.unwrap(),
            app.view_rect.unwrap().width() / 32.,
        );
        let end_doc = [
            pivot[0] + (corner[0] - pivot[0]) * 1.2,
            pivot[1] + (corner[1] - pivot[1]) * 1.2,
        ];
        let end = app.view.point(
            end_doc.map(|v| v as f32),
            app.view_rect.unwrap(),
            app.view_rect.unwrap().width() / 32.,
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(start), pointer(start, true)],
        );
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
        frame(&mut app, &ctx, vec![pointer(end, false)]);
        assert!(app.free_transform.as_ref().unwrap().values.scale[0] > 1.);
        assert_eq!(
            workspace::source(&app),
            before,
            "Handle release must retain a pending session"
        );
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        let flip = workspace::label(&output, "Flip H");
        workspace::click(&mut app, &ctx, SIZE, flip);
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        let commands = app.free_transform.as_ref().unwrap().commands().unwrap();
        assert_eq!(
            commands.len(),
            1,
            "All mode changes must retain one original-sampling command"
        );
        assert_eq!(commands[0]["op"], "transform");
        assert_eq!(commands[0]["dx"], 6.);
        let expected =
            crate::engine::Engine::preview_edits(app.shared.lock().unwrap().doc.clone(), &commands)
                .unwrap();
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
        assert!(app.free_transform.is_none(), "{}", app.message);
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.undo.len(), prior_undo + 1);
        assert_eq!(engine.doc.bit_depth, depth);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        engine.undo("human").unwrap();
        assert_eq!(
            engine.doc.layers[0].pixels.rgba16(),
            serde_json::from_value::<Document>(before["document"].clone())
                .unwrap()
                .layers[0]
                .pixels
                .rgba16()
        );
        assert_eq!(
            engine.doc.export_png().unwrap(),
            before["png"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap() as u8)
                .collect::<Vec<_>>()
        );
        engine.redo("human").unwrap();
        assert_eq!(
            engine.doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
    }
}

#[test]
fn actual_menu_cancel_and_held_pointer_escape_do_not_replay_brush_or_destructive_keys() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = workspace::source(&app);
        let output = frame(&mut app, &ctx, vec![]);
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Layer"));
        let output = frame(&mut app, &ctx, vec![]);
        workspace::click(
            &mut app,
            &ctx,
            SIZE,
            workspace::label(&output, "Free Transform..."),
        );
        let output = settle(&mut app, &ctx);
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Cancel"));
        assert!(app.free_transform.is_none());
        assert_eq!(workspace::source(&app), before);
        open(&mut app, &ctx);
        app.free_transform.as_mut().unwrap().values.offset = [3., 0.];
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        let editor = app.free_transform.as_ref().unwrap();
        let point = editor.point([
            editor.bounds[0] + (editor.bounds[2] - editor.bounds[0]) * 0.7,
            editor.bounds[1] + (editor.bounds[3] - editor.bounds[1]) * 0.7,
        ]);
        let point = app.view.point(
            point.map(|v| v as f32),
            app.view_rect.unwrap(),
            app.view_rect.unwrap().width() / 32.,
        );
        let screen_bounds = Rect::from_points(&editor.corners().map(|corner| {
            app.view.point(
                corner.map(|value| value as f32),
                app.view_rect.unwrap(),
                app.view_rect.unwrap().width() / 32.,
            )
        }));
        assert!(
            screen_bounds.contains(point),
            "Drag fixture must press inside the artwork"
        );
        assert_eq!(
            ctx.layer_id_at(point).map(|layer| layer.order),
            Some(egui::Order::Background),
            "Drag fixture must choose visible canvas outside floating controls"
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(point), pointer(point, true)],
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(point + Vec2::new(20., 10.))],
        );
        assert!(app.free_transform.as_ref().unwrap().drag.is_some());
        frame(
            &mut app,
            &ctx,
            vec![
                key_event(egui::Key::Escape, Default::default()),
                key_event(egui::Key::Delete, Default::default()),
            ],
        );
        assert!(app.free_transform.is_none());
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(point + Vec2::new(30., 20.))],
        );
        frame(&mut app, &ctx, vec![pointer(point, false)]);
        assert_eq!(
            workspace::source(&app),
            before,
            "Cancel through release must not paint, clear or commit the draft"
        );
        let mut repeat = key_event(egui::Key::Delete, Default::default());
        if let egui::Event::Key { repeat, .. } = &mut repeat {
            *repeat = true;
        }
        frame(&mut app, &ctx, vec![repeat]);
        assert_eq!(
            workspace::source(&app),
            before,
            "A held destructive key requires a fresh retry"
        );
    }
}

#[test]
fn stale_preview_focus_target_source_and_reserved_apply_keep_exact_human_work() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        open(&mut app, &ctx);
        app.free_transform.as_mut().unwrap().values.offset = [5., 0.];
        settle(&mut app, &ctx);
        let before = workspace::source(&app);
        let id = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .reserve(
                "other",
                "Human artwork",
                vec![crate::engine::Scope::layer(&id)],
            )
            .unwrap();
        let output = frame(&mut app, &ctx, vec![]);
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
        assert!(
            app.free_transform.is_some(),
            "Reservation failure must retain pending values"
        );
        assert_eq!(workspace::source(&app), before);
        app.shared.lock().unwrap().leases.clear();
        let editor = app.free_transform.as_mut().unwrap();
        editor.values.angle = 15.;
        editor.changed();
        app.transient = editor.commands().unwrap();
        let doc = app.shared.lock().unwrap().doc.clone();
        let live = app.live_key();
        while app.preview_rx.try_recv().is_ok() {}
        app.pending = true;
        app.preview_tx
            .send(Preview {
                coarse: false,
                error: None,
                live,
                revision: doc.revision,
                doc_id: doc.id.clone(),
                w: 1,
                h: 1,
                bytes: vec![1, 2, 3, 255],
                dirty: None,
                selection: doc.selection,
                selection_polygon: doc.selection_polygon.clone(),
                selection_coverage: doc.selection_coverage.clone(),
                target: "old-session-parameters".into(),
                mask: false,
            })
            .unwrap();
        app.request_preview(&ctx, &doc);
        assert!(
            app.free_transform.as_ref().unwrap().rendered.is_none(),
            "An older same-session result cannot enable Apply"
        );
        assert_ne!(
            app.canvas_pixels.as_ref().map(|v| &v.2),
            Some(&vec![1, 2, 3, 255])
        );
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":id,"name":"Later human work"})],
                None,
                None,
                "Later human work",
            )
            .unwrap();
        let later = workspace::source(&app);
        frame(
            &mut app,
            &ctx,
            vec![key_event(egui::Key::Enter, Default::default())],
        );
        assert!(app.free_transform.is_none());
        assert_eq!(
            workspace::source(&app),
            later,
            "Source changes must cancel without rebasing or committing"
        );
        open(&mut app, &ctx);
        let before = workspace::source(&app);
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
                focused: false,
                events: vec![
                    egui::Event::WindowFocused(false),
                    key_event(egui::Key::Enter, Default::default()),
                ],
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        );
        assert!(app.free_transform.is_none());
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn pending_session_blocks_save_close_update_and_ime_shortcuts_without_source_changes() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        open(&mut app, &ctx);
        settle(&mut app, &ctx);
        let before = workspace::source(&app);
        app.save(false);
        assert!(!app.busy && app.message.contains("Apply or cancel"));
        app.request_close(lifecycle::Intent::Exit);
        assert!(app.lifecycle.is_none() && !app.exit_approved);
        assert!(app.update_restart_blocker().is_some());
        let at = app.free_transform.as_ref().unwrap().inputs[0]
            .unwrap()
            .center();
        workspace::click(&mut app, &ctx, SIZE, at);
        workspace::click(&mut app, &ctx, SIZE, at);
        frame(
            &mut app,
            &ctx,
            vec![
                egui::Event::Ime(egui::ImeEvent::Enabled),
                egui::Event::Ime(egui::ImeEvent::Preedit("6".into())),
                key_event(egui::Key::Enter, Default::default()),
                key_event(egui::Key::Delete, Default::default()),
                key_event(egui::Key::Z, egui::Modifiers::COMMAND),
                key_event(egui::Key::W, Default::default()),
            ],
        );
        assert!(
            app.free_transform.is_some(),
            "IME/text Enter must not Apply or run tools"
        );
        assert_eq!(workspace::source(&app), before);
        app.cancel_free_transform();
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn selection_choice_pivot_reset_and_pure_move_keep_one_command_and_explicit_bounds() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        {
            let mut engine = app.shared.lock().unwrap();
            engine
                .edit(
                    "human",
                    &[json!({"op":"selection","rect":[4,4,13,13],"feather":1.})],
                    None,
                    None,
                    "Selection",
                )
                .unwrap();
        }
        open(&mut app, &ctx);
        let before = workspace::source(&app);
        let editor = app.free_transform.as_mut().unwrap();
        editor.values.offset = [4., 0.];
        assert_eq!(
            editor.commands().unwrap()[0]["op"],
            "move",
            "Pure movement must not rerender missing fonts"
        );
        editor.values.skew = [15., 0.];
        assert!(editor.commands().is_err());
        let output = frame(&mut app, &ctx, vec![]);
        workspace::click(
            &mut app,
            &ctx,
            SIZE,
            workspace::label(&output, "Whole target (keep selection)"),
        );
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        let editor = app.free_transform.as_ref().unwrap();
        let commands = editor.commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["op"], "transform");
        assert_eq!(commands[0]["selection_only"], false);
        assert!(editor.values != Parameters::default());
        let reference = editor.pivot;
        workspace::click(
            &mut app,
            &ctx,
            SIZE,
            workspace::label(&output, "Reset transform"),
        );
        settle(&mut app, &ctx);
        assert_eq!(app.free_transform.as_ref().unwrap().pivot, reference);
        assert!(app.free_transform.as_ref().unwrap().values == Parameters::default());
        app.cancel_free_transform();
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn space_navigation_does_not_also_transform_pending_artwork() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        open(&mut app, &ctx);
        app.free_transform.as_mut().unwrap().values.offset = [3., 0.];
        settle(&mut app, &ctx);
        let before = workspace::source(&app);
        let values = app.free_transform.as_ref().unwrap().values.clone();
        let at = app.view_rect.unwrap().center() + Vec2::new(10., 10.);
        frame(
            &mut app,
            &ctx,
            vec![
                key_event(egui::Key::Space, Default::default()),
                egui::Event::PointerMoved(at),
                pointer(at, true),
            ],
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(at + Vec2::new(40., 20.))],
        );
        frame(&mut app, &ctx, vec![pointer(at, false)]);
        let mut released = key_event(egui::Key::Space, Default::default());
        if let egui::Event::Key { pressed, .. } = &mut released {
            *pressed = false;
        }
        frame(&mut app, &ctx, vec![released]);
        assert!(app.free_transform.as_ref().unwrap().values == values);
        assert!(app.free_transform.as_ref().unwrap().drag.is_none());
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn existing_live_tool_and_file_loading_guards_refuse_session_without_discarding_work() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        frame(&mut app, &ctx, vec![]);
        let doc = app.shared.lock().unwrap().doc.clone();
        let before = workspace::source(&app);
        app.live_gesture = Some("existing-tool-gesture".into());
        app.open_free_transform(&ctx, &doc);
        assert!(app.free_transform.is_none());
        assert_eq!(app.live_gesture.as_deref(), Some("existing-tool-gesture"));
        app.live_gesture = None;
        app.load_control = Some(crate::loading::Control::default());
        app.open_free_transform(&ctx, &doc);
        assert!(app.free_transform.is_none() && app.load_control.is_some());
        assert_eq!(workspace::source(&app), before);
    }
}
