use super::*;
use crate::engine::{Engine, Scope};

#[test]
fn endpoint_only_decoding_preserves_projection_pressure_and_validation() {
    for (command, expected) in [
        (json!({"points":[[1,2],[4,7]]}), ([4., 7.], 1.)),
        (json!({"points":[[1,2,0.25],[4,7]]}), ([4., 7.], 1.)),
        (
            json!({"points":[[1,2],[4,7,0.75]],"stroke_axis":"axis"}),
            ([1., 7.], 0.75),
        ),
        (
            json!({"points":[[0,0],[4,7],[11,12]],"pressures":[0.1,0.2,0.8],
                "stroke_axis":"horizontal","stroke_axis_from":1}),
            ([11., 7.], 0.8),
        ),
    ] {
        assert_eq!(endpoint(&command), Some(expected));
        let (points, pressures) = crate::brush::points_from_command(&command).unwrap();
        assert_eq!(
            (*points.last().unwrap(), pressures.map_or(1., |p| *p.last().unwrap())),
            expected,
        );
    }
    for command in [
        json!({"points":[[0,0],[100001,1],[4,7]]}),
        json!({"points":[[0,0,-0.5],[4,7]]}),
        json!({"points":[[0,0],[4,7]],"pressures":[2,0.5]}),
        json!({"points":[[0,0],[4,7]],"pressures":[0.5]}),
        json!({"points":[[0,0,0.5],[4,7]],"pressures":[0.5,0.5]}),
        json!({"points":[[0,0],[4,7]],"stroke_axis":"horizontal","stroke_axis_from":2}),
        json!({"points":[[0,0],[4,7]],"stroke_axis_from":0}),
    ] {
        assert_eq!(
            crate::brush::endpoint_from_command(&command).unwrap_err(),
            crate::brush::points_from_command(&command).unwrap_err(),
        );
    }
}

fn stroke(app: &mut PeerBrush) -> Value {
    app.points = vec![[7., 11.], [21., 13.]];
    app.stroke_pressures = vec![0.3, 0.8];
    app.stroke_has_pressure = true;
    app.straight.axis = Some((0, "horizontal"));
    app.stroke_command(&app.points, app.stroke_color())
}
fn queued(app: &mut PeerBrush, ctx: &egui::Context, command: Value, doc: &Document) -> egui::Id {
    let token = app
        .track_stroke(endpoint(&command), &doc.id, doc.revision, None)
        .unwrap();
    app.submit_edit_job(
        edit_job::Request {
            project: app.project_id.clone(),
            document: doc.id.clone(),
            revision: doc.revision,
            label: "Queued brush stroke".into(),
            selection_edit: false,
            origin: edit_job::Origin::Stroke(token),
        },
        edit_job::Commands::resident(vec![command]),
        ctx,
    )
    .unwrap();
    token
}

#[test]
fn stroke_receipt_records_exact_endpoint_and_pressure_after_resident_or_queued_commit() {
    for depth in [8, 16] {
        for mask in [false, true] {
            for worker in [false, true] {
                let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
                app.select_tool(Tool::Brush);
                if mask {
                    let layer = app.selected.clone();
                    app.shared
                        .lock()
                        .unwrap()
                        .edit(
                            "human",
                            &[json!({"op":"mask.add","layer":layer,"value":255})],
                            None,
                            None,
                            "Mask",
                        )
                        .unwrap();
                }
                app.mask = mask;
                app.mask_value = 37;
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                let command = stroke(&mut app);
                let (points, pressures) = crate::brush::points_from_command(&command).unwrap();
                let expected_point = *points.last().unwrap();
                let expected_pressure = *pressures.unwrap().last().unwrap();
                let expected = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
                if worker {
                    let shared = app.shared.clone();
                    let held = shared.lock().unwrap();
                    let token = queued(&mut app, &ctx, command, &before);
                    assert!(app.straight.last.is_none());
                    // Releasing a gesture must keep its pending publication callback.
                    app.straight.cancel();
                    let rejected = app.stroke_command(&[[1., 1.]], [1, 2, 3, 255]);
                    app.submit_stroke(rejected, "Rejected overlapping stroke");
                    assert_eq!(app.straight.pending.as_ref().unwrap().0, token);
                    drop(held);
                    super::super::text_tests::wait_edit(&mut app);
                } else {
                    app.submit_stroke(command, "Brush stroke");
                    app.straight.cancel();
                }
                let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                assert_eq!(
                    serde_json::to_value(&doc.layers).unwrap(),
                    serde_json::to_value(&expected.layers).unwrap()
                );
                assert_eq!(doc.bit_depth, depth);
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history + 1);
                let last = app.straight.last.as_ref().unwrap();
                assert_eq!(last.point, expected_point);
                assert_eq!(last.pressure, expected_pressure);
                assert_eq!(last.revision, doc.revision);
                assert!(app.straight.pending.is_none());
                app.straight_click(&doc, [24., 16.], true);
                assert!(app.straight.line);
                assert_eq!(app.points[0], expected_point);
                assert_eq!(app.stroke_pressures[0], expected_pressure);
            }
        }
    }
}

#[test]
fn failed_or_cancelled_strokes_do_not_create_shift_line_anchors_or_history() {
    for refusal in ["cancel", "reservation", "revision"] {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        app.select_tool(Tool::Brush);
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
        let command = stroke(&mut app);
        let shared = app.shared.clone();
        let mut held = shared.lock().unwrap();
        queued(&mut app, &ctx, command, &before);
        match refusal {
            "cancel" => app.cancel_pending_edit(),
            "reservation" => {
                held.reserve("other", "Human work", vec![Scope::layer(&app.selected)])
                    .unwrap();
            }
            "revision" => held.resident_state_mut().expect("resident fixture").document.revision += 1,
            _ => unreachable!(),
        }
        let preserved = serde_json::to_value(&held.resident_state().expect("resident fixture").document).unwrap();
        drop(held);
        super::super::text_tests::wait_edit(&mut app);
        assert!(app.straight.last.is_none());
        assert!(app.straight.pending.is_none());
        let engine = shared.lock().unwrap();
        assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), preserved);
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history);
    }
}

#[test]
fn late_receipts_and_same_document_ids_in_another_engine_cannot_restore_old_anchors() {
    for change in [
        "tool", "layer", "mask", "step", "project", "engine", "reset",
    ] {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        app.select_tool(Tool::Brush);
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let command = stroke(&mut app);
        let shared = app.shared.clone();
        let held = shared.lock().unwrap();
        queued(&mut app, &ctx, command, &before);
        match change {
            "tool" => app.tool = Tool::Eraser,
            "layer" => app.selected = "other".into(),
            "mask" => app.mask = true,
            "step" => app.mask_step = Some("other".into()),
            "project" => app.project_id = "other".into(),
            "engine" => app.shared = Arc::new(std::sync::Mutex::new(Engine::new())),
            "reset" => app.straight.reset(),
            _ => unreachable!(),
        }
        drop(held);
        super::super::text_tests::wait_edit(&mut app);
        assert!(app.straight.last.is_none());
        assert!(app.straight.pending.is_none());
        assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, before.revision + 1);
    }
    let (mut app, _) = super::super::workspace_tests::fixture(16);
    app.select_tool(Tool::Brush);
    let dot = app.stroke_command(&[[7., 11.]], app.color);
    app.submit_stroke(dot, "Dot");
    assert!(app.straight.last.is_some());
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let mut other = Engine::new();
    other.resident_state_mut().expect("resident fixture").document = doc.clone();
    other.project_id = app.project_id.clone();
    app.shared = Arc::new(std::sync::Mutex::new(other));
    app.straight_click(&doc, [24., 16.], true);
    assert!(!app.straight.line);
}

#[test]
fn terminal_revision_and_token_must_match_the_submitted_stroke() {
    let (mut app, _) = super::super::workspace_tests::fixture(16);
    app.select_tool(Tool::Brush);
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let command = stroke(&mut app);
    let old = app
        .track_stroke(endpoint(&command), &doc.id, doc.revision, None)
        .unwrap();
    let current = app
        .track_stroke(endpoint(&command), &doc.id, doc.revision, None)
        .unwrap();
    app.stroke_finished(old, Some(doc.revision + 1));
    assert_eq!(app.straight.pending.as_ref().unwrap().0, current);
    assert!(app.straight.last.is_none());
    app.stroke_finished(current, Some(doc.revision));
    assert!(app.straight.pending.is_none());
    assert!(app.straight.last.is_none());
}

#[test]
fn endpoint_metadata_charge_lasts_until_the_anchor_is_retired_and_refusal_keeps_no_pending_work() {
    let (mut app, _) = super::super::workspace_tests::fixture(16);
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let command = stroke(&mut app);
    let pool = crate::large_document::ResidentBudget::new(1024 * 1024).unwrap();
    let token = app
        .track_stroke(endpoint(&command), &doc.id, doc.revision, Some(&pool))
        .unwrap();
    let admitted = pool.used();
    assert!(admitted >= 1024);
    app.stroke_finished(token, Some(doc.revision + 1));
    assert!(app.straight.last.is_some());
    assert_eq!(pool.used(), admitted);
    app.straight.reset();
    assert_eq!(pool.used(), 0);
    let full = pool.reserve_bytes(pool.limit(), pool.limit()).unwrap();
    assert!(app
        .track_stroke(endpoint(&command), &doc.id, doc.revision, Some(&pool))
        .is_err());
    assert!(app.straight.pending.is_none());
    assert!(app.straight.last.is_none());
    drop(full);
    assert_eq!(pool.used(), 0);
}

#[test]
fn an_edit_after_publication_before_ui_poll_does_not_extend_an_obsolete_stroke() {
    let (mut app, ctx) = super::super::workspace_tests::fixture(16);
    app.select_tool(Tool::Brush);
    let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let command = stroke(&mut app);
    queued(&mut app, &ctx, command, &before);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        if let Ok(mut engine) = app.shared.try_lock() {
            if engine.resident_state().expect("resident fixture").document.revision == before.revision + 1 {
                engine.edit("human", &[json!({"op":"layer.update","layer":app.selected,"name":"Later human edit"})], None, None, "Later edit").unwrap();
                break;
            }
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    super::super::text_tests::wait_edit(&mut app);
    assert_eq!(
        app.straight.last.as_ref().unwrap().revision,
        before.revision + 1
    );
    app.straight.cancel();
    let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    assert_eq!(current.revision, before.revision + 2);
    app.straight_click(&current, [24., 16.], true);
    assert!(!app.straight.line);
}
