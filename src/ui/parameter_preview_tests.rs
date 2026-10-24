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
fn settle_worker(app: &mut PeerBrush, ctx: &egui::Context) {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let source = app.shared.lock().unwrap().doc.clone();
        app.request_preview(ctx, &source);
        if !app.pending && app.canvas_preview.is_some() && app.canvas_preview == app.last_preview {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "Actual parameter worker did not finish"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}
struct WorkerHold(Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);
impl Drop for WorkerHold {
    fn drop(&mut self) {
        *self.0 .0.lock().unwrap() = true;
        self.0 .1.notify_all();
    }
}
fn hold_worker(app: &mut PeerBrush) -> WorkerHold {
    let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    app.parameter_worker_gate = Some(gate.clone());
    WorkerHold(gate)
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
    app.invalidate_parameter_preview();
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
            let hold = hold_worker(&mut app);
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
            assert_eq!(
                runs, 0,
                "Real held control must not replay on the UI thread"
            );
            let previous_commands = edit.commands.clone();
            app.request_preview(&ctx, &before);
            assert!(app.pending);
            assert!(app.pending_parameter_document(&ctx, &before).is_none());
            let accepted_pixels = app.canvas_pixels.clone();
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
                edit.preview_runs, runs,
                "Changed input must remain responsive while the worker is held"
            );
            let commands = edit.commands.clone();
            frame(&mut app, &ctx, vec![]);
            assert_eq!(
                app.canvas_pixels, accepted_pixels,
                "Pending input retains the actual accepted canvas"
            );
            assert_eq!(workspace::source(&app), unchanged);
            drop(hold);
            let latest = Engine::preview_edits(before.clone(), &commands).unwrap();
            settle(&mut app, &ctx);
            assert_eq!(
                app.parameter_gesture.as_ref().unwrap().preview_runs,
                1,
                "Only the latest held input is accepted"
            );
            assert_eq!(
                app.parameter_worker_runs
                    .load(std::sync::atomic::Ordering::Relaxed),
                1,
                "Superseded queued input is skipped before shared replay"
            );
            let (w, h, actual) = app.canvas_pixels.as_ref().unwrap();
            let (_, _, mut pixels, _) = latest.preview(None, (*w).max(*h), None, false).unwrap();
            crate::selection::display::apply(&latest, *w, *h, &mut pixels, "cutout");
            assert_eq!(actual, &pixels);

            let accepted_pixels = app.canvas_pixels.clone();
            wand_control(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at + Vec2::new(24., 0.))],
            );
            let edit = app.parameter_gesture.as_ref().unwrap();
            assert_ne!(edit.commands, commands);
            assert!(edit.preview.is_none());
            assert_eq!(edit.preview_runs, 1);
            let expected = Engine::preview_edits(before.clone(), &edit.commands).unwrap();
            app.request_preview(&ctx, &before);
            assert_eq!(app.canvas_pixels, accepted_pixels);
            let output = settle(&mut app, &ctx);
            let runs = app.parameter_gesture.as_ref().unwrap().preview_runs;
            assert_eq!(
                runs, 2,
                "An actual changed held command produces exactly one new validated result"
            );
            assert_eq!(
                app.parameter_worker_runs
                    .load(std::sync::atomic::Ordering::Relaxed),
                2
            );
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
        settle(&mut app, &ctx);
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
            assert!(app.pending_parameter_document(&ctx, &source).is_none());
            settle_worker(&mut app, &ctx);
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
        assert!(app.pending_parameter_document(&ctx, &display).is_none());
        settle_worker(&mut app, &ctx);
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
        settle_worker(&mut app, &ctx);
        assert!(app.parameter_gesture.is_none());
        assert!(
            app.message.contains("blend"),
            "Shared engine error must reach the actual caller: {}",
            app.message
        );
        let runs = app
            .parameter_worker_runs
            .load(std::sync::atomic::Ordering::Relaxed);
        for _ in 0..4 {
            app.request_preview(&ctx, &source);
        }
        assert_eq!(
            app.parameter_worker_runs
                .load(std::sync::atomic::Ordering::Relaxed),
            runs
        );
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
        settle_worker(&mut app, &ctx);
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

#[test]
fn view_changes_reuse_only_the_exact_pure_parameter_document() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let id = app.selected.clone();
        let source = install(
            &mut app,
            vec![json!({"op":"layer.update","layer":id,"opacity":0.35})],
        );
        let before = workspace::source(&app);
        settle_worker(&mut app, &ctx);
        let pixels = app.canvas_pixels.clone();
        let runs = app
            .parameter_worker_runs
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(runs, 1);
        app.view_rect = Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(600., 400.)));
        settle_worker(&mut app, &ctx);
        assert_eq!(app.canvas_pixels, pixels);
        assert_eq!(
            app.parameter_worker_runs
                .load(std::sync::atomic::Ordering::Relaxed),
            runs,
            "A view-only render must reuse the worker's validated parameter document"
        );

        install(
            &mut app,
            vec![json!({"op":"layer.update","layer":id,"opacity":0.45})],
        );
        app.transient = vec![json!({"op":"layer.update","layer":id,"name":"Mixed display only"})];
        settle_worker(&mut app, &ctx);
        assert!(
            app.parameter_gesture.as_ref().unwrap().preview.is_none(),
            "Mixed tool/parameter results cannot become a parameter-only panel baseline"
        );
        app.transient.clear();
        settle_worker(&mut app, &ctx);
        let preview = app.pending_parameter_document(&ctx, &source).unwrap();
        assert_eq!(preview.layers[0].name, source.layers[0].name);
        assert_eq!(preview.layers[0].opacity, 0.45);
        assert_eq!(
            preview.layers[0].pixels.rgba16(),
            source.layers[0].pixels.rgba16()
        );
        assert_eq!(workspace::source(&app), before);

        install(
            &mut app,
            vec![json!({"op":"layer.update","layer":id,"opacity":0.55})],
        );
        app.transient =
            vec![json!({"op":"layer.update","layer":id,"blend":"invalid mixed fixture blend"})];
        let pixels = app.canvas_pixels.clone();
        app.request_preview(&ctx, &source);
        let reply = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(reply.error.is_some());
        assert!(
            reply.parameter_document.is_none(),
            "A mixed-command error is not a pure parameter validation error"
        );
        app.preview_tx.send(reply).unwrap();
        app.request_preview(&ctx, &source);
        assert!(app.parameter_gesture.is_some());
        assert!(app.parameter_gesture.as_ref().unwrap().preview.is_none());
        assert_eq!(app.canvas_pixels, pixels);
        app.transient.clear();
        settle_worker(&mut app, &ctx);
        assert_eq!(
            app.pending_parameter_document(&ctx, &source)
                .unwrap()
                .layers[0]
                .opacity,
            0.55
        );
        assert_eq!(workspace::source(&app), before);
        app.finish_parameter(true);
    }
}

#[test]
fn completed_worker_errors_cannot_cancel_newer_input_or_touch_other_projects() {
    for depth in [8, 16] {
        for interruption in ["latest", "cancel", "remote", "replacement"] {
            let (mut app, ctx) = workspace::fixture(depth);
            settle(&mut app, &ctx);
            let id = app.selected.clone();
            let source = install(
                &mut app,
                vec![json!({"op":"layer.update","layer":id,"blend":"invalid fixture blend"})],
            );
            let before = workspace::source(&app);
            let pixels = app.canvas_pixels.clone();
            let history = app.shared.lock().unwrap().undo.len();
            app.request_preview(&ctx, &source);
            let reply = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let old_error = reply
                .error
                .clone()
                .expect("Actual shared replay must reject the invalid blend");
            assert!(app.pending);
            assert_eq!(workspace::source(&app), before);
            match interruption {
                "latest" => {
                    install(
                        &mut app,
                        vec![json!({"op":"layer.update","layer":id,"opacity":0.35})],
                    );
                }
                "cancel" => app.finish_parameter(true),
                "remote" => {
                    app.shared
                        .lock()
                        .unwrap()
                        .edit(
                            "agent",
                            &[json!({"op":"layer.update","layer":id,"name":"Remote work"})],
                            None,
                            None,
                            "Remote work",
                        )
                        .unwrap();
                }
                "replacement" => {
                    let mut replacement = Engine::new();
                    replacement.doc = source.clone();
                    replacement.doc.name = "Independent project at the same revision".into();
                    app.shared = Arc::new(std::sync::Mutex::new(replacement));
                }
                _ => unreachable!(),
            }
            let retained = workspace::source(&app);
            app.message = "Newer input owns the workspace".into();
            app.preview_tx.send(reply).unwrap();
            let current = app.shared.lock().unwrap().doc.clone();
            app.request_preview(&ctx, &current);
            assert_ne!(
                app.message, old_error,
                "A completed obsolete error must not replace current status"
            );
            assert_eq!(
                app.canvas_pixels, pixels,
                "Obsolete errors cannot replace the accepted canvas"
            );
            assert_eq!(workspace::source(&app), retained);
            if interruption == "latest" {
                assert!(
                    app.parameter_gesture.is_some(),
                    "An old error must not cancel newer desired commands"
                );
                settle_worker(&mut app, &ctx);
                let expected = Engine::preview_edits(
                    source.clone(),
                    &app.parameter_gesture.as_ref().unwrap().commands,
                )
                .unwrap();
                assert_eq!(
                    app.canvas_pixels.as_ref().unwrap().2,
                    expected.preview(None, 1536, None, false).unwrap().2
                );
                assert_eq!(workspace::source(&app), before);
                app.finish_parameter(false);
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.undo.len(), history + 1);
                assert_eq!(engine.doc.layers[0].opacity, 0.35);
                engine.undo("human").unwrap();
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    source.export_png().unwrap()
                );
            } else {
                assert!(app.parameter_gesture.is_none());
                settle_worker(&mut app, &ctx);
                assert_eq!(workspace::source(&app), retained);
            }
        }
    }
}

#[test]
fn real_relative_arrow_input_after_release_accumulates_while_the_old_worker_is_held() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let id = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":id,"opacity":0.6})],
                None,
                None,
                "Starting opacity",
            )
            .unwrap();
        settle(&mut app, &ctx);
        let source = app.shared.lock().unwrap().doc.clone();
        let history = app.shared.lock().unwrap().undo.len();
        let at = app.opacity_rect.unwrap().center();
        let hold = hold_worker(&mut app);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(at), pointer(at, true)],
        );
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(at - Vec2::new(20., 0.))],
        );
        let edit = app.parameter_gesture.as_ref().expect("Actual opacity drag");
        let control = edit.control;
        let desired = edit.commands[0]["opacity"].as_f64().unwrap() as f32;
        assert_ne!(desired, source.layers[0].opacity);
        assert!(app.pending && edit.preview.is_none());
        assert_eq!(
            app.parameter_worker_runs
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        frame(
            &mut app,
            &ctx,
            vec![pointer(at - Vec2::new(20., 0.), false)],
        );
        assert!(app.parameter_gesture.is_none());
        assert_eq!(app.shared.lock().unwrap().doc.layers[0].opacity, desired);
        assert_eq!(app.shared.lock().unwrap().undo.len(), history + 1);

        // Slider keyboard edits intentionally retain their existing immediate
        // shared-engine path; they never wait for the obsolete preview to arrive.
        ctx.memory_mut(|memory| memory.request_focus(control));
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::ArrowRight, Default::default())],
        );
        let first = app.shared.lock().unwrap().doc.layers[0].opacity;
        assert!(
            first > desired && first < 1.,
            "First arrow must increment the committed desired value"
        );
        frame(
            &mut app,
            &ctx,
            vec![key(egui::Key::ArrowRight, Default::default())],
        );
        let second = app.shared.lock().unwrap().doc.layers[0].opacity;
        assert!(
            second > first,
            "A second real arrow before worker completion must accumulate"
        );
        assert!(app.parameter_gesture.is_none());
        assert_eq!(
            app.parameter_worker_runs
                .load(std::sync::atomic::Ordering::Relaxed),
            0
        );
        assert_eq!(app.shared.lock().unwrap().undo.len(), history + 3);
        assert_eq!(
            app.shared.lock().unwrap().doc.layers[0].pixels.rgba16(),
            source.layers[0].pixels.rgba16()
        );
        let accepted = workspace::source(&app);
        drop(hold);
        settle(&mut app, &ctx);
        assert_eq!(
            workspace::source(&app),
            accepted,
            "An old worker must not change accepted keyboard edits or history"
        );
        let current = app.shared.lock().unwrap().doc.clone();
        let (w, h, bytes, _) = current.preview(None, 1536, None, false).unwrap();
        assert_eq!(app.canvas_pixels.as_ref(), Some(&(w, h, bytes)));
        let mut engine = app.shared.lock().unwrap();
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].opacity, first);
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].opacity, desired);
        engine.undo("human").unwrap();
        assert_eq!(
            engine.doc.export_png().unwrap(),
            source.export_png().unwrap()
        );
        assert_eq!(
            engine.doc.layers[0].pixels.rgba16(),
            source.layers[0].pixels.rgba16()
        );
    }
}

fn long_stroke(app: &mut PeerBrush, count: usize) {
    app.points = (0..count)
        .map(|index| [3. + (index % 24) as f32, 4. + ((index / 24) % 12) as f32])
        .collect();
    app.stroke_pressures = (0..count)
        .map(|index| 0.2 + (index % 8) as f32 * 0.1)
        .collect();
    app.stroke_has_pressure = true;
}

fn begin_held_stroke(app: &mut PeerBrush, ctx: &egui::Context) -> (WorkerHold, Rect) {
    app.tool = Tool::Brush;
    app.radius = 1.5;
    app.brush.smoothing = 0.;
    app.brush.spacing = 1.;
    app.brush.pressure_size = true;
    settle(app, ctx);
    let rect = app.view_rect.unwrap();
    let at = rect.min + Vec2::new(3., 4.) * (rect.width() / 32.);
    let hold = hold_worker(app);
    app.current_pressure = Some(0.3);
    frame(
        app,
        ctx,
        vec![egui::Event::PointerMoved(at), pointer(at, true)],
    );
    assert!(app.pending && !app.points.is_empty());
    assert!(app.live_gesture.is_some() && app.drag_revision.is_some());
    (hold, rect)
}

#[test]
fn pending_long_stroke_frames_skip_full_variant_and_render_latest_native_input_before_one_commit() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace::fixture(depth);
        let before = app.shared.lock().unwrap().doc.clone();
        let untouched = workspace::source(&app);
        let history = app.shared.lock().unwrap().undo.len();
        let (hold, rect) = begin_held_stroke(&mut app, &ctx);
        let old_job = app.preview_job;
        let accepted = app.canvas_pixels.clone();
        let builds = app.preview_variant_builds.get();
        long_stroke(&mut app, 1000);
        let mut at = Pos2::ZERO;
        for index in 0..8 {
            app.current_pressure = Some(0.35 + index as f32 * 0.07);
            app.radius = 1.5 + index as f32 * 0.1;
            app.brush.flow = 0.6 + index as f32 * 0.03;
            app.color = [43 + index * 7, 97, 211, 255];
            at = rect.min + Vec2::new(20. + index as f32, 18.) * (rect.width() / 32.);
            frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
            assert!(app.pending);
            assert_eq!(app.preview_job, old_job);
            assert_eq!(app.preview_variant_builds.get(), builds);
            assert_eq!(app.canvas_pixels, accepted);
            assert_eq!(workspace::source(&app), untouched);
        }
        assert_eq!(app.points.len(), 1008);
        assert_eq!(app.stroke_pressures.len(), app.points.len());
        assert_eq!(app.stroke_pressures.last(), app.current_pressure.as_ref());
        let command = app.stroke_command(app.points.clone(), app.color);
        assert_eq!(command["points"].as_array().unwrap().len(), 1008);
        assert_eq!(command["pressures"].as_array().unwrap().len(), 1008);
        let expected = Engine::preview_edits(before.clone(), &[command]).unwrap();
        assert_ne!(expected.layers[0].pixels, before.layers[0].pixels);
        app.parameter_worker_gate = None;
        drop(hold);
        let reply = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(reply.job, old_job);
        assert!(!reply.coarse && reply.error.is_none());
        let completed_pixels = (reply.w, reply.h, reply.bytes.clone());
        app.preview_tx.send(reply).unwrap();
        frame(&mut app, &ctx, vec![]);
        assert_eq!(app.preview_variant_builds.get(), builds + 1);
        assert_eq!(app.canvas_pixels.as_ref().unwrap(), &completed_pixels);
        assert!(app.pending && app.preview_job != old_job);
        assert_eq!(workspace::source(&app), untouched);
        settle(&mut app, &ctx);
        let (w, h, pixels) = app.canvas_pixels.as_ref().unwrap();
        assert_eq!(
            pixels,
            &expected.preview(None, (*w).max(*h), None, false).unwrap().2
        );
        assert_eq!(workspace::source(&app), untouched);
        frame(&mut app, &ctx, vec![pointer(at, false)]);
        assert!(app.points.is_empty());
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.doc.bit_depth, depth);
        assert_eq!(engine.doc.layers[0].pixels, expected.layers[0].pixels);
        assert_eq!(engine.doc.layers[0].source, before.layers[0].source);
        assert_eq!(
            engine.doc.layers[0].pixels.get16(1, 1),
            before.layers[0].pixels.get16(1, 1)
        );
        assert_eq!(engine.undo.len(), history + 1);
        assert!(engine.redo.is_empty());
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
        engine.redo("human").unwrap();
        assert_eq!(engine.doc.layers[0].pixels, expected.layers[0].pixels);
    }
}

#[test]
fn pending_long_stroke_still_cancels_and_rejects_obsolete_results_after_escape_or_shared_edit() {
    for depth in [8, 16] {
        for interruption in ["escape", "shared"] {
            let (mut app, ctx) = workspace::fixture(depth);
            let (hold, _) = begin_held_stroke(&mut app, &ctx);
            long_stroke(&mut app, 1000);
            let accepted = app.canvas_pixels.clone();
            let before = workspace::source(&app);
            let builds = app.preview_variant_builds.get();
            if interruption == "escape" {
                workspace::key(&mut app, &ctx, SIZE, egui::Key::Escape, Default::default());
                assert_eq!(workspace::source(&app), before);
            } else {
                app.shared.lock().unwrap().edit(
                    "human", &[json!({"op":"layer.update","layer":app.selected,"name":"Newer human work"})],
                    None, None, "Human rename",
                ).unwrap();
                frame(&mut app, &ctx, vec![]);
                assert!(app.message.contains("Image changed during brush stroke"));
            }
            assert!(app.points.is_empty() && app.drag_start.is_none());
            assert!(app.live_gesture.is_none());
            assert!(app.pending);
            assert_eq!(app.preview_variant_builds.get(), builds);
            assert_eq!(app.canvas_pixels, accepted);
            let retained = workspace::source(&app);
            app.parameter_worker_gate = None;
            drop(hold);
            let reply = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            app.preview_tx.send(reply).unwrap();
            frame(&mut app, &ctx, vec![]);
            assert_eq!(app.canvas_pixels, accepted);
            assert_eq!(app.preview_variant_builds.get(), builds + 1);
            assert_eq!(workspace::source(&app), retained);
            frame(
                &mut app,
                &ctx,
                vec![pointer(
                    ctx.input(|i| i.pointer.interact_pos()).unwrap(),
                    false,
                )],
            );
            settle(&mut app, &ctx);
            assert_eq!(workspace::source(&app), retained);
            let engine = app.shared.lock().unwrap();
            let (w, h, pixels) = app.canvas_pixels.as_ref().unwrap();
            assert_eq!(
                pixels,
                &engine
                    .doc
                    .preview(None, (*w).max(*h), None, false)
                    .unwrap()
                    .2
            );
            assert_eq!(engine.doc.bit_depth, depth);
        }
    }
}

#[test]
#[ignore = "Descriptive pending-frame construction diagnostic; run separately from regular QA"]
fn pending_stroke_variant_construction_diagnostic() {
    for depth in [8, 16] {
        for samples in [100, 1000, 9000] {
            let (mut app, ctx) = workspace::fixture(depth);
            let (hold, _) = begin_held_stroke(&mut app, &ctx);
            long_stroke(&mut app, samples);
            let before = workspace::source(&app);
            let builds = app.preview_variant_builds.get();
            let started = std::time::Instant::now();
            for _ in 0..12 {
                frame(&mut app, &ctx, vec![]);
            }
            let elapsed = started.elapsed();
            assert!(app.pending);
            assert_eq!(app.preview_variant_builds.get(), builds);
            assert_eq!(workspace::source(&app), before);
            println!(
                "{}",
                json!({"kind":"pending_stroke_variant", "depth":depth,
                "seeded_samples":samples, "retained_samples":app.points.len(), "frames":12,
                "full_variant_builds":app.preview_variant_builds.get() - builds,
                "elapsed_ns":elapsed.as_nanos()})
            );
            app.parameter_worker_gate = None;
            drop(hold);
            let _ = app.preview_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
    }
}

fn alt_sample_frame(
    app: &mut PeerBrush,
    ctx: &egui::Context,
    events: Vec<egui::Event>,
    alt: bool,
    focused: bool,
) {
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)),
            time: Some(ctx.input(|i| i.time) + 0.05),
            modifiers: egui::Modifiers {
                alt,
                ..Default::default()
            },
            focused,
            events,
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    );
}
fn alt_sample_button(at: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers {
            alt: true,
            ..Default::default()
        },
    }
}
fn alt_sample_point(app: &PeerBrush, point: [f32; 2]) -> Pos2 {
    let rect = app.view_rect.expect("Actual canvas");
    rect.min + Vec2::new(point[0], point[1]) * (rect.width() / 32.)
}
fn alt_sample_expected(app: &PeerBrush, point: [i32; 2]) -> Pixel {
    let engine = app.shared.lock().unwrap();
    if app.mask {
        let layer = engine
            .doc
            .layers
            .iter()
            .find(|l| l.id == app.selected)
            .unwrap();
        let value =
            (layer.mask_value_raw(point[0] - layer.x, point[1] - layer.y) * 255.).round() as u8;
        [value; 4]
    } else {
        engine
            .doc
            .preview(
                Some([point[0], point[1], point[0] + 1, point[1] + 1]),
                1,
                None,
                false,
            )
            .unwrap()
            .2
            .try_into()
            .unwrap()
    }
}
fn alt_sample_assert(app: &PeerBrush, point: [i32; 2]) {
    let expected = alt_sample_expected(app, point);
    if app.mask {
        assert_eq!(app.mask_value, expected[0]);
    } else {
        assert_eq!(app.color, expected);
    }
}
fn alt_sample_fixture(depth: u16) -> (PeerBrush, egui::Context) {
    let (mut app, ctx) = workspace::fixture(depth);
    app.tool = Tool::Brush;
    app.radius = 1.5;
    app.brush.smoothing = 0.;
    app.brush.spacing = 1.;
    {
        let mut engine = app.shared.lock().unwrap();
        let layer = app.selected.clone();
        engine.edit("human", &[
            json!({"op":"mask.add","layer":layer}),
            json!({"op":"paint","layer":layer,"mask":true,"points":[[6,8]],"radius":4,"color":[73,73,73,255]}),
            json!({"op":"mask.step.add","layer":layer,"kind":"gaussian","settings":{"radius":1}}),
            json!({"op":"mask.toggle","layer":layer,"enabled":false}),
            json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":1}}),
            json!({"op":"filter.add","kind":"blur","settings":{"radius":1}}),
            json!({"op":"layer.add","name":"Other native layer"}),
        ], None, None, "Sampling native effects and raw spatial mask").unwrap();
        for opacity in [0.9, 0.8] {
            engine
                .edit(
                    "human",
                    &[json!({"op":"layer.update","layer":layer,"opacity":opacity})],
                    None,
                    None,
                    "Human settings",
                )
                .unwrap();
        }
        engine.undo("human").unwrap();
        assert!(!engine.undo.is_empty() && !engine.redo.is_empty());
    }
    settle(&mut app, &ctx);
    (app, ctx)
}

#[test]
fn alt_held_identical_native_pixels_reapply_exact_samples_without_resampling_and_paint_immediately()
{
    for depth in [8, 16] {
        for mask in [false, true] {
            let (mut app, ctx) = alt_sample_fixture(depth);
            app.mask = mask;
            let before = workspace::source(&app);
            let reservations = json!(app.shared.lock().unwrap().leases);
            let at = alt_sample_point(&app, [1.25, 1.25]);
            let runs = app.color_pick_runs;
            alt_sample_frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at), alt_sample_button(at, true)],
                true,
                true,
            );
            assert_eq!(app.color_pick_runs, runs + 1);
            assert_eq!(app.alt_sample.as_ref().unwrap().point, [1, 1]);
            alt_sample_assert(&app, [1, 1]);
            let subpixel = alt_sample_point(&app, [1.45, 1.4]);
            for _ in 0..8 {
                app.color = [255, 0, 255, 23];
                app.mask_value = 19;
                alt_sample_frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::PointerMoved(subpixel)],
                    true,
                    true,
                );
                assert_eq!(app.color_pick_runs, runs + 1);
                alt_sample_assert(&app, [1, 1]);
                assert_eq!(workspace::source(&app), before);
            }
            let moved = alt_sample_point(&app, [8.25, 8.25]);
            alt_sample_frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(moved)],
                true,
                true,
            );
            assert_eq!(app.color_pick_runs, runs + 2);
            alt_sample_assert(&app, [8, 8]);
            assert!(
                app.color[3] > 0,
                "The immediate paint must have nonzero alpha"
            );
            if mask {
                let engine = app.shared.lock().unwrap();
                let layer = engine
                    .doc
                    .layers
                    .iter()
                    .find(|l| l.id == app.selected)
                    .unwrap();
                assert_ne!(
                    app.mask_value,
                    (layer.mask_value_raw(24, 16) * 255.).round() as u8
                );
            }
            assert_eq!(workspace::source(&app), before);
            alt_sample_frame(
                &mut app,
                &ctx,
                vec![alt_sample_button(moved, false)],
                true,
                true,
            );
            assert!(app.alt_sample.is_none());
            assert_eq!(json!(app.shared.lock().unwrap().leases), reservations);
            let destination = alt_sample_point(&app, [24.25, 16.25]);
            let baseline = app.shared.lock().unwrap().doc.clone();
            let history = app.shared.lock().unwrap().undo.len();
            let layer = app.selected.clone();
            let target_pixel = |doc: &Document| {
                let layer = doc.layers.iter().find(|l| l.id == layer).unwrap();
                if mask {
                    layer
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .iter()
                        .rev()
                        .find(|step| step.kind == "paint")
                        .unwrap()
                        .pixels
                        .get16(24, 16)
                } else {
                    layer.pixels.get16(24, 16)
                }
            };
            let native_before = target_pixel(&baseline);
            // The very next ordinary pointer press starts paint synchronously with the sampled value.
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(destination),
                    pointer(destination, true),
                ],
            );
            assert!(!app.points.is_empty());
            let color = if mask {
                [app.mask_value, app.mask_value, app.mask_value, app.color[3]]
            } else {
                app.color
            };
            let command = app.stroke_command(app.points.clone(), color);
            let expected = Engine::preview_edits(baseline.clone(), &[command]).unwrap();
            assert_ne!(
                target_pixel(&expected),
                native_before,
                "The real shared preview must paint native data"
            );
            frame(&mut app, &ctx, vec![pointer(destination, false)]);
            let mut engine = app.shared.lock().unwrap();
            assert_eq!(engine.undo.len(), history + 1);
            assert_eq!(engine.doc.bit_depth, depth);
            assert_ne!(
                target_pixel(&engine.doc),
                native_before,
                "Immediate paint must change native data"
            );
            assert_eq!(target_pixel(&engine.doc), target_pixel(&expected));
            assert_eq!(
                engine
                    .doc
                    .layers
                    .iter()
                    .map(|l| json!([l.pixels, l.mask]))
                    .collect::<Vec<_>>(),
                expected
                    .layers
                    .iter()
                    .map(|l| json!([l.pixels, l.mask]))
                    .collect::<Vec<_>>()
            );
            engine.undo("human").unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                baseline.export_png().unwrap()
            );
            engine.redo("human").unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                expected.export_png().unwrap()
            );
        }
    }
}

#[test]
fn alt_sample_source_revision_selected_mask_and_step_changes_cannot_reuse_old_pixel() {
    for depth in [8, 16] {
        let (mut app, ctx) = alt_sample_fixture(depth);
        let point = [6, 8];
        let at = alt_sample_point(&app, [6.25, 8.25]);
        alt_sample_frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(at), alt_sample_button(at, true)],
            true,
            true,
        );
        let mut runs = app.color_pick_runs;
        let layer = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"opacity":0.4})],
                None,
                None,
                "Newer human source",
            )
            .unwrap();
        for operation in ["changed", "undo", "redo", "mask", "step", "selected"] {
            match operation {
                "undo" => app.shared.lock().unwrap().undo("human").unwrap(),
                "redo" => app.shared.lock().unwrap().redo("human").unwrap(),
                "mask" => app.mask = true,
                "step" => {
                    app.mask_step = Some(
                        app.shared
                            .lock()
                            .unwrap()
                            .doc
                            .layers
                            .iter()
                            .find(|l| l.id == layer)
                            .unwrap()
                            .mask
                            .as_ref()
                            .unwrap()
                            .steps[0]
                            .id
                            .clone(),
                    );
                }
                "selected" => {
                    app.selected = app
                        .shared
                        .lock()
                        .unwrap()
                        .doc
                        .layers
                        .iter()
                        .find(|l| l.id != layer)
                        .unwrap()
                        .id
                        .clone();
                }
                _ => {}
            }
            let before = workspace::source(&app);
            alt_sample_frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at)],
                true,
                true,
            );
            runs += 1;
            assert_eq!(app.color_pick_runs, runs, "{depth}/{operation}");
            alt_sample_assert(&app, point);
            assert_eq!(workspace::source(&app), before);
            alt_sample_frame(&mut app, &ctx, vec![], true, true);
            assert_eq!(app.color_pick_runs, runs);
        }
        alt_sample_frame(
            &mut app,
            &ctx,
            vec![alt_sample_button(at, false)],
            true,
            true,
        );
        assert!(app.alt_sample.is_none());
    }
}

#[test]
fn alt_sample_runtime_restore_engine_project_focus_and_plain_picker_boundaries_stay_owned() {
    for depth in [8, 16] {
        for boundary in ["restore", "engine", "project", "focus", "escape", "tool"] {
            let (mut app, ctx) = alt_sample_fixture(depth);
            let at = alt_sample_point(&app, [6.25, 8.25]);
            alt_sample_frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at), alt_sample_button(at, true)],
                true,
                true,
            );
            let runs = app.color_pick_runs;
            let previous = app.shared.clone();
            let original = previous.lock().unwrap().doc.clone();
            let original_id = original.id.clone();
            let mut events = vec![];
            let mut focused = true;
            match boundary {
                "restore" => {
                    app.shared
                        .lock()
                        .unwrap()
                        .replace(original.clone(), None)
                        .unwrap();
                    assert_ne!(app.shared.lock().unwrap().doc.id, original_id);
                }
                "engine" => {
                    // Same serialized identity in a new Engine is still a different owner.
                    let mut replacement = Engine::new();
                    replacement.project_id = previous.lock().unwrap().project_id.clone();
                    replacement.doc = original.clone();
                    app.shared = Arc::new(std::sync::Mutex::new(replacement));
                }
                "project" => {
                    let mut replacement = Engine::new();
                    replacement.doc = original.clone();
                    let added =
                        crate::workspace::register_in(&app.workspace, replacement, None).unwrap();
                    let id = added.lock().unwrap().project_id.clone();
                    crate::workspace::select_in(&app.workspace, &id).unwrap();
                    app.switch_project(&id);
                    assert_ne!(app.shared.lock().unwrap().doc.id, original_id);
                }
                "focus" => {
                    focused = false;
                    events.push(egui::Event::WindowFocused(false));
                }
                "escape" => events.push(key(
                    egui::Key::Escape,
                    egui::Modifiers {
                        alt: true,
                        ..Default::default()
                    },
                )),
                "tool" => {
                    app.select_tool(Tool::Picker);
                    app.select_tool(Tool::Brush);
                }
                _ => unreachable!(),
            }
            let before = workspace::source(&app);
            alt_sample_frame(&mut app, &ctx, events, true, focused);
            assert_eq!(workspace::source(&app), before, "{depth}/{boundary}");
            // At any fresh sampling opportunity the old entry cannot suppress exact work.
            assert!(
                app.color_pick_runs > runs || app.alt_sample.is_none(),
                "{depth}/{boundary}"
            );
            if !focused {
                alt_sample_frame(
                    &mut app,
                    &ctx,
                    vec![egui::Event::WindowFocused(true)],
                    true,
                    true,
                );
            }
            alt_sample_frame(
                &mut app,
                &ctx,
                vec![alt_sample_button(at, false)],
                false,
                true,
            );
            assert!(app.alt_sample.is_none());
            app.select_tool(Tool::Picker);
            frame(&mut app, &ctx, vec![]);
            let at = alt_sample_point(&app, [6.25, 8.25]);
            let before = workspace::source(&app);
            let plain_runs = app.color_pick_runs;
            workspace::click(&mut app, &ctx, SIZE, at);
            workspace::click(&mut app, &ctx, SIZE, at);
            assert_eq!(app.color_pick_runs, plain_runs + 2);
            assert!(app.alt_sample.is_none());
            alt_sample_assert(&app, [6, 8]);
            assert_eq!(workspace::source(&app), before);
            if matches!(boundary, "engine" | "project") {
                assert_eq!(
                    serde_json::to_value(&previous.lock().unwrap().doc).unwrap(),
                    serde_json::to_value(original).unwrap()
                );
            }
        }
    }
}
#[test]
fn entering_alt_commits_existing_native_stroke_before_sampling_and_respects_later_reservation() {
    for depth in [8, 16] {
        let (mut app, ctx) = alt_sample_fixture(depth);
        app.color = [211, 43, 97, 255];
        let before = app.shared.lock().unwrap().doc.clone();
        let history = app.shared.lock().unwrap().undo.len();
        let start = alt_sample_point(&app, [14.25, 12.25]);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(start), pointer(start, true)],
        );
        let at = alt_sample_point(&app, [16.25, 12.25]);
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(at)]);
        assert!(!app.points.is_empty());
        let expected = Engine::preview_edits(
            before.clone(),
            &[app.stroke_command(app.points.clone(), app.color)],
        )
        .unwrap();
        alt_sample_frame(&mut app, &ctx, vec![], true, true);
        assert!(app.points.is_empty());
        alt_sample_assert(&app, [16, 12]);
        {
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.undo.len(), history + 1);
            assert_eq!(engine.doc.bit_depth, depth);
            assert_eq!(
                engine
                    .doc
                    .layers
                    .iter()
                    .map(|l| json!(l.pixels))
                    .collect::<Vec<_>>(),
                expected
                    .layers
                    .iter()
                    .map(|l| json!(l.pixels))
                    .collect::<Vec<_>>()
            );
            assert_eq!(
                app.alt_sample.as_ref().unwrap().revision,
                engine.doc.revision
            );
        }
        let layer = app.selected.clone();
        app.shared
            .lock()
            .unwrap()
            .reserve(
                "other",
                "Keep sampled native work",
                vec![Scope::layer(&layer)],
            )
            .unwrap();
        let retained = workspace::source(&app);
        let reservations = json!(app.shared.lock().unwrap().leases);
        let runs = app.color_pick_runs;
        alt_sample_frame(&mut app, &ctx, vec![], true, true);
        assert_eq!(app.color_pick_runs, runs);
        assert_eq!(workspace::source(&app), retained);
        assert_eq!(json!(app.shared.lock().unwrap().leases), reservations);
        alt_sample_frame(
            &mut app,
            &ctx,
            vec![alt_sample_button(at, false)],
            true,
            true,
        );
        let destination = alt_sample_point(&app, [24.25, 16.25]);
        workspace::click(&mut app, &ctx, SIZE, destination);
        assert_eq!(workspace::source(&app), retained);
        assert_eq!(json!(app.shared.lock().unwrap().leases), reservations);
        assert_eq!(app.shared.lock().unwrap().doc.bit_depth, depth);
    }
}
