use super::workspace_tests as workspace;
use super::*;
use crate::engine::{Engine, Scope};

const SIZE: Vec2 = Vec2::new(1360., 900.);
fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    workspace::draw(app, ctx, SIZE, events)
}
fn pointer(at: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}
fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}
fn settle(app: &mut PeerBrush, ctx: &egui::Context) -> egui::FullOutput {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let output = frame(app, ctx, vec![]);
        if !app.pending && app.canvas_preview.is_some() && app.canvas_preview == app.last_preview {
            return output;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Actual canvas preview did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn wand_control(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>) -> Rect {
    let doc = app.shared.lock().unwrap().doc.clone();
    let mut rect = Rect::NOTHING;
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1200., 160.))),
            time: Some(ctx.input(|i| i.time) + 0.05),
            events,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                rect = app.selection_toolbar(ui, &doc)[0];
            });
        },
    );
    rect
}
fn install(app: &mut PeerBrush, commands: Vec<Value>) -> Document {
    let doc = app.shared.lock().unwrap().doc.clone();
    app.parameter_gesture = Some(ParameterEdit {
        control: egui::Id::new("pending preview fixture"),
        gesture: crate::engine::id(),
        engine: Arc::downgrade(&app.shared),
        document: doc.id.clone(),
        revision: doc.revision,
        commands,
        preview: None,
        preview_runs: 0,
        label: "Pending preview".into(),
        typing: None,
        dirty: true,
    });
    doc
}

#[test]
fn held_wand_reuses_validated_document_in_real_panels_and_preserves_canvas_history_and_reservations(
) {
    for depth in [8, 16] {
        for finish in ["cancel", "apply", "reserved"] {
            let (mut app, ctx) = workspace::fixture(depth);
            app.tool = Tool::MagicWand;
            app.selection_kind = "wand".into();
            {
                let mut engine = app.shared.lock().unwrap();
                for y in 0..24 {
                    for x in 0..32 {
                        engine.doc.layers[0].pixels.set16(
                            x,
                            y,
                            if x < 10 {
                                [10001, 20003, 30007, 65535]
                            } else {
                                [16001, 26003, 36007, 65535]
                            },
                        );
                    }
                }
                let id = app.selected.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"selection","kind":"wand","layer":id,
                    "point":[5,5],"tolerance":0,"feather":2})],
                        None,
                        None,
                        "Wand seed",
                    )
                    .unwrap();
            }
            settle(&mut app, &ctx);
            let before = app.shared.lock().unwrap().doc.clone();
            let unchanged = workspace::source(&app);
            let history = app.shared.lock().unwrap().undo.len();
            let at = wand_control(&mut app, &ctx, vec![]).center();
            wand_control(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at), pointer(at, true)],
            );
            wand_control(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at + Vec2::new(8., 0.))],
            );
            let edit = app
                .parameter_gesture
                .as_ref()
                .expect("Actual held Wand control");
            let runs = edit.preview_runs;
            assert!(runs > 0);
            let previous_commands = edit.commands.clone();
            wand_control(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at + Vec2::new(16., 0.))],
            );
            let edit = app.parameter_gesture.as_ref().unwrap();
            assert_ne!(
                edit.commands, previous_commands,
                "The second real pointer movement must change the pending command"
            );
            assert_eq!(
                edit.preview_runs,
                runs + 1,
                "A changed held command is validated exactly once"
            );
            let runs = edit.preview_runs;
            let commands = edit.commands.clone();
            let expected = Engine::preview_edits(before.clone(), &commands).unwrap();
            let output = settle(&mut app, &ctx);
            let generation = app.canvas_generation;
            for _ in 0..8 {
                frame(&mut app, &ctx, vec![]);
                let _ = ctx.run(Default::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        app.document_filter_stack(ui, &expected);
                    });
                });
            }
            assert_eq!(
                app.parameter_gesture.as_ref().unwrap().preview_runs,
                runs,
                "Unchanged held frames and both panels must not replay shared edits"
            );
            assert_eq!(app.canvas_generation, generation);
            let (w, h, actual) = app.canvas_pixels.as_ref().unwrap();
            let (_, _, mut pixels, _) = expected.preview(None, (*w).max(*h), None, false).unwrap();
            crate::selection::display::apply(&expected, *w, *h, &mut pixels, "cutout");
            assert_eq!(actual, &pixels);
            let texture = app.texture.as_ref().unwrap().id();
            assert!(output.shapes.iter().any(|shape| {
                matches!(&shape.shape, egui::Shape::Mesh(mesh) if mesh.texture_id == texture)
            }));
            assert_eq!(workspace::source(&app), unchanged);
            if finish == "reserved" {
                app.shared
                    .lock()
                    .unwrap()
                    .reserve("other", "Selection owner", vec![Scope::layer("@selection")])
                    .unwrap();
                frame(&mut app, &ctx, vec![]);
                assert_eq!(app.parameter_gesture.as_ref().unwrap().preview_runs, runs);
            }
            if finish == "cancel" {
                frame(
                    &mut app,
                    &ctx,
                    vec![key(egui::Key::Escape, Default::default())],
                );
            }
            frame(&mut app, &ctx, vec![pointer(at, false)]);
            assert!(app.parameter_gesture.is_none());
            if finish != "apply" {
                assert_eq!(workspace::source(&app), unchanged);
            } else {
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.undo.len(), history + 1);
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                let actual = engine.doc.selection_coverage.as_ref().unwrap();
                let expected = expected.selection_coverage.as_ref().unwrap();
                assert_eq!(
                    (&actual.mask, actual.bounds, actual.origin),
                    (&expected.mask, expected.bounds, expected.origin)
                );
                let applied = engine.doc.selection_coverage.clone();
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.selection_coverage, before.selection_coverage);
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                engine.redo("human").unwrap();
                assert_eq!(engine.doc.selection_coverage, applied);
            }
        }
    }
}

#[test]
fn provisional_numeric_text_reuses_preview_and_target_cancel_preserves_source() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let id = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":id,"kind":"invert"})],
                None,
                None,
                "Effect",
            )
            .unwrap();
        frame(&mut app, &ctx, vec![]);
        let before = workspace::source(&app);
        let at = app.opacity_rect.unwrap().center();
        workspace::click(&mut app, &ctx, SIZE, at);
        workspace::click(&mut app, &ctx, SIZE, at);
        frame(&mut app, &ctx, vec![]);
        assert!(app.parameter_gesture.as_ref().unwrap().typing.is_some());
        let runs = app.parameter_gesture.as_ref().unwrap().preview_runs;
        let control = app.parameter_gesture.as_ref().unwrap().control;
        let commands = app.parameter_gesture.as_ref().unwrap().commands.clone();
        let output = frame(&mut app, &ctx, vec![egui::Event::Text("35".into())]);
        assert_eq!(
            ctx.data(|data| data.get_temp::<String>(control.with("number")))
                .as_deref(),
            Some("35")
        );
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "35")), "The numeric editor must actually display its provisional text");
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.parameter_gesture.as_ref().unwrap().preview_runs, runs);
        assert_eq!(app.parameter_gesture.as_ref().unwrap().commands, commands);
        for _ in 0..4 {
            frame(&mut app, &ctx, vec![]);
        }
        assert_eq!(app.parameter_gesture.as_ref().unwrap().preview_runs, runs);
        frame(
            &mut app,
            &ctx,
            vec![
                key(egui::Key::A, egui::Modifiers::COMMAND),
                egui::Event::Text("35".into()),
            ],
        );
        frame(&mut app, &ctx, vec![]);
        assert_eq!(
            app.parameter_gesture.as_ref().unwrap().preview_runs,
            runs,
            "Provisional numeric text does not replay unchanged commands"
        );
        assert_eq!(
            ctx.data(|data| data.get_temp::<String>(control.with("number")))
                .as_deref(),
            Some("35")
        );
        assert_eq!(app.parameter_gesture.as_ref().unwrap().commands, commands);
        assert_eq!(workspace::source(&app), before);
        let effect = app.shared.lock().unwrap().doc.layers[0].effects[0]
            .id
            .clone();
        app.select_effect(&id, false, &effect);
        assert!(app.parameter_gesture.is_none());
        assert!(!controls::number_editing(&ctx, control));
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn current_engine_identity_revision_and_contention_are_checked_before_cache_reuse() {
    for depth in [8, 16] {
        for replacement in [false, true] {
            let (mut app, ctx) = workspace::fixture(depth);
            let id = app.selected.clone();
            let source = install(
                &mut app,
                vec![json!({"op":"layer.update","layer":id,"opacity":0.35})],
            );
            assert!(app.pending_parameter_document(&ctx, &source).is_some());
            let runs = app.parameter_gesture.as_ref().unwrap().preview_runs;
            let shared = app.shared.clone();
            let guard = shared.lock().unwrap();
            assert!(
                app.pending_parameter_document(&ctx, &source).is_none(),
                "A busy engine must not wait or authorize a cached source"
            );
            drop(guard);
            assert!(app.pending_parameter_document(&ctx, &source).is_some());
            assert_eq!(app.parameter_gesture.as_ref().unwrap().preview_runs, runs);
            {
                let mut engine = app.shared.lock().unwrap();
                if replacement {
                    engine.doc.id = "replacement-at-identical-revision".into();
                } else {
                    engine
                        .edit(
                            "agent",
                            &[json!({"op":"layer.update","layer":id,"name":"Remote work"})],
                            None,
                            None,
                            "Remote work",
                        )
                        .unwrap();
                }
            }
            let changed = workspace::source(&app);
            assert!(app.pending_parameter_document(&ctx, &source).is_none());
            assert!(app.parameter_gesture.as_ref().unwrap().preview.is_none());
            app.finish_parameter(false);
            assert!(app.parameter_gesture.is_none());
            assert_eq!(workspace::source(&app), changed);
        }
    }
}

#[test]
fn misses_use_shared_source_not_a_derived_display_and_errors_are_not_replayed() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let id = app.selected.clone();
        let source = install(
            &mut app,
            vec![json!({"op":"layer.update","layer":id,"opacity":0.35})],
        );
        let before = workspace::source(&app);
        let mut display = source.clone();
        display.name = "Already previewed display".into();
        let result = app.pending_parameter_document(&ctx, &display).unwrap();
        assert_eq!(result.name, source.name);
        assert_eq!(result.layers[0].opacity, 0.35);
        assert_eq!(
            result.layers[0].pixels.rgba16(),
            source.layers[0].pixels.rgba16()
        );
        assert_eq!(app.parameter_gesture.as_ref().unwrap().preview_runs, 1);
        let source = install(
            &mut app,
            vec![json!({"op":"layer.update","layer":id,"blend":"invalid fixture blend"})],
        );
        for _ in 0..4 {
            assert!(app.pending_parameter_document(&ctx, &source).is_none());
        }
        let edit = app.parameter_gesture.as_ref().unwrap();
        assert!(edit.preview.as_ref().is_some_and(Result::is_err));
        assert_eq!(edit.preview_runs, 1);
        app.finish_parameter(true);
        assert_eq!(workspace::source(&app), before);
    }
}

#[test]
fn replacing_the_shared_engine_at_identical_document_id_and_revision_cannot_reuse_or_commit() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let id = app.selected.clone();
        let source = install(
            &mut app,
            vec![json!({"op":"layer.update","layer":id,"opacity":0.35})],
        );
        assert!(app.pending_parameter_document(&ctx, &source).is_some());
        let original = app.shared.clone();
        let original_state = workspace::source(&app);
        let mut replacement = Engine::new();
        replacement.doc = source.clone();
        replacement.doc.name = "Independent project at the same revision".into();
        assert_eq!(
            (&replacement.doc.id, replacement.doc.revision),
            (&source.id, source.revision)
        );
        app.shared = Arc::new(std::sync::Mutex::new(replacement));
        let replacement_state = workspace::source(&app);
        assert!(app.pending_parameter_document(&ctx, &source).is_none());
        assert!(app.parameter_gesture.as_ref().unwrap().preview.is_none());
        app.finish_parameter(false);
        assert!(app.parameter_gesture.is_none());
        assert_eq!(workspace::source(&app), replacement_state);
        app.shared = original;
        assert_eq!(workspace::source(&app), original_state);
    }
}
