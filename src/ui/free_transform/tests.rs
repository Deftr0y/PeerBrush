use super::super::workspace_tests as workspace;
use super::*;

fn independent_coverage_eq(
    actual: Option<&crate::selection::Coverage>,
    expected: Option<&crate::selection::Coverage>,
) -> bool {
    let canonical = |contours: &[Vec<[f32; 2]>]| {
        let mut paths = contours
            .iter()
            .map(|points| {
                let mut path = points
                    .iter()
                    .map(|point| point.map(f32::to_bits))
                    .collect::<Vec<_>>();
                let start = (0..path.len())
                    .min_by(|&a, &b| {
                        path[a..]
                            .iter()
                            .chain(&path[..a])
                            .cmp(path[b..].iter().chain(&path[..b]))
                    })
                    .unwrap_or(0);
                path.rotate_left(start);
                path
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    };
    match (actual, expected) {
        (Some(a), Some(b)) => {
            a.mask == b.mask
                && a.bounds == b.bounds
                && a.origin == b.origin
                && canonical(&a.contours) == canonical(&b.contours)
        }
        (None, None) => true,
        _ => false,
    }
}

#[test]
fn independent_coverage_comparison_changes_only_closed_loop_start_and_order() {
    let base = crate::selection::Coverage {
        mask: crate::raster::Raster::new(4, 4),
        bounds: [0, 0, 4, 4],
        origin: [0, 0],
        contours: vec![
            vec![[0., 0.], [4., 0.], [4., 4.], [0., 4.]],
            vec![[1., 1.], [1., 2.], [2., 2.], [2., 1.]],
            vec![[0., 0.], [1., 0.], [0., 0.], [0., 1.]],
        ],
    };
    let mut rotated = base.clone();
    for (index, path) in rotated.contours.iter_mut().enumerate() {
        path.rotate_left(index + 1);
    }
    rotated.contours.reverse();
    assert!(independent_coverage_eq(Some(&base), Some(&rotated)));
    assert!(independent_coverage_eq(None, None));
    assert!(!independent_coverage_eq(Some(&base), None));
    for change in 0..7 {
        let mut different = base.clone();
        match change {
            0 => different.mask.set(0, 0, [1; 4]),
            1 => different.bounds[2] -= 1,
            2 => different.origin[0] += 1,
            3 => different.contours[0][0][0] = -0.,
            4 => different.contours[0].reverse(),
            5 => {
                different.contours.pop();
            }
            6 => different.contours[0].swap(1, 2),
            _ => unreachable!(),
        }
        assert!(
            !independent_coverage_eq(Some(&base), Some(&different)),
            "Preserve difference {change}"
        );
    }
}

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
                job: app.preview_job,
                parameter: None,
                parameter_document: None,
                parameter_replayed: false,
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
        assert_eq!(editor.commands().unwrap()[0]["selection_only"], true);
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

#[test]
fn selected_skew_numeric_control_previews_native_result_then_applies_once_and_undo_restores_coverage(
) {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"selection","rect":[4,4,13,13],"feather":1.})],
                None,
                None,
                "Soft selection",
            )
            .unwrap();
        let before = app.shared.lock().unwrap().doc.clone();
        let history = app.shared.lock().unwrap().undo.len();
        open(&mut app, &ctx);
        settle(&mut app, &ctx);
        let at = app.free_transform.as_ref().unwrap().inputs[5]
            .unwrap()
            .center();
        workspace::click(&mut app, &ctx, SIZE, at);
        workspace::click(&mut app, &ctx, SIZE, at);
        frame(
            &mut app,
            &ctx,
            vec![
                key_event(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text("45".into()),
            ],
        );
        frame(
            &mut app,
            &ctx,
            vec![key_event(egui::Key::Enter, Default::default())],
        );
        assert!(
            app.free_transform.is_some(),
            "A numeric Enter does not Apply"
        );
        assert_eq!(app.free_transform.as_ref().unwrap().values.skew[0], 45.);
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        let commands = app.free_transform.as_ref().unwrap().commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["selection_only"], true);
        let expected = crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
        assert_eq!(
            app.shared.lock().unwrap().doc.layers[0].pixels,
            before.layers[0].pixels
        );
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
        assert!(app.free_transform.is_none(), "{}", app.message);
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.doc.bit_depth, depth);
        assert_eq!(engine.undo.len(), history + 1);
        assert_eq!(engine.doc.layers[0].pixels, expected.layers[0].pixels);
        assert!(independent_coverage_eq(
            engine.doc.selection_coverage.as_ref(),
            expected.selection_coverage.as_ref()
        ));
        let applied_coverage = engine.doc.selection_coverage.clone();
        assert_eq!(
            engine.doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
        assert_eq!(engine.doc.selection_coverage, before.selection_coverage);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
        engine.redo("human").unwrap();
        assert_eq!(engine.doc.layers[0].pixels, expected.layers[0].pixels);
        assert_eq!(engine.doc.selection_coverage, applied_coverage);
        assert!(independent_coverage_eq(
            engine.doc.selection_coverage.as_ref(),
            expected.selection_coverage.as_ref()
        ));
    }
}

fn enter_reference_value(app: &mut PeerBrush, ctx: &egui::Context, index: usize, text: &str) {
    let at = app.free_transform.as_ref().unwrap().inputs[index]
        .unwrap()
        .center();
    workspace::click(app, ctx, SIZE, at);
    workspace::click(app, ctx, SIZE, at);
    frame(
        app,
        ctx,
        vec![
            key_event(egui::Key::A, egui::Modifiers::COMMAND),
            egui::Event::Text(text.into()),
        ],
    );
    frame(
        app,
        ctx,
        vec![key_event(egui::Key::Enter, Default::default())],
    );
    assert!(app.free_transform.is_some(), "Numeric Enter must not Apply");
}

fn reference_mask_eq(actual: &Option<crate::engine::Mask>, expected: &Option<crate::engine::Mask>) {
    let (actual, expected) = (actual.as_ref().unwrap(), expected.as_ref().unwrap());
    assert_eq!(
        (actual.enabled, actual.linked),
        (expected.enabled, expected.linked)
    );
    assert_eq!(actual.steps.len(), expected.steps.len());
    for (a, b) in actual.steps.iter().zip(&expected.steps) {
        assert_eq!(
            (
                &a.id,
                &a.kind,
                a.enabled,
                a.weight.to_bits(),
                a.value.to_bits(),
                &a.settings
            ),
            (
                &b.id,
                &b.kind,
                b.enabled,
                b.weight.to_bits(),
                b.value.to_bits(),
                &b.settings
            )
        );
        assert_eq!(a.pixels, b.pixels);
    }
}

#[test]
fn moved_reference_preserves_exact_command_then_anchors_the_next_affine_change() {
    let (mut app, ctx) = workspace::fixture(16);
    open(&mut app, &ctx);
    let editor = app.free_transform.as_mut().unwrap();
    editor.pivot = [0., 0.];
    editor.values.scale = [2., 1.];
    let original = editor.commands().unwrap();
    let point = editor.point([3.25, 4.5]);
    editor.set_reference([0.5, 0.5]).unwrap();
    assert_eq!(editor.commands().unwrap(), original);
    assert_eq!(
        editor.point([3.25, 4.5]).map(f64::to_bits),
        point.map(f64::to_bits)
    );
    editor.values.scale = [1., 1.];
    let command = editor.commands().unwrap().pop().unwrap();
    assert_eq!(
        command["op"], "transform",
        "Identity plus fractional placement is not an integer Move"
    );
    assert_eq!(command["fractional_translation"], true);
    assert_eq!(command["dx"], 0.25);
    assert_eq!(command["dy"], 0.);
    assert_eq!(editor.point([0.25, 0.5]), [0.5, 0.5]);

    editor.reference = None;
    editor.pivot = [13., 9.];
    editor.values = Parameters {
        offset: [4., -2.],
        scale: [1.3, 0.8],
        angle: 27.,
        skew: [9., -4.],
        mirror: [true, false],
    };
    for reference in [[9.125, 11.5], [12.25, 7.75]] {
        let commands = editor.commands().unwrap();
        let key = editor.key().unwrap();
        let points = [[-3., 8.], [0., 0.], [24., 20.]].map(|p| editor.point(p).map(f64::to_bits));
        editor.set_reference(reference).unwrap();
        assert_eq!(editor.commands().unwrap(), commands);
        assert_eq!(editor.key().unwrap(), key);
        assert_eq!(
            [[-3., 8.], [0., 0.], [24., 20.]].map(|p| editor.point(p).map(f64::to_bits)),
            points
        );
    }
    let anchor = editor.reference.as_ref().unwrap();
    let [a, b, c, d, tx, ty] = anchor.matrix;
    let [qx, qy] = anchor.point;
    let det = a * d - b * c;
    let source = [
        (d * (qx - tx) - c * (qy - ty)) / det,
        (-b * (qx - tx) + a * (qy - ty)) / det,
    ];
    editor.values.angle += 19.;
    editor.values.scale = [0.7, 1.6];
    let mapped = editor.point(source);
    assert!((mapped[0] - qx).abs() < 1e-10 && (mapped[1] - qy).abs() < 1e-10);
    assert_eq!(editor.commands().unwrap().len(), 1);
    editor.values.offset[0] += 3.;
    assert_eq!(editor.reference_point(), [qx + 3., qy]);
    let commands = editor.commands().unwrap();
    assert!(editor.set_reference([100001., 0.]).is_err());
    assert_eq!(editor.commands().unwrap(), commands);
}

#[test]
fn actual_reference_fields_keep_painted_result_then_rotate_once_with_native_masks_and_selection() {
    for depth in [8, 16] {
        for selected in [false, true] {
            let (mut app, ctx) = workspace::fixture(depth);
            {
                let mut engine = app.shared.lock().unwrap();
                let id = app.selected.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"mask.add","layer":id})],
                        None,
                        None,
                        "Mask",
                    )
                    .unwrap();
                engine.doc.layers[0].mask.as_mut().unwrap().steps[1]
                    .pixels
                    .set16(8, 8, [23459, 23459, 23459, 65535]);
                if selected {
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"selection","rect":[4,4,13,13],"feather":1.})],
                            None,
                            None,
                            "Soft selection",
                        )
                        .unwrap();
                }
            }
            let before = workspace::source(&app);
            let original = app.shared.lock().unwrap().doc.clone();
            let history = app.shared.lock().unwrap().undo.len();
            open(&mut app, &ctx);
            app.free_transform.as_mut().unwrap().values.scale = [1.2, 0.9];
            let output = settle(&mut app, &ctx);
            actual_preview(&app, &output);
            let commands = app.free_transform.as_ref().unwrap().commands().unwrap();
            let key = app.free_transform.as_ref().unwrap().key().unwrap();
            let pixels = app.canvas_pixels.clone();
            let generation = app.canvas_generation;
            enter_reference_value(&mut app, &ctx, 7, "12.25");
            enter_reference_value(&mut app, &ctx, 8, "12.5");
            let output = settle(&mut app, &ctx);
            actual_preview(&app, &output);
            let editor = app.free_transform.as_ref().unwrap();
            assert_eq!(editor.reference_point(), [12.25, 12.5]);
            assert_eq!(editor.commands().unwrap(), commands);
            assert_eq!(editor.key().unwrap(), key);
            assert_eq!(editor.rendered.as_ref(), Some(&key));
            assert_eq!(app.canvas_pixels, pixels);
            assert_eq!(app.canvas_generation, generation);
            assert_eq!(workspace::source(&app), before);
            let [a, b, c, d, tx, ty] = Editor::matrix(&commands[0]).unwrap();
            let det = a * d - b * c;
            let [qx, qy] = [12.25, 12.5];
            let source = [
                (d * (qx - tx) - c * (qy - ty)) / det,
                (-b * (qx - tx) + a * (qy - ty)) / det,
            ];
            enter_reference_value(&mut app, &ctx, 4, "23");
            let output = settle(&mut app, &ctx);
            actual_preview(&app, &output);
            let editor = app.free_transform.as_ref().unwrap();
            let point = editor.point(source);
            assert!((point[0] - qx).abs() < 1e-10 && (point[1] - qy).abs() < 1e-10);
            let commands = editor.commands().unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0]["selection_only"], selected);
            assert_eq!(commands[0]["fractional_translation"], true);
            assert_eq!(workspace::source(&app), before);
            let expected =
                crate::engine::Engine::preview_edits(original.clone(), &commands).unwrap();
            workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
            assert!(app.free_transform.is_none(), "{}", app.message);
            let mut engine = app.shared.lock().unwrap();
            assert_eq!(engine.undo.len(), history + 1);
            assert_eq!(engine.doc.bit_depth, depth);
            assert_eq!(engine.doc.layers[0].pixels, expected.layers[0].pixels);
            reference_mask_eq(&engine.doc.layers[0].mask, &expected.layers[0].mask);
            assert!(independent_coverage_eq(
                engine.doc.selection_coverage.as_ref(),
                expected.selection_coverage.as_ref()
            ));
            assert_eq!(
                engine.doc.export_png().unwrap(),
                expected.export_png().unwrap()
            );
            let applied = engine.doc.clone();
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.layers[0].pixels, original.layers[0].pixels);
            reference_mask_eq(&engine.doc.layers[0].mask, &original.layers[0].mask);
            assert_eq!(engine.doc.selection_coverage, original.selection_coverage);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                original.export_png().unwrap()
            );
            engine.redo("human").unwrap();
            assert_eq!(engine.doc.layers[0].pixels, applied.layers[0].pixels);
            reference_mask_eq(&engine.doc.layers[0].mask, &applied.layers[0].mask);
            assert_eq!(engine.doc.selection_coverage, applied.selection_coverage);
        }
    }
}

#[test]
fn actual_reference_handle_after_affine_is_presentation_only_and_escape_preserves_human_work() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = workspace::source(&app);
        open(&mut app, &ctx);
        app.free_transform.as_mut().unwrap().values.angle = 15.;
        settle(&mut app, &ctx);
        // The reference may be outside the artwork; choose visible, unoccluded canvas.
        let rect = app.view_rect.unwrap();
        let scale = rect.width() / 32.;
        let start_doc = [24., 18.];
        app.free_transform
            .as_mut()
            .unwrap()
            .set_reference(start_doc)
            .unwrap();
        settle(&mut app, &ctx);
        let start = app.view.point(start_doc.map(|v| v as f32), rect, scale);
        let end = app.view.point([25., 19.], rect, scale);
        assert!(rect.contains(start) && rect.contains(end));
        assert_eq!(
            ctx.layer_id_at(start).map(|layer| layer.order),
            Some(egui::Order::Background)
        );
        let commands = app.free_transform.as_ref().unwrap().commands().unwrap();
        let pixels = app.canvas_pixels.clone();
        let generation = app.canvas_generation;
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(start), pointer(start, true)],
        );
        assert!(matches!(
            app.free_transform
                .as_ref()
                .unwrap()
                .drag
                .as_ref()
                .map(|drag| drag.handle),
            Some(Handle::Pivot)
        ));
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
        let editor = app.free_transform.as_ref().unwrap();
        assert_eq!(editor.reference_point(), [25., 19.]);
        assert_eq!(editor.commands().unwrap(), commands);
        assert_eq!(app.canvas_pixels, pixels);
        assert_eq!(app.canvas_generation, generation);
        assert_eq!(workspace::source(&app), before);
        frame(
            &mut app,
            &ctx,
            vec![
                key_event(egui::Key::Escape, Default::default()),
                key_event(egui::Key::Delete, Default::default()),
            ],
        );
        assert!(app.free_transform.is_none());
        frame(&mut app, &ctx, vec![pointer(end, false)]);
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn scale_crossing_uses_signed_axes_about_the_reference_with_rotation_skew_and_prior_mirrors() {
    let cases = [
        ([-1.25, 0.75], false, false, [true, true], [-1.25, 0.75]),
        ([1.25, -0.75], false, false, [true, true], [1.25, -0.75]),
        ([-1.25, -0.75], false, false, [true, true], [-1.25, -0.75]),
        ([-0.75, 1.5], true, false, [true, true], [-1.5, 1.5]),
        ([-1.25, 1.], false, true, [true, false], [-1.25, 1.25]),
    ];
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = workspace::source(&app);
        open(&mut app, &ctx);
        for mirrors in [[false, false], [true, false], [false, true], [true, true]] {
            for (input, proportional, shift, axes, expected) in cases {
                let expected: [f64; 2] = expected;
                let editor = app.free_transform.as_mut().unwrap();
                editor.reference = None;
                editor.values = Parameters {
                    scale: [1.6, 0.8],
                    angle: 30.,
                    skew: [17., -9.],
                    mirror: mirrors,
                    ..Default::default()
                };
                editor.proportional = proportional;
                editor.snap = false;
                editor.set_reference([5.25, 21.75]).unwrap();
                let start = editor.values.clone();
                let key = editor.key().unwrap();
                let center = editor.reference_point();
                let origin = editor.corners()[2];
                let (sin, cos) = start.angle.to_radians().sin_cos();
                let [dx, dy] = [origin[0] - center[0], origin[1] - center[1]];
                let local = [dx * cos + dy * sin, -dx * sin + dy * cos];
                let position = |f: [f64; 2]| {
                    [
                        center[0] + cos * local[0] * f[0] - sin * local[1] * f[1],
                        center[1] + sin * local[0] * f[0] + cos * local[1] * f[1],
                    ]
                };
                editor.drag = Some(Drag {
                    origin,
                    start: start.clone(),
                    handle: Handle::Scale(axes[0], axes[1]),
                });
                editor.drag_to(position(input), shift);
                for i in 0..2 {
                    assert_eq!(editor.values.mirror[i], mirrors[i] ^ (expected[i] < 0.));
                    assert!(
                        (editor.values.scale[i] - start.scale[i] * expected[i].abs()).abs() < 1e-10
                    );
                    assert!((editor.corners()[2][i] - position(expected)[i]).abs() < 1e-7);
                }
                assert_eq!(editor.reference_point(), center);
                assert!(editor.commands().is_ok());
                editor.drag_to(origin, false);
                assert_eq!(
                    editor.key().unwrap(),
                    key,
                    "Crossing back must restore the starting command exactly"
                );
            }
        }
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn real_corner_crossing_previews_one_native_flip_then_commits_and_roundtrips_once() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = workspace::source(&app);
        let original = app.shared.lock().unwrap().doc.clone();
        let undo = app.shared.lock().unwrap().undo.len();
        open(&mut app, &ctx);
        settle(&mut app, &ctx);
        workspace::key(&mut app, &ctx, SIZE, egui::Key::R, Default::default());
        let editor = app.free_transform.as_ref().unwrap();
        let corner = editor.corners()[2];
        let center = editor.reference_point();
        let reflected = [2. * center[0] - corner[0], corner[1]];
        let rect = app.view_rect.unwrap();
        let screen = |point: [f64; 2]| {
            app.view
                .point(point.map(|v| v as f32), rect, rect.width() / 32.)
        };
        let start = screen(corner);
        let end = screen(reflected);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(start), pointer(start, true)],
        );
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
        assert_eq!(
            app.free_transform.as_ref().unwrap().values.mirror,
            [true, false]
        );
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(start)]);
        assert_eq!(
            app.free_transform.as_ref().unwrap().values.mirror,
            [false, false]
        );
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
        frame(&mut app, &ctx, vec![pointer(end, false)]);
        assert_eq!(
            workspace::source(&app),
            before,
            "Crossing and release stay pending"
        );
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        let commands = app.free_transform.as_ref().unwrap().commands().unwrap();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0]["mirror_x"], true);
        assert_eq!(commands[0]["mirror_y"], false);
        let expected = crate::engine::Engine::preview_edits(original.clone(), &commands).unwrap();
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
        assert!(app.free_transform.is_none(), "{}", app.message);
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.undo.len(), undo + 1);
        assert_eq!(engine.doc.bit_depth, depth);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        for encoded in [
            crate::psd::encode(&engine.doc).unwrap(),
            crate::psd::encode_psb(&engine.doc).unwrap(),
        ] {
            let reopened = crate::psd::decode(&encoded).unwrap();
            assert!(!reopened.read_only, "Roundtrip must retain editability");
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(
                reopened.export_png().unwrap(),
                engine.doc.export_png().unwrap()
            );
            assert_eq!(
                reopened.layers[0].pixels.rgba16(),
                engine.doc.layers[0].pixels.rgba16()
            );
        }
        engine.undo("human").unwrap();
        assert_eq!(
            engine.doc.export_png().unwrap(),
            original.export_png().unwrap()
        );
        assert_eq!(
            engine.doc.layers[0].pixels.rgba16(),
            original.layers[0].pixels.rgba16()
        );
    }
}

#[test]
fn proportional_zero_magnitude_preserves_the_other_axis_crossing_and_never_flips_zero() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = workspace::source(&app);
        open(&mut app, &ctx);
        for (ratio, mirror) in [
            ([-0.5, 0.], [true, false]),
            ([0., -0.5], [false, true]),
            ([-0., 0.], [false, false]),
            ([-0.5, -0.], [true, false]),
        ] {
            let editor = app.free_transform.as_mut().unwrap();
            editor.reference = None;
            editor.values = Parameters::default();
            editor.proportional = true;
            editor.snap = false;
            let origin = editor.corners()[2];
            let center = editor.reference_point();
            editor.drag = Some(Drag {
                origin,
                start: editor.values.clone(),
                handle: Handle::Scale(true, true),
            });
            editor.drag_to(
                [
                    center[0] + (origin[0] - center[0]) * ratio[0],
                    center[1] + (origin[1] - center[1]) * ratio[1],
                ],
                false,
            );
            assert_eq!(editor.values.scale, [0.05; 2]);
            assert_eq!(editor.values.mirror, mirror);
            assert!(editor.commands().is_ok());
        }
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn edge_skew_follows_the_actual_sheared_axes_and_reference_and_restores_exact_commands() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        open(&mut app, &ctx);
        let before = workspace::source(&app);
        for mirrored in [[false, false], [true, false], [false, true], [true, true]] {
            for axis in [0, 1] {
                for snap in [false, true] {
                    let editor = app.free_transform.as_mut().unwrap();
                    editor.reference = None;
                    editor.values = Parameters {
                        offset: [3., -2.],
                        scale: [1.6, 0.8],
                        angle: 30.,
                        skew: [15., -15.],
                        mirror: mirrored,
                    };
                    editor.snap = snap;
                    editor.set_reference([5.25, 21.75]).unwrap();
                    let start = editor.values.clone();
                    let key = editor.key().unwrap();
                    let b = editor.whole_bounds;
                    let point = if axis == 0 {
                        [(b[0] + b[2]) * 0.5, b[1]]
                    } else {
                        [b[2], (b[1] + b[3]) * 0.5]
                    };
                    let origin = editor.point(point);
                    let center = editor.reference_point();
                    let old = Editor::matrix(&editor.command("", snap).unwrap()).unwrap();
                    let [a, b, c, d, tx, ty] = old;
                    let det = a * d - b * c;
                    // Independently recover the source point at the fixed reference.
                    let anchor = [
                        (d * (center[0] - tx) - c * (center[1] - ty)) / det,
                        (-b * (center[0] - tx) + a * (center[1] - ty)) / det,
                    ];
                    let mut values = start.clone();
                    values.skew[axis] = if axis == 0 { 30. } else { -30. };
                    let (sin, cos) = values.angle.to_radians().sin_cos();
                    let signed = [
                        if mirrored[0] {
                            -values.scale[0]
                        } else {
                            values.scale[0]
                        },
                        if mirrored[1] {
                            -values.scale[1]
                        } else {
                            values.scale[1]
                        },
                    ];
                    let [kx, ky] = values.skew.map(|angle| angle.to_radians().tan());
                    let [mut u, mut v] = [point[0] - anchor[0], point[1] - anchor[1]];
                    u += kx * v;
                    v += ky * u;
                    let target = [
                        center[0] + cos * signed[0] * u - sin * signed[1] * v,
                        center[1] + sin * signed[0] * u + cos * signed[1] * v,
                    ];
                    editor.drag = Some(Drag {
                        origin,
                        start: start.clone(),
                        handle: Handle::Skew(axis),
                    });
                    editor.drag_to(target, false);
                    assert!((editor.values.skew[axis] - values.skew[axis]).abs() < 1e-10);
                    assert_eq!(editor.reference_point(), center);
                    for i in 0..2 {
                        assert!((editor.point(point)[i] - target[i]).abs() < 1e-7);
                    }
                    assert_eq!(editor.values.mirror, mirrored);
                    assert_eq!(editor.values.scale, start.scale);
                    editor.drag_to(origin, false);
                    assert_eq!(
                        editor.key().unwrap(),
                        key,
                        "Returning the handle restores exact pending commands"
                    );
                }
            }
        }
        let editor = app.free_transform.as_mut().unwrap();
        editor.reference = None;
        editor.values = Parameters::default();
        editor.snap = false;
        let [l, t, r, _] = editor.whole_bounds;
        let origin = [(l + r) * 0.5, t];
        let key = editor.key().unwrap();
        editor.drag = Some(Drag {
            origin,
            start: editor.values.clone(),
            handle: Handle::Skew(0),
        });
        let pivot = editor.reference_point();
        editor.drag_to(
            [
                origin[0] + (origin[1] - pivot[1]) * 38_f64.to_radians().tan(),
                origin[1],
            ],
            true,
        );
        assert_eq!(
            editor.values.skew[0], 45.,
            "Shift uses the shared fifteen-degree shear snap"
        );
        assert_eq!(editor.commands().unwrap()[0]["skew_x"], 45.);
        editor.drag_to([origin[0] + 1e6, origin[1]], false);
        assert_eq!(editor.values.skew[0].abs(), crate::transform::SKEW_LIMIT);
        editor.drag_to(origin, false);
        assert_eq!(editor.key().unwrap(), key);
        assert_eq!(workspace::source(&app), before);
    }
}

fn skew_edge_drag(app: &mut PeerBrush, ctx: &egui::Context, axis: usize) {
    let output = frame(app, ctx, vec![]);
    workspace::click(app, ctx, SIZE, workspace::label(&output, "Skew"));
    assert!(app.free_transform.as_ref().unwrap().skew_mode);
    assert!(app.free_transform.as_ref().unwrap().mode().is_none());
    let editor = app.free_transform.as_ref().unwrap();
    let corners = editor.corners();
    let rect = app.view_rect.unwrap();
    let start = [axis, axis + 2]
        .into_iter()
        .map(|edge| {
            let point = [
                (corners[edge][0] + corners[(edge + 1) % 4][0]) * 0.5,
                (corners[edge][1] + corners[(edge + 1) % 4][1]) * 0.5,
            ];
            app.view
                .point(point.map(|v| v as f32), rect, rect.width() / 32.)
        })
        .find(|point| {
            rect.contains(*point)
                && ctx
                    .layer_id_at(*point)
                    .is_some_and(|layer| layer.order == egui::Order::Background)
        })
        .expect("An actual skew edge handle must be visible outside the floating controls");
    let end = start
        + if axis == 0 {
            Vec2::new(18., 0.)
        } else {
            Vec2::new(0., 18.)
        };
    frame(
        app,
        ctx,
        vec![egui::Event::PointerMoved(start), pointer(start, true)],
    );
    assert!(
        matches!(app.free_transform.as_ref().unwrap().drag.as_ref().map(|drag| drag.handle), Some(Handle::Skew(value)) if value == axis)
    );
    frame(app, ctx, vec![egui::Event::PointerMoved(end)]);
    frame(app, ctx, vec![pointer(end, false)]);
    assert!(app.free_transform.as_ref().unwrap().drag.is_none());
}

fn skew_structure_eq(actual: &Document, expected: &Document) {
    assert_eq!(actual.layers.len(), expected.layers.len());
    for (actual, expected) in actual.layers.iter().zip(&expected.layers) {
        assert_eq!(
            (&actual.id, &actual.parent, actual.visible, &actual.kind),
            (
                &expected.id,
                &expected.parent,
                expected.visible,
                &expected.kind
            )
        );
        assert_eq!(actual.mask.is_some(), expected.mask.is_some());
        if let (Some(actual), Some(expected)) = (&actual.mask, &expected.mask) {
            assert_eq!(
                (actual.enabled, actual.linked, actual.steps.len()),
                (expected.enabled, expected.linked, expected.steps.len())
            );
            for (actual, expected) in actual.steps.iter().zip(&expected.steps) {
                assert_eq!(
                    (
                        &actual.id,
                        &actual.kind,
                        actual.enabled,
                        actual.weight.to_bits(),
                        actual.value.to_bits(),
                        &actual.settings
                    ),
                    (
                        &expected.id,
                        &expected.kind,
                        expected.enabled,
                        expected.weight.to_bits(),
                        expected.value.to_bits(),
                        &expected.settings
                    )
                );
            }
        }
    }
}
fn skew_native_layers_eq(actual: &Document, expected: &Document) {
    skew_structure_eq(actual, expected);
    for (actual, expected) in actual.layers.iter().zip(&expected.layers) {
        assert_eq!((actual.x, actual.y), (expected.x, expected.y));
        assert_eq!(actual.pixels, expected.pixels);
        assert_eq!(actual.source, expected.source);
        if actual.mask.is_some() {
            reference_mask_eq(&actual.mask, &expected.mask);
        }
    }
}

#[test]
fn real_skew_handles_share_selected_masks_folders_preview_storage_and_one_guarded_undo() {
    for depth in [8, 16] {
        for target in [
            "linked-color",
            "unlinked-color",
            "unlinked-mask",
            "paint-step",
            "folder",
        ] {
            let (mut app, ctx) = workspace::fixture(depth);
            let id = app.selected.clone();
            {
                let mut engine = app.shared.lock().unwrap();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"mask.add","layer":id})],
                        None,
                        None,
                        "Painted mask",
                    )
                    .unwrap();
                let layer = &mut engine.doc.layers[0];
                for y in 0..24 {
                    for x in 0..32 {
                        layer.pixels.set16(
                            x,
                            y,
                            [
                                10001 + x as u16 * 137,
                                20003 + y as u16 * 193,
                                40007,
                                if (x + y) % 7 == 0 { 0 } else { 49157 },
                            ],
                        );
                        let value = 12347 + x as u16 * 97 + y as u16 * 193;
                        layer.mask.as_mut().unwrap().steps[1].pixels.set16(
                            x,
                            y,
                            [value, value, value, 65535],
                        );
                    }
                }
                if target != "linked-color" && target != "folder" {
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"mask.link","layer":id,"linked":false})],
                            None,
                            None,
                            "Unlink mask",
                        )
                        .unwrap();
                }
                if target == "folder" {
                    let mut root = crate::engine::Layer::new("Skew folder", "group", 32, 24);
                    root.pixels = crate::raster::Raster::new_depth(32, 24, depth);
                    let mut child = engine.doc.layers[0].clone();
                    child.parent = Some(root.id.clone());
                    let mut hidden = child.clone();
                    hidden.id = crate::engine::id();
                    hidden.name = "Hidden native child".into();
                    hidden.visible = false;
                    hidden.pixels.set16(7, 6, [14143, 25253, 36367, 0]);
                    hidden.mask = None;
                    app.selected = root.id.clone();
                    engine.doc.layers = vec![root, child, hidden];
                }
                engine.edit("human", &[
                    json!({"op":"selection","kind":"ellipse","rect":[4,4,26,21],"feather":2.}),
                    json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[11,9,16,14]})
                ], None, None, "Soft selection with hole").unwrap();
            }
            app.selection_layers = [app.selected.clone()].into_iter().collect();
            app.mask = target == "unlinked-mask" || target == "paint-step";
            if target == "paint-step" {
                app.mask_step = Some(
                    app.shared.lock().unwrap().doc.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps[1]
                        .id
                        .clone(),
                );
            }
            let original = app.shared.lock().unwrap().doc.clone();
            let unchanged = workspace::source(&app);
            let history = app.shared.lock().unwrap().undo.len();
            open(&mut app, &ctx);
            settle(&mut app, &ctx);
            skew_edge_drag(&mut app, &ctx, 0);
            let output = settle(&mut app, &ctx);
            actual_preview(&app, &output);
            assert_ne!(app.free_transform.as_ref().unwrap().values.skew[0], 0.);
            skew_edge_drag(&mut app, &ctx, 1);
            let output = settle(&mut app, &ctx);
            actual_preview(&app, &output);
            let commands = app.free_transform.as_ref().unwrap().commands().unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0]["selection_only"], true);
            assert_eq!(commands[0]["mask"], app.mask);
            assert_eq!(commands[0]["step"].as_str(), app.mask_step.as_deref());
            let key = app.free_transform.as_ref().unwrap().key().unwrap();
            workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Move"));
            assert!(!app.free_transform.as_ref().unwrap().skew_mode);
            assert!(matches!(
                app.free_transform.as_ref().unwrap().mode(),
                Some(Tool::Move)
            ));
            assert_eq!(app.free_transform.as_ref().unwrap().key().unwrap(), key);
            assert_eq!(
                workspace::source(&app),
                unchanged,
                "Modes and release retain the same pending source"
            );
            let expected =
                crate::engine::Engine::preview_edits(original.clone(), &commands).unwrap();
            skew_structure_eq(&expected, &original);
            let output = settle(&mut app, &ctx);
            if target == "linked-color" {
                app.shared
                    .lock()
                    .unwrap()
                    .reserve(
                        "other",
                        "Selection owner",
                        vec![crate::engine::Scope::layer("@selection")],
                    )
                    .unwrap();
                workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
                assert!(
                    app.free_transform.is_some(),
                    "Reserved Apply must preserve the actual pending session"
                );
                assert_eq!(workspace::source(&app), unchanged);
                app.shared.lock().unwrap().leases.clear();
            }
            let output = settle(&mut app, &ctx);
            workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
            assert!(app.free_transform.is_none(), "{}", app.message);
            let mut engine = app.shared.lock().unwrap();
            assert_eq!(engine.undo.len(), history + 1);
            skew_native_layers_eq(&engine.doc, &expected);
            assert!(independent_coverage_eq(
                engine.doc.selection_coverage.as_ref(),
                expected.selection_coverage.as_ref()
            ));
            assert_eq!(
                engine.doc.export_png().unwrap(),
                expected.export_png().unwrap()
            );
            for encoded in [
                crate::psd::encode(&engine.doc).unwrap(),
                crate::psd::encode_psb(&engine.doc).unwrap(),
            ] {
                let reopened = crate::psd::decode(&encoded).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.bit_depth, depth);
                assert_eq!(
                    reopened.export_png().unwrap(),
                    engine.doc.export_png().unwrap()
                );
                skew_native_layers_eq(&reopened, &engine.doc);
            }
            let applied = engine.doc.clone();
            engine.undo("human").unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                original.export_png().unwrap()
            );
            assert_eq!(engine.doc.selection_coverage, original.selection_coverage);
            skew_native_layers_eq(&engine.doc, &original);
            engine.redo("human").unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                applied.export_png().unwrap()
            );
            assert_eq!(engine.doc.selection_coverage, applied.selection_coverage);
            skew_native_layers_eq(&engine.doc, &applied);
        }
    }
}

#[test]
fn skew_mode_cancel_and_reference_axis_leave_native_history_exact() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = workspace::source(&app);
        open(&mut app, &ctx);
        settle(&mut app, &ctx);
        let editor = app.free_transform.as_mut().unwrap();
        let b = editor.whole_bounds;
        let origin = [(b[0] + b[2]) * 0.5, b[1]];
        editor.set_reference(origin).unwrap();
        let key = editor.key().unwrap();
        editor.drag = Some(Drag {
            origin,
            start: editor.values.clone(),
            handle: Handle::Skew(0),
        });
        editor.drag_to([origin[0] + 20., origin[1]], false);
        assert_eq!(
            editor.key().unwrap(),
            key,
            "An edge on its reference axis cannot change that shear"
        );
        editor.drag = None;
        editor
            .set_reference([(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5])
            .unwrap();
        // Sparse whole-target content starts beneath the floating controls.
        // Pan presentation into actual free canvas space; retain real hit tests.
        let reference = editor.reference_point();
        let rect = app.view_rect.unwrap();
        let point = app
            .view
            .point(reference.map(|v| v as f32), rect, rect.width() / 32.);
        let controls = ctx
            .memory(|memory| memory.area_rect(egui::Id::new("free transform")))
            .unwrap();
        let available = app.canvas_input_region.unwrap().0;
        let left = controls.right().max(available.left()) + 16.;
        let right = available.right() - 16.;
        assert!(left < right, "The fixture needs unobscured canvas space");
        app.pan.x += (left + right) * 0.5 - point.x;
        skew_edge_drag(&mut app, &ctx, 0);
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        frame(
            &mut app,
            &ctx,
            vec![
                key_event(egui::Key::Escape, Default::default()),
                key_event(egui::Key::Delete, Default::default()),
            ],
        );
        assert!(app.free_transform.is_none());
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn skew_handles_keep_selected_source_protection_and_support_explicit_whole_source_preview() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let source = crate::source::Source {
            width: 32,
            height: 24,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: crate::source::Content::Shape {
                shape: "rectangle".into(),
                bounds: [4., 4., 26., 20.],
                points: vec![],
                closed: true,
                fill: [12347, 33559, 51237, 49157],
                stroke: [0; 4],
                stroke_width: 0.,
            },
        };
        {
            let mut engine = app.shared.lock().unwrap();
            engine
                .edit(
                    "human",
                    &[json!({"op":"source.add","source":source})],
                    None,
                    None,
                    "Editable source",
                )
                .unwrap();
            app.selected = engine.doc.layers[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"selection","rect":[4,4,26,20],"feather":1.})],
                    None,
                    None,
                    "Source selection",
                )
                .unwrap();
        }
        app.selection_layers = [app.selected.clone()].into_iter().collect();
        let before = workspace::source(&app);
        open(&mut app, &ctx);
        settle(&mut app, &ctx);
        skew_edge_drag(&mut app, &ctx, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let output = loop {
            let output = frame(&mut app, &ctx, vec![]);
            if !app.pending && app.free_transform.as_ref().unwrap().error.is_some() {
                break output;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Shared source protection did not arrive"
            );
            std::thread::sleep(Duration::from_millis(2));
        };
        assert!(app
            .free_transform
            .as_ref()
            .unwrap()
            .error
            .as_ref()
            .unwrap()
            .contains("rasterize"));
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Apply"));
        assert!(
            app.free_transform.is_some(),
            "An unsupported selected-source preview cannot enable Apply"
        );
        assert_eq!(workspace::source(&app), before);
        let output = frame(&mut app, &ctx, vec![]);
        workspace::click(
            &mut app,
            &ctx,
            SIZE,
            workspace::label(&output, "Whole target (keep selection)"),
        );
        let output = settle(&mut app, &ctx);
        actual_preview(&app, &output);
        assert!(
            !app.free_transform.as_ref().unwrap().commands().unwrap()[0]["selection_only"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(workspace::source(&app), before);
        workspace::click(&mut app, &ctx, SIZE, workspace::label(&output, "Cancel"));
        assert!(app.free_transform.is_none());
        assert_eq!(workspace::source(&app), before);
    }
}
