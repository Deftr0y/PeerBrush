use super::*;
use crate::engine::Engine;
use std::time::{Duration, Instant};

pub(in crate::ui) fn settle(app: &mut PeerBrush, ctx: &egui::Context) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.sampled_input.is_some() {
        app.poll_edit_job();
        app.doc_snapshot = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone().into();
        app.poll_sampled_input(ctx);
        assert!(
            app.sampled_input
                .as_ref()
                .and_then(|input| input.error.as_ref())
                .is_none(),
            "{}",
            app.message
        );
        assert!(Instant::now() < deadline, "Sampled input did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn fixture(depth: u16) -> (PeerBrush, egui::Context, Document) {
    let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
    app.select_tool(Tool::Brush);
    let doc = {
        let mut engine = app.shared.lock().unwrap();
        engine.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(1, 1, [12347, 23459, 34571, 65535]);
        engine.resident_state().expect("resident fixture").document.clone()
    };
    app.doc_snapshot = doc.clone().into();
    let _ = ctx.run(
        egui::RawInput {
            focused: true,
            ..Default::default()
        },
        |_| {},
    );
    (app, ctx, doc)
}
fn paint(app: &mut PeerBrush, doc: &Document, point: [f32; 2]) -> Value {
    app.points = vec![point, [point[0] + 1., point[1] + 1.]];
    app.stroke_pressures = vec![0.4, 0.9];
    app.stroke_has_pressure = true;
    app.drag_revision = Some(doc.revision);
    app.straight.document = Some(doc.id.clone());
    let command = app.stroke_command(&app.points, app.stroke_color());
    assert!(app.submit_stroke(command.clone(), "Sampled stroke"));
    app.points.clear();
    app.stroke_pressures.clear();
    app.stroke_has_pressure = false;
    app.drag_revision = None;
    app.straight.cancel();
    command
}

fn apply_sample(command: &mut Value, doc: &Document, point: [i32; 2]) {
    let sample = crate::render::sample::resident_color(doc, point, None, &|| Ok(())).unwrap();
    command["color"] = json!(sample.display());
    if let Some(words) = sample.words() {
        command["color16"] = json!(words);
    }
}

#[test]
fn queued_alt_sample_stroke_sample_stroke_retains_pressure_and_commits_in_order() {
    for depth in [8, 16] {
        let (mut app, ctx, doc) = fixture(depth);
        let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
        let (release, entered) = app.alt_sampler.hold_next();
        app.queue_alt_sample(&ctx, &doc, [1.25, 1.25]);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(app.sampled_stroke_waiting());
        let mut first = paint(&mut app, &doc, [15., 12.]);
        app.queue_alt_sample(&ctx, &doc, [15.25, 12.25]);
        let mut second = paint(&mut app, &doc, [24., 16.]);
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
        apply_sample(&mut first, &doc, [1, 1]);
        if depth == 16 {
            assert_eq!(first["color16"], json!([12347, 23459, 34571, 65535]));
        }
        let intermediate = Engine::preview_edits(doc.clone(), &[first]).unwrap();
        apply_sample(&mut second, &intermediate, [15, 12]);
        second["source_revision"] = json!(intermediate.revision);
        let expected = Engine::preview_edits(intermediate, &[second]).unwrap();
        release.send(()).unwrap();
        settle(&mut app, &ctx);
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 2);
        assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
        assert_eq!(
            serde_json::to_value(&engine.resident_state().expect("resident fixture").document.layers).unwrap(),
            serde_json::to_value(&expected.layers).unwrap()
        );
        engine.undo("human").unwrap();
        engine.undo("human").unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.export_png().unwrap(), doc.export_png().unwrap());
    }
}

#[test]
fn manual_color_override_and_escape_do_not_publish_late_sample_colors() {
    for action in ["manual", "cancel"] {
        let (mut app, ctx, doc) = fixture(16);
        let before = super::super::workspace_tests::source(&app);
        let (release, entered) = app.alt_sampler.hold_next();
        app.queue_alt_sample(&ctx, &doc, [1., 1.]);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        app.color = [17, 53, 129, 211];
        let desired = app.color;
        let command = paint(&mut app, &doc, [20., 12.]);
        if action == "cancel" {
            app.cancel_sampled_input();
        }
        release.send(()).unwrap();
        settle(&mut app, &ctx);
        app.alt_sampler.stop();
        assert_eq!(app.color, desired);
        if action == "cancel" {
            assert_eq!(super::super::workspace_tests::source(&app), before);
        } else {
            let expected = Engine::preview_edits(doc, &[command]).unwrap();
            assert_eq!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                expected.export_png().unwrap()
            );
        }
    }
}

fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>, alt: bool) {
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 1040.))),
            time: Some(ctx.input(|i| i.time) + 0.05),
            events,
            modifiers: egui::Modifiers {
                alt,
                ..Default::default()
            },
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    );
}
fn button(point: Pos2, pressed: bool, alt: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos: point,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers {
            alt,
            ..Default::default()
        },
    }
}
#[test]
fn canvas_preserves_the_first_press_and_movement_before_or_on_sample_completion() {
    for depth in [8, 16] {
        for ready_before_press in [false, true] {
            let (mut app, ctx, doc) = fixture(depth);
            frame(&mut app, &ctx, vec![], false);
            let rect = app.view_rect.unwrap();
            let at = app
                .view
                .point([1.25, 1.25], rect, rect.width() / doc.width as f32);
            let destination = app
                .view
                .point([20.25, 12.25], rect, rect.width() / doc.width as f32);
            let moved = app
                .view
                .point([24.25, 15.25], rect, rect.width() / doc.width as f32);
            let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
            let (release, entered) = app.alt_sampler.hold_next();
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(at), button(at, true, true)],
                true,
            );
            entered.recv_timeout(Duration::from_secs(5)).unwrap();
            frame(&mut app, &ctx, vec![button(at, false, true)], true);
            if ready_before_press {
                release.send(()).unwrap();
                app.alt_sampler.wait_ready();
            }
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(destination),
                    button(destination, true, false),
                ],
                false,
            );
            assert!(!app.points.is_empty(), "First paint press was lost");
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::PointerMoved(moved)],
                false,
            );
            assert!(app.points.len() >= 2);
            let mut command = app.stroke_command(&app.points, app.stroke_color());
            apply_sample(&mut command, &doc, [1, 1]);
            let expected = Engine::preview_edits(doc.clone(), &[command]).unwrap();
            frame(&mut app, &ctx, vec![button(moved, false, false)], false);
            if !ready_before_press {
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
                release.send(()).unwrap();
            }
            settle(&mut app, &ctx);
            assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history + 1);
            assert_eq!(
                app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
                expected.export_png().unwrap()
            );
        }
    }
}

#[test]
fn shift_line_uses_the_prior_queued_endpoint_without_publishing_early() {
    let (mut app, ctx, doc) = fixture(16);
    let (release, entered) = app.alt_sampler.hold_next();
    app.queue_alt_sample(&ctx, &doc, [1., 1.]);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut first = paint(&mut app, &doc, [15., 12.]);
    app.straight_click(&doc, [24., 16.], true);
    assert!(app.straight.line);
    assert_eq!(app.points[0], [16., 13.]);
    assert_eq!(app.stroke_pressures[0], 0.9);
    let mut second = app.stroke_command(&app.points, app.stroke_color());
    assert!(app.submit_stroke(second.clone(), "Queued Shift line"));
    app.points.clear();
    app.straight.cancel();
    app.drag_revision = None;
    apply_sample(&mut first, &doc, [1, 1]);
    apply_sample(&mut second, &doc, [1, 1]);
    let intermediate = Engine::preview_edits(doc.clone(), &[first]).unwrap();
    second["source_revision"] = json!(intermediate.revision);
    let expected = Engine::preview_edits(intermediate, &[second]).unwrap();
    release.send(()).unwrap();
    settle(&mut app, &ctx);
    assert_eq!(
        app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
        expected.export_png().unwrap()
    );
}

#[test]
fn resolved_queued_strokes_share_the_real_uncommitted_canvas_preview() {
    let (mut app, ctx, doc) = fixture(16);
    let (release, entered) = app.alt_sampler.hold_next();
    app.queue_alt_sample(&ctx, &doc, [1., 1.]);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    let mut first = paint(&mut app, &doc, [15., 12.]);
    let mut second = paint(&mut app, &doc, [24., 16.]);
    let source = super::super::workspace_tests::source(&app);
    app.refresh_sampled_preview((&doc).into());
    assert!(app.sampled_preview_commands().is_empty());
    release.send(()).unwrap();
    app.alt_sampler.wait_ready();
    app.process_sampled_input(&ctx);
    app.refresh_sampled_preview((&doc).into());
    assert_eq!(app.sampled_preview_commands().len(), 2);
    apply_sample(&mut first, &doc, [1, 1]);
    apply_sample(&mut second, &doc, [1, 1]);
    let expected = Engine::preview_edits(doc.clone(), &[first, second]).unwrap();
    let commands = app
        .sampled_preview_commands()
        .iter()
        .map(|command| (**command).clone())
        .collect::<Vec<_>>();
    let preview = Engine::preview_edits(doc.clone(), &commands).unwrap();
    assert_eq!(
        preview.export_png().unwrap(),
        expected.export_png().unwrap()
    );
    assert_eq!(super::super::workspace_tests::source(&app), source);
    let key = app.sampled_preview_key();
    app.refresh_sampled_preview((&doc).into());
    assert_eq!(app.sampled_preview_key(), key);
    settle(&mut app, &ctx);
    assert_eq!(
        app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
        expected.export_png().unwrap()
    );
}

#[test]
fn publication_receipts_wait_for_the_frame_that_contains_the_committed_pixels() {
    let (mut app, ctx, doc) = fixture(16);
    let (release, entered) = app.alt_sampler.hold_next();
    app.queue_alt_sample(&ctx, &doc, [1., 1.]);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    paint(&mut app, &doc, [15., 12.]);
    paint(&mut app, &doc, [24., 16.]);
    release.send(()).unwrap();
    app.alt_sampler.wait_ready();
    app.process_sampled_input(&ctx);
    app.dispatch_sampled_input(&ctx);
    let deadline = Instant::now() + Duration::from_secs(5);
    let current = loop {
        if let Ok(engine) = app.shared.try_lock() {
            if engine.resident_state().expect("resident fixture").document.revision == doc.revision + 1 {
                break engine.resident_state().expect("resident fixture").document.clone();
            }
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    app.poll_edit_job();
    assert!(app.sampled_publication_pending((&current).into()));
    app.process_sampled_input(&ctx);
    assert!(app.sampled_input.as_ref().unwrap().receipt.is_some());
    app.refresh_sampled_preview((&doc).into());
    assert_eq!(app.sampled_preview_commands().len(), 2);
    app.doc_snapshot = current.clone().into();
    app.process_sampled_input(&ctx);
    assert!(!app.sampled_publication_pending((&current).into()));
    app.refresh_sampled_preview((&current).into());
    assert_eq!(app.sampled_preview_commands().len(), 1);
    let commands = app
        .sampled_preview_commands()
        .iter()
        .map(|command| (**command).clone())
        .collect::<Vec<_>>();
    let expected = Engine::preview_edits(current, &commands).unwrap();
    settle(&mut app, &ctx);
    assert_eq!(
        app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
        expected.export_png().unwrap()
    );
}

#[test]
fn conflicts_keep_uncommitted_input_and_focus_loss_keeps_completed_gestures() {
    for failure in ["reservation", "source"] {
        let (mut app, ctx, doc) = fixture(16);
        let (release, entered) = app.alt_sampler.hold_next();
        app.queue_alt_sample(&ctx, &doc, [1., 1.]);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        paint(&mut app, &doc, [20., 12.]);
        {
            let mut engine = app.shared.lock().unwrap();
            if failure == "reservation" {
                engine
                    .reserve(
                        "other",
                        "Existing work",
                        vec![crate::engine::Scope::layer(&app.selected)],
                    )
                    .unwrap();
            } else {
                engine.edit("human",&[json!({"op":"layer.update","layer":app.selected,"name":"Later human edit"})],None,None,"Later edit").unwrap();
            }
        }
        let preserved = super::super::workspace_tests::source(&app);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.sampled_input.as_ref().unwrap().error.is_none() {
            app.poll_edit_job();
            app.doc_snapshot = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone().into();
            app.poll_sampled_input(&ctx);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(app.sampled_input.as_ref().unwrap().timeline.has_strokes());
        assert_eq!(super::super::workspace_tests::source(&app), preserved);
        app.cancel_sampled_input();
        app.alt_sampler.stop();
        assert_eq!(super::super::workspace_tests::source(&app), preserved);
    }
    for completed in [false, true] {
        let (mut app, ctx, doc) = fixture(16);
        let original = app.color;
        let (release, entered) = app.alt_sampler.hold_next();
        app.queue_alt_sample(&ctx, &doc, [1., 1.]);
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        let expected = if completed {
            let mut command = paint(&mut app, &doc, [20., 12.]);
            apply_sample(&mut command, &doc, [1, 1]);
            Engine::preview_edits(doc.clone(), &[command]).unwrap()
        } else {
            doc.clone()
        };
        let _ = ctx.run(
            egui::RawInput {
                focused: false,
                ..Default::default()
            },
            |_| {},
        );
        app.sampled_focus_lost();
        release.send(()).unwrap();
        settle(&mut app, &ctx);
        app.alt_sampler.stop();
        assert_eq!(app.color, original);
        assert_eq!(
            app.shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(),
            expected.export_png().unwrap()
        );
    }
}

#[test]
fn render_capture_rejects_queued_previews_after_an_intervening_edit() {
    let (mut app, ctx, doc) = fixture(16);
    let (release, entered) = app.alt_sampler.hold_next();
    app.queue_alt_sample(&ctx, &doc, [1., 1.]);
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    paint(&mut app, &doc, [20., 12.]);
    release.send(()).unwrap();
    app.alt_sampler.wait_ready();
    app.process_sampled_input(&ctx);
    app.refresh_sampled_preview((&doc).into());
    assert_eq!(app.sampled_preview_commands().len(), 1);
    app.shared
        .lock()
        .unwrap()
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":app.selected,"name":"Later human edit"})],
            None,
            None,
            "Later edit",
        )
        .unwrap();
    let preserved = super::super::workspace_tests::source(&app);
    // Keep the ordinary renderer idle; capture must still retire stale commands.
    app.pending = true;
    app.request_preview(&ctx, &doc);
    assert!(app.sampled_preview_commands().is_empty());
    assert!(app.sampled_input.as_ref().unwrap().timeline.has_strokes());
    assert_eq!(super::super::workspace_tests::source(&app), preserved);
    app.cancel_sampled_input();
}
