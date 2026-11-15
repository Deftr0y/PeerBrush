use super::*;
use crate::{
    engine::{native::NativeDocument, Engine},
    large_document::{DiskBudget, ResidentBudget},
};
use std::time::{Duration, Instant};

fn fixture(depth: u16) -> (PeerBrush, egui::Context, Document) {
    let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
    app.select_tool(Tool::Selection);
    app.selection_kind = "magnetic".into();
    app.selection_width = 2.;
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    app.selection_gesture_source = Some(app.canvas_gesture_source(&doc).unwrap());
    (app, ctx, doc)
}
fn settle(app: &mut PeerBrush, ctx: &egui::Context, doc: &Document) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll_magnetic_path(ctx, doc.into());
        let Some(input) = &app.magnetic_input else {
            break;
        };
        assert!(input.error.is_none(), "{}", app.message);
        if input.queue.is_empty() && input.ticket.is_none() {
            break;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn delayed_snaps_keep_clicked_anchors_and_close_with_one_shared_selection_commit() {
    for depth in [8, 16] {
        let (mut app, ctx, doc) = fixture(depth);
        let original = super::super::workspace_tests::source(&app);
        let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
        let points = [[-3., 3.5], [28.5, 3.5], [28.5, 20.5], [-3., 20.5]];
        app.queue_magnetic_point((&doc).into(), points[0], true)
            .unwrap();
        app.poll_magnetic_path(&ctx, (&doc).into());
        assert_eq!(app.selection_path, [points[0]]);
        assert!(app.magnetic_input.as_ref().unwrap().ticket.is_none());
        let (release, entered) = app.magnetic_sampler.hold_next();
        app.queue_magnetic_point((&doc).into(), points[1], true)
            .unwrap();
        app.poll_magnetic_path(&ctx, (&doc).into());
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        for point in &points[2..] {
            app.queue_magnetic_point((&doc).into(), *point, true)
                .unwrap();
        }
        app.close_magnetic_path();
        assert!(app.selection_path_pending());
        assert_eq!(app.selection_path, [points[0]]);
        assert_eq!(super::super::workspace_tests::source(&app), original);
        let mut snapped = points;
        for point in &mut snapped[1..3] {
            *point = crate::render::magnetic::resident(&doc, *point, 2., &mut || Ok(())).unwrap();
        }
        let command = app.select_command(&doc, &snapped, "replace");
        let expected = Engine::preview_edits(doc.clone(), &[command]).unwrap();
        release.send(()).unwrap();
        settle(&mut app, &ctx, &doc);
        assert!(!app.selection_path_pending());
        assert!(app.magnetic_input.is_none());
        let mut engine = app.shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 1);
        assert_eq!(engine.resident_state().expect("resident fixture").document.selection, expected.selection);
        assert_eq!(engine.resident_state().expect("resident fixture").document.selection_polygon, expected.selection_polygon);
        assert_eq!(
            serde_json::to_value(&engine.resident_state().expect("resident fixture").document.selection_coverage).unwrap(),
            serde_json::to_value(&expected.selection_coverage).unwrap()
        );
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels, doc.layers[0].pixels);
        engine.undo("human").unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.selection, doc.selection);
    }
}

#[test]
fn hover_coalescing_delete_and_source_changes_preserve_pending_clicks() {
    let (mut app, ctx, doc) = fixture(16);
    for (point, click) in [
        ([3., 3.], true),
        ([8., 8.], false),
        ([12., 8.], false),
        ([16., 8.], true),
        ([20., 8.], false),
        ([24., 8.], false),
    ] {
        app.queue_magnetic_point((&doc).into(), point, click)
            .unwrap();
    }
    assert_eq!(
        app.magnetic_input
            .as_ref()
            .unwrap()
            .queue
            .iter()
            .map(|p| p.position)
            .collect::<Vec<_>>(),
        vec![[3., 3.], [12., 8.], [16., 8.], [24., 8.]]
    );
    app.pop_magnetic_point();
    assert_eq!(
        app.magnetic_input
            .as_ref()
            .unwrap()
            .queue
            .back()
            .unwrap()
            .position,
        [16., 8.]
    );
    app.shared
        .lock()
        .unwrap()
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":app.selected,"name":"Later work"})],
            None,
            None,
            "Later work",
        )
        .unwrap();
    let preserved = super::super::workspace_tests::source(&app);
    let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    app.poll_magnetic_path(&ctx, (&current).into());
    assert!(app.magnetic_input.as_ref().unwrap().error.is_some());
    assert_eq!(app.magnetic_input.as_ref().unwrap().queue.len(), 3);
    app.clear_selection_path();
    assert_eq!(super::super::workspace_tests::source(&app), preserved);
}

#[test]
fn clicking_a_queued_hover_fixes_the_anchor_and_stationary_hover_does_not_resample() {
    let (mut app, ctx, doc) = fixture(16);
    for (point, clicked) in [
        ([3., 3.], true),
        ([12., 8.], false),
        ([12., 8.], true),
        ([20., 8.], false),
    ] {
        app.queue_magnetic_point((&doc).into(), point, clicked)
            .unwrap();
    }
    let input = app.magnetic_input.as_ref().unwrap();
    assert_eq!(input.queue.len(), 3);
    assert!(input.queue[1].clicked);
    settle(&mut app, &ctx, &doc);
    app.queue_magnetic_point((&doc).into(), [20., 8.], false)
        .unwrap();
    assert!(app.magnetic_input.as_ref().unwrap().queue.is_empty());
    app.pop_magnetic_point();
    app.queue_magnetic_point((&doc).into(), [20., 8.], true)
        .unwrap();
    assert!(!app.magnetic_input.as_ref().unwrap().queue.is_empty());
}

#[test]
fn canvas_accepts_native_metadata_and_keeps_rotated_brush_points_in_document_coordinates() {
    let (mut app, ctx, _) = fixture(16);
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let document = NativeDocument::new_blank(
        32,
        24,
        16,
        &pool,
        &DiskBudget::new(4 * 1024 * 1024, 4).unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    app.select_tool(Tool::Brush);
    app.selected = document.metadata().layers().next().unwrap().id().into();
    app.doc_snapshot = document.clone().into();
    app.view.angle = 37.;
    let before = super::super::workspace_tests::source(&app);
    let frame = |app: &mut PeerBrush, events| {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(900., 700.))),
                events,
                focused: true,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui, &document, &[], &[]));
            },
        );
    };
    frame(&mut app, vec![]);
    let rect = app.view_rect.unwrap();
    let scale = rect.width() / 32.;
    let point = [11.25, 8.75];
    let screen = app.view.point(point, rect, scale);
    frame(
        &mut app,
        vec![
            egui::Event::PointerMoved(screen),
            egui::Event::PointerButton {
                pos: screen,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ],
    );
    assert_eq!(app.points.len(), 1);
    assert!((app.points[0][0] - point[0]).abs() < 0.001);
    assert!((app.points[0][1] - point[1]).abs() < 0.001);
    assert_eq!(app.drag_revision, Some(document.metadata().revision()));
    assert_eq!(
        app.straight.document.as_deref(),
        Some(document.metadata().id())
    );
    let captured = app
        .capture_native_canvas(&ctx, &app.project_id.clone(), &document)
        .unwrap();
    assert!(captured.is_some());
    assert_eq!(super::super::workspace_tests::source(&app), before);
    app.select_tool(Tool::Selection);
    app.selection_kind = "magnetic".into();
    app.selection_gesture_source = Some(app.canvas_gesture_source(&document).unwrap());
    app.queue_magnetic_point((&document).into(), [3., 3.], true)
        .unwrap();
    let _ = ctx.run(
        egui::RawInput {
            focused: false,
            ..Default::default()
        },
        |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| app.canvas(ui, &document, &[], &[]));
        },
    );
    assert!(!app.selection_path_pending());
    assert!(app.magnetic_input.is_none());
    assert_eq!(super::super::workspace_tests::source(&app), before);
}

#[test]
fn native_queue_and_source_metadata_release_the_real_budget_on_tool_change() {
    let (mut app, _, _) = fixture(16);
    let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
    let doc = NativeDocument::new_blank(
        32,
        24,
        16,
        &pool,
        &DiskBudget::new(4 * 1024 * 1024, 4).unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    app.selected = doc.metadata().layers().next().unwrap().id().into();
    let baseline = pool.used();
    app.selection_gesture_source = Some(app.canvas_gesture_source(&doc).unwrap());
    app.queue_magnetic_point((&doc).into(), [3., 3.], true)
        .unwrap();
    assert!(pool.used() > baseline);
    assert!(app
        .magnetic_input
        .as_ref()
        .unwrap()
        ._charge
        .as_ref()
        .unwrap()
        .uses_budget(&pool));
    app.select_tool(Tool::Brush);
    assert_eq!(app.selection_path.capacity(), 0);
    assert!(app.magnetic_input.is_none());
    assert_eq!(pool.used(), baseline);
    let full = pool
        .reserve_bytes(pool.limit() - pool.used(), pool.limit())
        .unwrap();
    assert!(app.canvas_gesture_source(&doc).is_err());
    assert!(app
        .queue_magnetic_point((&doc).into(), [3., 3.], true)
        .is_err());
    assert!(app.magnetic_input.is_none());
    assert_eq!(app.selection_path.capacity(), 0);
    drop(full);
    drop(doc);
    assert_eq!(pool.used(), 0);
}

#[test]
fn cancellation_and_refused_publication_preserve_existing_selection() {
    for cancel in [true, false] {
        let (mut app, ctx, doc) = fixture(16);
        let (release, entered) = app.magnetic_sampler.hold_next();
        app.queue_magnetic_point((&doc).into(), [3., 3.], true)
            .unwrap();
        app.poll_magnetic_path(&ctx, (&doc).into());
        entered.recv_timeout(Duration::from_secs(5)).unwrap();
        for point in [[28., 3.], [28., 20.], [3., 20.]] {
            app.queue_magnetic_point((&doc).into(), point, true)
                .unwrap();
        }
        if cancel {
            app.select_tool(Tool::Brush);
        } else {
            app.shared
                .lock()
                .unwrap()
                .reserve(
                    "other",
                    "Existing selection",
                    vec![crate::engine::Scope::layer("@selection")],
                )
                .unwrap();
            app.close_magnetic_path();
        }
        let before = super::super::workspace_tests::source(&app);
        release.send(()).unwrap();
        if !cancel {
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.magnetic_input.as_ref().unwrap().error.is_none() {
                app.poll_magnetic_path(&ctx, (&doc).into());
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(2));
            }
            assert!(app.selection_path.len() >= 3);
            assert!(app.magnetic_input.as_ref().unwrap().commit.is_none());
        }
        app.magnetic_sampler.stop();
        assert_eq!(super::super::workspace_tests::source(&app), before);
    }
}
