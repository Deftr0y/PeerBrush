use super::*;
use crate::engine::{authority::DocumentAuthority, native::worker};
use crate::ui::Tool;
use serde_json::json;
use std::time::{Duration, Instant};

fn request_input(document: &NativeDocument, commands: &[&Value], mode: Option<&str>) -> Input {
    Input::new(
        document,
        "canvas-test",
        document
            .copy_commands_metadata(commands, &|| Ok(()))
            .unwrap(),
        48,
        None,
        false,
        mode,
    )
    .unwrap()
}

#[test]
fn live_stroke_keeps_a_compatible_native_job_and_captures_the_latest_samples() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let pool = runtime.resident();
        let document = NativeDocument::new_blank(2048, 64, depth, &pool, &runtime.disk(), &|| Ok(())).unwrap();
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
        app.set_preview_quality(false).unwrap();
        app.doc_snapshot = document.clone().into();
        app.selected = document.metadata().layers().next().unwrap().id().into();
        app.tool = Tool::Brush;
        app.live_gesture = Some("original stroke".into());
        app.points = vec![[16., 16.], [32., 16.]];
        let first = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        let epoch = app.preview_epoch.load(Ordering::Relaxed);
        assert_eq!(first.stamp.edge, 8192);
        assert!(matches!(first.stamp.gesture, Some(LiveGesture::Stroke(_))));
        app.native_dispatch.expected = Some((epoch, first.stamp.clone(), true));

        app.points.push([85., 22.]);
        let available = pool.limit() - pool.used();
        let held = pool.reserve_bytes(available, pool.limit()).unwrap();
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
        assert!(app.capture_native_canvas(&ctx, "other project", &document).is_err());
        drop(held);

        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(2048., 64.));
        app.view_rect = Some(canvas);
        app.canvas_viewport = Some(egui::Rect::from_min_size(egui::pos2(140., 0.), egui::vec2(180., 64.)));
        let moved = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert!(moved.stamp.rect.is_some());
        app.canvas_viewport = Some(canvas);
        app.live_gesture = Some("other stroke".into());
        let other = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_ne!(first.stamp.gesture, other.stamp.gesture);
        app.live_gesture = Some("original stroke".into());
        app.native_dispatch.expected = Some((epoch, first.stamp.clone(), false));
        let latest = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(first.stamp.gesture, latest.stamp.gesture);
        assert_ne!(first.stamp.signature, latest.stamp.signature);
        assert_eq!(latest.commands.as_slice(), &[app.preview_stroke().unwrap()]);
        drop((app, latest, other, moved, first, document, runtime, connection));
    }
}

#[test]
fn native_pointer_parameters_preserve_compatible_work_and_skip_an_obsolete_failure() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let document = NativeDocument::new_blank(
            2048, 64, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
        app.set_preview_quality(false).unwrap();
        app.shared.lock().unwrap().authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        app.doc_snapshot = document.clone().into();
        let layer = document.metadata().layer(0).unwrap().id().to_owned();
        let frame = |app: &mut crate::ui::PeerBrush, opacity, events| {
            let _ = ctx.run(egui::RawInput { events, ..Default::default() }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let (_, mut response) = ui.allocate_exact_size(egui::Vec2::splat(100.), egui::Sense::click_and_drag());
                    if response.is_pointer_button_down_on() || response.dragged() {
                        response.mark_changed();
                        app.layer_parameter(json!({"layer":layer,"opacity":opacity}), "Layer opacity", &response);
                    }
                });
            });
        };
        let press = egui::pos2(25., 25.);
        frame(&mut app, 1., vec![]);
        frame(&mut app, 0.75, vec![egui::Event::PointerMoved(press), egui::Event::PointerButton {
            pos: press, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default(),
        }]);
        let first = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert!(matches!(first.stamp.gesture, Some(LiveGesture::Parameter(_))));
        let epoch = app.preview_epoch.load(Ordering::Relaxed);
        app.native_dispatch.expected = Some((epoch, first.stamp.clone(), true));

        frame(&mut app, 0.35, vec![egui::Event::PointerMoved(egui::pos2(55., 25.))]);
        assert_eq!(app.preview_epoch.load(Ordering::Relaxed), epoch);
        let pool = document.budget();
        let free = pool.limit() - pool.used();
        let held = pool.reserve_bytes(free, free).unwrap();
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
        assert!(app.native_dispatch.gesture_deferred);
        drop(held);

        app.native_dispatch.replies.complete(Reply {
            job: epoch, stamp: first.stamp.clone(), result: Err("Obsolete preview failed".into()),
        });
        assert!(!app.present_native_canvas("canvas-test", &document, &ctx).unwrap());
        assert!(!app.native_dispatch.pending());
        assert!(app.native_dispatch.skip_cache.is_none());
        let latest = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(latest.stamp.gesture, first.stamp.gesture);
        assert_ne!(latest.stamp.signature, first.stamp.signature);
        assert_eq!(latest.commands[0]["opacity"], json!(0.35));

        app.native_dispatch.expected = Some((epoch, latest.stamp.clone(), true));
        app.parameter_gesture.as_mut().unwrap().commands[0]["layer"] = json!("other layer");
        let retargeted = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_ne!(retargeted.stamp.gesture, latest.stamp.gesture);
        assert!(!app.native_dispatch.gesture_deferred);
        app.native_dispatch.expected = Some((epoch, retargeted.stamp.clone(), true));
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
        assert!(!app.native_dispatch.gesture_deferred);
        app.native_dispatch.replies.complete(Reply {
            job: epoch, stamp: retargeted.stamp, result: Err("Current source unavailable".into()),
        });
        assert_eq!(app.present_native_canvas("canvas-test", &document, &ctx).unwrap_err(),
            "Current source unavailable");
        app.finish_parameter(true);
        assert_ne!(app.preview_epoch.load(Ordering::Relaxed), epoch);
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
    }
}

#[test]
fn native_gizmo_waits_for_a_compatible_render_and_supersedes_an_ended_drag() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let document = NativeDocument::new_blank(
        64, 48, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()),
    ).unwrap();
    let (mut app, ctx) = crate::ui::workspace_tests::fixture(16);
    app.set_preview_quality(false).unwrap();
    app.doc_snapshot = document.clone().into();
    let metadata = document.metadata();
    let layer = metadata.layer(0).unwrap().id().to_owned();
    app.tool = Tool::Move;
    app.gizmo_handle = 3;
    app.gizmo_source = Some(crate::ui::TransformGestureSource {
        document: metadata.id().into(), revision: metadata.revision(), native: Some(document.key()),
        roots: vec![layer.clone()], tool: Tool::Move, mask: false, step: None,
    });
    app.live_gesture = Some("move gesture".into());
    app.transient.push(json!({"op":"move","layer":layer,"dx":7,"dy":0}));
    let first = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
    assert!(matches!(first.stamp.gesture, Some(LiveGesture::Transform(_))));
    let epoch = app.preview_epoch.load(Ordering::Relaxed);
    app.native_dispatch.expected = Some((epoch, first.stamp.clone(), true));

    app.transient[0]["dx"] = json!(15);
    assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
    assert!(app.native_dispatch.gesture_deferred);
    app.native_dispatch.expected = Some((epoch, first.stamp.clone(), false));
    let latest = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
    assert_eq!(latest.stamp.gesture, first.stamp.gesture);
    assert_eq!(latest.commands[0]["dx"], json!(15));

    app.native_dispatch.expected = Some((epoch, latest.stamp, true));
    app.gizmo_source = None;
    app.gizmo_handle = 0;
    app.live_gesture = None;
    app.transient.clear();
    let settled = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
    assert!(settled.stamp.gesture.is_none());
    assert!(!app.native_dispatch.gesture_deferred);
}

#[test]
fn pending_reduced_native_stroke_yields_to_the_settled_full_detail_grid() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let document = NativeDocument::new_blank(2048, 64, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
    let (mut app, ctx) = crate::ui::workspace_tests::fixture(16);
    app.set_preview_quality(true).unwrap();
    app.doc_snapshot = document.clone().into();
    app.selected = document.metadata().layers().next().unwrap().id().into();
    app.tool = Tool::Brush;
    app.live_gesture = Some("reduced stroke".into());
    app.points = vec![[16., 16.], [32., 16.]];
    let capture = |app: &mut crate::ui::PeerBrush, time| {
        ctx.begin_pass(egui::RawInput { time: Some(time), ..Default::default() });
        let result = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap();
        let _ = ctx.end_pass();
        result
    };
    let first = capture(&mut app, 1.).unwrap();
    assert_eq!(first.stamp.edge, 1536);
    let epoch = app.preview_epoch.load(Ordering::Relaxed);
    app.native_dispatch.expected = Some((epoch, first.stamp, true));
    app.points[1] = [62., 18.];
    assert!(capture(&mut app, 1.12).is_none());
    assert!(capture(&mut app, 1.20).is_none());
    let full = capture(&mut app, 1.30).unwrap();
    assert_eq!(full.stamp.edge, 8192);
    assert_eq!(full.commands.as_slice(), &[app.preview_stroke().unwrap()]);
}

#[test]
fn rotated_native_view_keeps_full_detail_when_display_pixels_cover_the_samples() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let document = NativeDocument::new_blank(
        4096, 4096, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()),
    ).unwrap();
    let (mut app, ctx) = crate::ui::workspace_tests::fixture(16);
    app.set_preview_quality(true).unwrap();
    app.view.angle = 45.;
    app.view.mirror_h = true;
    let layer = document.metadata().layer(0).unwrap().id().to_owned();
    app.transient = vec![json!({"op":"move", "layer":layer, "dx":4})];
    let pixels_per_point = ctx.pixels_per_point();
    let side = 850. / pixels_per_point;
    app.canvas_viewport = Some(egui::Rect::from_center_size(
        egui::Pos2::ZERO, egui::Vec2::splat(side),
    ));
    let capture = |app: &mut crate::ui::PeerBrush, size: f32| {
        app.view_rect = Some(egui::Rect::from_center_size(egui::Pos2::ZERO, egui::Vec2::splat(size)));
        app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap()
    };
    let full = capture(&mut app, 4096. / pixels_per_point);
    let rect = full.stamp.rect.unwrap();
    assert!(rect[2] - rect[0] > 850);
    assert_eq!(full.stamp.edge, 8192);
    let reduced = capture(&mut app, 2048. / pixels_per_point);
    assert!((1202..=1207).contains(&reduced.stamp.edge));
}

#[test]
fn canvas_capture_at_every_size_waits_for_the_current_visible_region() {
    for depth in [8, 16] {
      for [width, height] in [[1024, 768], [8192, 8192], [16000, 9000]] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let document = NativeDocument::new_blank(
            width, height, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
        app.project_id = "canvas-test".into();
        app.preview_epoch.store(1, Ordering::Relaxed);
        app.preview_job = 1;
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width as f32, height as f32));
        let viewport = egui::Rect::from_min_size(egui::pos2(400., 300.), egui::vec2(32., 24.));
        app.view_rect = Some(canvas);
        app.canvas_viewport = Some(canvas);
        assert_eq!(app.native_canvas_region(&document), None);
        app.canvas_viewport = Some(viewport);
        assert_eq!(app.native_canvas_region(&document), Some([398, 298, 434, 326]));
        let input = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(input.stamp.rect, Some([394, 295, 438, 329]));
        app.native_dispatch.expected = Some((1, input.stamp.clone(), true));
        assert!(app.native_dispatch.matches_base_capture(
            &document, "canvas-test", input.stamp.edge, input.stamp.rect, None, false, 1,
        ));
        let parameter = json!({"op":"layer.update","layer":document.metadata().layer(0).unwrap().id(),"opacity":0.5});
        let edited = request_input(&document, &[&parameter], None);
        app.native_dispatch.expected = Some((1, edited.stamp.clone(), true));
        assert!(!app.native_dispatch.matches_base_capture(
            &document, "canvas-test", edited.stamp.edge, edited.stamp.rect, None, false, 1,
        ));
        drop(edited);
        let scene = Arc::new(authority.capture_scene(
            None, "canvas-test".into(), vec![], None, None,
        ).unwrap().into_native().unwrap());
        let read = runtime.begin_read(&scene).unwrap();
        let frame = Request::capture(scene, read, input.stamp.rect, input.stamp.edge, None, false)
            .unwrap().render(&mut || Ok(())).unwrap();
        assert_eq!((frame.key().width, frame.key().height), (44, 34));
        let output = upload(frame, None, &app.native_dispatch.retirement, 64, &|| Ok(())).unwrap();
        app.native_dispatch.expected = Some((1, input.stamp.clone(), true));
        app.native_dispatch.replies.complete(Reply {
            job: 1, stamp: input.stamp, result: Ok(output),
        });
        assert!(app.present_native_canvas("canvas-test", &document, &ctx).unwrap());
        assert!(app.canvas_capture_ready((&document).into()));
        app.view_rect = Some(canvas.translate(egui::vec2(2., 0.)));
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
        assert!(app.canvas_capture_ready((&document).into()));
        app.view_rect = Some(canvas.translate(egui::vec2(10., 0.)));
        assert!(!app.canvas_capture_ready((&document).into()));
        let outside = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(outside.stamp.rect, Some([384, 295, 428, 329]));
        app.view_rect = Some(canvas);
        app.canvas_viewport = Some(viewport.expand(10.));
        assert!(!app.canvas_capture_ready((&document).into()));
        app.canvas_viewport = Some(viewport);
        assert!(app.canvas_capture_ready((&document).into()));
        app.view.mirror_h = true;
        assert!(!app.canvas_capture_ready((&document).into()));
        app.view.mirror_h = false;
        app.view.angle = 90.;
        assert!(!app.canvas_capture_ready((&document).into()));
        app.view.angle = 0.;
        app.view_rect = Some(canvas.translate(egui::vec2(10., 0.)));
        app.preview_epoch.store(2, Ordering::Relaxed);
        app.native_dispatch.expected = Some((2, outside.stamp.clone(), true));
        let painted = app.native_canvas.current().unwrap().tag().clone();
        let busy = if depth == 8 {
            crate::workspace::source_runtime::INVENTORY_CHECK_BUSY.into()
        } else {
            let _held = app.shared.lock().unwrap();
            crate::workspace::native_dispatch::try_source_engine(&app.shared)
                .err()
                .unwrap()
        };
        app.native_dispatch.replies.complete(Reply {
            job: 2,
            stamp: outside.stamp.clone(),
            result: Err(busy),
        });
        assert!(!app.present_native_canvas("canvas-test", &document, &ctx).unwrap());
        assert!(!app.native_dispatch.pending());
        assert!(app.native_dispatch.retry_after.is_some());
        assert!(app.native_dispatch.skip_cache.is_none());
        assert!(painted.same_allocation(app.native_canvas.current().unwrap().tag()));
        assert!(!app.canvas_capture_ready((&document).into()));

        app.native_dispatch.retry_after = Some(Instant::now() + Duration::from_secs(5));
        assert!(app.native_dispatch.unchanged(&outside, 2));
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
        app.view_rect = Some(canvas.translate(egui::vec2(40., 0.)));
        let moved = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert!(!app.native_dispatch.unchanged(&moved, 2));
        app.view_rect = Some(canvas.translate(egui::vec2(10., 0.)));
        app.native_dispatch.retry_after = Some(Instant::now() - Duration::from_secs(1));
        let retry = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert!(retry.stamp.same_request(&outside.stamp));
        assert!(!app.native_dispatch.unchanged(&retry, 2));

        app.preview_epoch.store(3, Ordering::Relaxed);
        app.native_dispatch.expected = Some((3, retry.stamp.clone(), true));
        app.native_dispatch.replies.complete(Reply {
            job: 3, stamp: retry.stamp, result: Err("preview admission refused".into()),
        });
        assert!(app.present_native_canvas("canvas-test", &document, &ctx).is_err());
        assert!(app.native_dispatch.retry_after.is_none());
        let retry = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(retry.stamp.rect, Some([388, 298, 424, 326]));
        assert_eq!(authority.history_lengths(), [0, 0]);
        drop(connection);
      }
    }
}

#[test]
fn native_canvas_reuses_and_recaptures_the_view_under_metadata_pressure() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let document = NativeDocument::new_blank(
            1024, 768, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
        app.doc_snapshot = crate::engine::view::DocumentSnapshot::Native(document.clone());
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024., 768.));
        app.view_rect = Some(canvas);
        app.canvas_viewport = Some(egui::Rect::from_min_size(egui::pos2(400., 300.), egui::vec2(32., 24.)));
        let first = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        let original_region = first.stamp.rect;
        let epoch = app.preview_epoch.load(Ordering::Relaxed);
        app.native_dispatch.expected = Some((epoch, first.stamp, false));

        let budget = document.budget();
        let headroom = budget.reserve_bytes(8192, 8192).unwrap();
        let remaining = budget.limit() - budget.used();
        let pressure = budget.reserve_bytes(remaining, remaining).unwrap();
        app.view_rect = Some(canvas.translate(egui::vec2(2., 0.)));
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
        assert_eq!(budget.used(), budget.limit());

        drop(headroom);
        app.view_rect = Some(canvas.translate(egui::vec2(100., 0.)));
        let moved = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_ne!(moved.stamp.rect, original_region);
        assert_eq!(moved.stamp.edge, 8192);
        drop(moved);

        app.blend_hover = Some(crate::ui::BlendHover { layers: vec![app.selected.clone()], setting: crate::ui::BlendSetting::Mode("multiply") });
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).is_err());
        drop(pressure);
        let hover = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(hover.commands[0]["blend"], "multiply");
        assert!(matches!(hover.stamp.gesture, Some(LiveGesture::BlendHover(_))));
        app.native_dispatch.expected = Some((epoch, hover.stamp, true));
        app.blend_hover.as_mut().unwrap().setting = crate::ui::BlendSetting::Channels([true, false, true]);
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().is_none());
        assert!(app.native_dispatch.gesture_deferred);
        app.native_dispatch.expected.as_mut().unwrap().2 = false;
        let latest = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(latest.commands[0]["blend_channels"], json!([true, false, true]));
        app.native_dispatch.expected = Some((epoch, latest.stamp, true));
        app.blend_hover = None;
        let restored = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert!(restored.commands.is_empty());
        assert!(!app.native_dispatch.gesture_deferred);
        assert_eq!(document.metadata().revision(), 0);
        drop((app, document, runtime, connection));
    }
}

#[test]
fn panning_native_artwork_offscreen_cancels_pending_render_and_resumes_full_detail() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let document = NativeDocument::new_blank(
            16000, 16000, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        let (mut app, ctx) = crate::ui::tests::fixture();
        app.doc_snapshot = crate::engine::view::DocumentSnapshot::Native(document.clone());
        let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(16000.));
        let viewport = egui::Rect::from_min_size(egui::pos2(400., 300.), egui::vec2(32., 24.));
        app.view_rect = Some(canvas);
        app.canvas_viewport = Some(viewport);
        let request = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        let epoch = app.preview_epoch.load(Ordering::Relaxed);
        app.native_dispatch.expected = Some((epoch, request.stamp.clone(), true));
        app.pending = true;
        let abandoned = app.native_dispatch.replies.clone();

        app.view_rect = Some(canvas.translate(egui::vec2(500., 0.)));
        app.request_native_canvas(&ctx, "canvas-test", &document);
        assert!(app.native_dispatch.expected.is_none());
        assert!(!app.pending);
        assert_eq!(app.preview_epoch.load(Ordering::Relaxed), epoch + 1);
        abandoned.complete(Reply {
            job: epoch, stamp: request.stamp, result: Err("Cancelled".into()),
        });
        assert!(app.native_dispatch.replies.take().is_none());
        assert!(app.canvas_capture_ready((&document).into()));
        let other = NativeDocument::new_blank(
            16000, 16000, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        assert!(!app.canvas_capture_ready((&other).into()));
        app.request_native_canvas(&ctx, "canvas-test", &document);
        assert_eq!(app.preview_epoch.load(Ordering::Relaxed), epoch + 1);

        app.view_rect = Some(canvas);
        assert!(!app.canvas_capture_ready((&document).into()));
        let request = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
        assert_eq!(request.stamp.edge, 16000);
        assert_eq!(request.stamp.rect, Some([394, 295, 438, 329]));
        drop((app, document, other, runtime, connection));
    }
}

#[test]
fn full_quality_request_rejects_a_smaller_sampling_grid() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let document = NativeDocument::new_blank(
            128,
            97,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        let scene = Arc::new(
            authority
                .capture_scene(None, "canvas-test".into(), vec![], None, None)
                .unwrap()
                .into_native()
                .unwrap(),
        );
        let read = runtime.begin_read(&scene).unwrap();
        let request = Request::capture(scene.clone(), read.clone(), None, 48, None, false).unwrap();
        let output = upload(
            request.render(&mut || Ok(())).unwrap(),
            None,
            &Mutex::new(None),
            64,
            &|| Ok(()),
        )
        .unwrap();
        assert!(output
            .validate(&request_input(&document, &[], None).stamp)
            .is_ok());
        let full = Input::new(
            &document,
            "canvas-test",
            document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            8192,
            None,
            false,
            None,
        )
        .unwrap();
        assert!(output.validate(&full.stamp).is_err());
        let rect = [80, 60, 112, 92];
        let cropped = request_input(&document, &[], None).with_region(&document, Some(rect)).unwrap();
        assert!(!cropped.stamp.same_view(&full.stamp));
        let request = Request::capture(scene, read, Some(rect), 48, None, false).unwrap();
        let frame = request.render(&mut || Ok(())).unwrap();
        assert_eq!((frame.key().width, frame.key().height), (32, 32));
        let baseline = Baseline::capture(&frame, &document, "canvas-test", &|| Ok(())).unwrap();
        assert_eq!(baseline.tag().key().rect, rect);
        let output = upload(frame, None, &Mutex::new(None), 64, &|| Ok(())).unwrap();
        output.validate(&cropped.stamp).unwrap();
        let shifted = request_input(&document, &[], None)
            .with_region(&document, Some([81, 60, 113, 92])).unwrap();
        assert_ne!(cropped.stamp.signature, shifted.stamp.signature);
        assert!(!cropped.stamp.same_view(&shifted.stamp));
        assert!(output.validate(&shifted.stamp).is_err());
        drop(connection);
    }
}

#[test]
fn capture_receipts_require_the_exact_published_tag_and_current_settled_request() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let document = NativeDocument::new_blank(
            4,
            2,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        let (mut app, ctx) = crate::ui::workspace_tests::fixture(depth);
        app.project_id = "canvas-test".into();
        app.preview_epoch.store(1, Ordering::Relaxed);
        app.preview_job = 1;
        let input = Input::new(
            &document,
            "canvas-test",
            document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            8192,
            None,
            false,
            None,
        )
        .unwrap();
        let make_output = |edge| {
            let scene = Arc::new(
                authority
                    .capture_scene(None, "canvas-test".into(), vec![], None, None)
                    .unwrap()
                    .into_native()
                    .unwrap(),
            );
            let weak_scene = Arc::downgrade(&scene);
            let read = runtime.begin_read(&scene).unwrap();
            let request = Request::capture(scene, read, None, edge, None, false).unwrap();
            let frame = request.render(&mut || Ok(())).unwrap();
            drop(request);
            assert!(weak_scene.upgrade().is_none());
            upload(frame, None, &app.native_dispatch.retirement, 64, &|| Ok(())).unwrap()
        };
        let output = make_output(8192);
        let other = make_output(8192);
        assert_eq!(output.uploads.tag().key(), other.uploads.tag().key());
        assert!(!output.uploads.tag().same_allocation(other.uploads.tag()));
        app.native_dispatch.expected = Some((1, input.stamp.clone(), true));
        app.native_dispatch.replies.complete(Reply {
            job: 1,
            stamp: input.stamp.clone(),
            result: Ok(output),
        });
        assert!(!app.canvas_capture_ready((&document).into()));
        let painted = app.canvas_generation;
        assert!(app
            .present_native_canvas("canvas-test", &document, &ctx)
            .unwrap());
        assert_ne!(
            app.canvas_generation, painted,
            "upload after paint must cross the shared generation fence"
        );
        assert!(app.canvas_capture_ready((&document).into()));
        let receipt = app.native_receipt.as_ref().unwrap();
        assert!(!app.native_dispatch.capture_ready(
            receipt,
            "canvas-test",
            &document,
            1,
            other.uploads.tag()
        ));
        assert!(!app.native_dispatch.capture_ready(
            receipt,
            "another-project",
            &document,
            1,
            app.native_canvas.current().unwrap().tag()
        ));
        let changed = NativeDocument::new_blank(
            4,
            2,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        assert!(!app.canvas_capture_ready((&changed).into()));
        app.preview_epoch.store(2, Ordering::Relaxed);
        assert!(!app.canvas_capture_ready((&document).into()));
        app.preview_epoch.store(1, Ordering::Relaxed);
        app.native_dispatch.expected.as_mut().unwrap().2 = true;
        assert!(!app.canvas_capture_ready((&document).into()));
        app.native_dispatch.expected.as_mut().unwrap().2 = false;
        assert!(app.canvas_capture_ready((&document).into()));
        let layer = document.metadata().layers().next().unwrap().id();
        let target = Input::new(
            &document,
            "canvas-test",
            document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            8192,
            Some(layer),
            false,
            None,
        )
        .unwrap();
        app.native_dispatch.expected = Some((1, target.stamp, false));
        assert!(!app.canvas_capture_ready((&document).into()));
        // A failed newer request preserves the canvas while withholding capture.
        app.preview_epoch.store(2, Ordering::Relaxed);
        app.preview_job = 2;
        app.native_dispatch.expected = Some((2, input.stamp.clone(), true));
        app.native_dispatch.replies.complete(Reply {
            job: 2,
            stamp: input.stamp.clone(),
            result: Err("injected render refusal".into()),
        });
        assert!(app
            .present_native_canvas("canvas-test", &document, &ctx)
            .is_err());
        assert!(!app.canvas_capture_ready((&document).into()));
        let coarse = Input::new(
            &document,
            "canvas-test",
            document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            48,
            None,
            false,
            None,
        )
        .unwrap();
        app.native_dispatch.expected = Some((2, coarse.stamp.clone(), true));
        app.native_dispatch.replies.complete(Reply {
            job: 2,
            stamp: coarse.stamp,
            result: Ok(other),
        });
        assert!(app
            .present_native_canvas("canvas-test", &document, &ctx)
            .unwrap());
        assert!(
            !app.canvas_capture_ready((&document).into()),
            "coarse requests do not qualify even on a small canvas"
        );
        let command =
            json!({"op":"paint.fill","layer":layer,"rect":[0,0,4,2],"color":[31,73,149,199]});
        let transient = Input::new(
            &document,
            "canvas-test",
            document
                .copy_commands_metadata(&[&command], &|| Ok(()))
                .unwrap(),
            8192,
            None,
            false,
            None,
        )
        .unwrap();
        let session = Arc::new(runtime.begin().unwrap());
        let prepared = worker::prepare(
            worker::tests::capture(&authority, &[command]),
            &session,
            &|| Ok(()),
        )
        .unwrap();
        let scene = Arc::new(
            authority
                .capture_scene(None, "canvas-test".into(), vec![], None, None)
                .unwrap()
                .into_native()
                .unwrap(),
        );
        let disk_before_render = runtime.disk().used().unwrap();
        let rendered = native_preview::render_prepared_with(
            scene, prepared, session.clone(), None, 8192, None, false, None,
            &|| Ok(()), true, |request, check| {
                let owner = upload_retirement(request.budget(), &app.native_dispatch.retirement)?;
                native_texture::Prepared::from_request(request, &owner, 64, None, None, &mut || check())
            },
        ).unwrap();
        let (origin, uploads, supplement) = rendered.into_parts();
        drop(supplement);
        assert!(runtime.disk().used().unwrap() < disk_before_render);
        let output = Output { uploads, origin: Some(origin), baseline: None, text: None };
        app.preview_epoch.store(3, Ordering::Relaxed);
        app.preview_job = 3;
        app.native_dispatch.expected = Some((3, transient.stamp.clone(), true));
        app.native_dispatch.replies.complete(Reply {
            job: 3,
            stamp: transient.stamp,
            result: Ok(output),
        });
        assert!(app
            .present_native_canvas("canvas-test", &document, &ctx)
            .unwrap());
        assert!(
            app.canvas_capture_ready((&document).into()),
            "a validated full-quality gesture can be captured without committing"
        );
        assert_eq!(authority.history_lengths(), [0, 0]);
        assert_eq!(authority.view().revision(), 0);
        app.native_dispatch.clear();
        assert!(!app.canvas_capture_ready((&document).into()));
        drop(connection);
    }
}

#[test]
fn native_animation_steps_are_captured_exactly_and_human_previews_take_precedence() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let before =
        NativeDocument::new_blank(12, 8, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let authority = DocumentAuthority::from_native(before.clone(), 4).unwrap();
    let layer = before.metadata().layers().next().unwrap().id().to_owned();
    let prepared = worker::prepare(
        worker::tests::capture(
            &authority,
            &[json!({"op":"layer.update","layer":layer,"opacity":0.2})],
        ),
        &session,
        &|| Ok(()),
    )
    .unwrap();
    {
        let after = prepared.preview();
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        app.doc_snapshot = after.clone().into();
        app.selected = layer.clone();
        app.animation.observe(
            &crate::engine::view::DocumentSnapshot::Native(before.clone()),
            None,
            0.,
        );
        app.animation.observe(
            &app.doc_snapshot,
            Some(
                crate::engine::AiChange {
                    serial: 1,
                    actor: "agent".into(),
                    label: "Opacity".into(),
                    tool: "opacity".into(),
                    scopes: vec![crate::engine::Scope::layer(&layer)],
                    at: 1,
                }
                .into(),
            ),
            1.,
        );
        let _ = ctx.run(
            egui::RawInput {
                time: Some(1.09),
                ..Default::default()
            },
            |_| {},
        );
        let first = app
            .capture_native_canvas(&ctx, "canvas-test", after)
            .unwrap()
            .unwrap();
        let repeat = app
            .capture_native_canvas(&ctx, "canvas-test", after)
            .unwrap()
            .unwrap();
        assert!(first.presentation.is_some());
        assert!(first.stamp.same_request(&repeat.stamp));
        let _ = ctx.run(
            egui::RawInput {
                time: Some(1.18),
                ..Default::default()
            },
            |_| {},
        );
        let pending = app
            .capture_native_canvas(&ctx, "canvas-test", after)
            .unwrap()
            .unwrap();
        assert!(
            first.stamp.same_request(&pending.stamp),
            "Elapsed UI time cannot supersede an unpublished animation step"
        );
        app.animation
            .native_presented(&before, first.stamp.signature, 1.18);
        assert!(first.stamp.same_request(
            &app.capture_native_canvas(&ctx, "canvas-test", after)
                .unwrap()
                .unwrap()
                .stamp
        ));
        let make_output = |input: &Input| {
            let scene = Arc::new(
                crate::engine::scene::SceneSnapshot::from_native(
                    crate::engine::scene::SceneIdentity {
                        project_id: "canvas-test".into(),
                        document_id: after.metadata().id().into(),
                        revision: after.metadata().revision(),
                    },
                    after.clone(),
                    vec![],
                    None,
                    None,
                )
                .unwrap(),
            );
            let read = runtime.begin_read(&scene).unwrap();
            let (frame, origin) = native_preview::presentation::render(
                scene,
                read,
                input.presentation.as_ref().unwrap(),
                None,
                48,
                None,
                false,
                &|| Ok(()),
            )
            .unwrap();
            upload(frame, Some(origin), &Mutex::new(None), 2048, &|| Ok(())).unwrap()
        };
        let output = make_output(&first);
        output.validate(&first.stamp).unwrap();
        app.preview_epoch.store(1, Ordering::Relaxed);
        app.preview_job = 1;
        app.native_dispatch.expected = Some((1, first.stamp.clone(), true));
        app.native_dispatch.replies.complete(Reply {
            job: 1,
            stamp: first.stamp.clone(),
            result: Ok(output),
        });
        assert!(app
            .present_native_canvas("canvas-test", after, &ctx)
            .unwrap());
        let _ = ctx.run(
            egui::RawInput {
                time: Some(1.22),
                ..Default::default()
            },
            |_| {},
        );
        let half = app
            .capture_native_canvas(&ctx, "canvas-test", after)
            .unwrap()
            .unwrap();
        assert!(!first.stamp.same_request(&half.stamp));
        assert_ne!(
            first.presentation.as_ref().unwrap().signature(),
            half.presentation.as_ref().unwrap().signature()
        );
        let output = make_output(&half);
        output.validate(&half.stamp).unwrap();
        assert!(output.validate(&first.stamp).is_err());
        app.transient = vec![json!({"op":"layer.update","layer":layer,"opacity":0.6})];
        let human = app
            .capture_native_canvas(&ctx, "canvas-test", after)
            .unwrap()
            .unwrap();
        assert!(human.presentation.is_none());
        assert_eq!(human.commands[0]["opacity"], 0.6);
        assert!(request_input(after, &[&human.commands[0]], None)
            .with_presentation(
                after,
                app.animation.native_presentation(after, 1.18).unwrap()
            )
            .is_err());
        app.transient.clear();
        app.animation.cancel();
        assert!(app
            .capture_native_canvas(&ctx, "canvas-test", after)
            .unwrap()
            .unwrap()
            .presentation
            .is_none());
    }
    prepared
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(authority.history_lengths(), [0, 0]);
    drop((authority, before, session, runtime, connection));
}

fn wait(
    dispatch: &mut Dispatch,
    document: &NativeDocument,
    epoch: u64,
) -> Result<Option<native_texture::Prepared>> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let result = dispatch.take("canvas-test", document, epoch);
        if !matches!(result, Ok(None)) || !dispatch.pending() {
            return result;
        }
        assert!(Instant::now() < end, "Canvas dispatcher did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn admitted_commands_bind_content_view_and_pool_without_retaining_source() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let pool = runtime.resident();
    let document = NativeDocument::new_blank(4, 2, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
    let baseline = pool.used();
    let command = json!({"op":"layer.update", "name":"Human draft"});
    let request = request_input(&document, &[&command], None);
    assert!(pool.used() > baseline);
    assert_eq!(request.commands[0], command);
    assert!(request
        .stamp
        .same_request(&request_input(&document, &[&command], None).stamp));
    assert!(!request
        .stamp
        .same_request(&request_input(&document, &[&command], Some("mask")).stamp));
    assert!(!request.stamp.same_request(
        &request_input(
            &document,
            &[&json!({"op":"layer.update", "name":"Changed"})],
            None
        )
        .stamp
    ));
    assert!(document
        .copy_commands_metadata(&vec![&command; 101], &|| Ok(()))
        .is_err());
    assert!(document
        .copy_commands_metadata(&[&command], &|| Err("Cancelled".into()))
        .is_err());
    let other_pool = crate::large_document::ResidentBudget::new(32 * 1024 * 1024).unwrap();
    let other =
        NativeDocument::new_blank(4, 2, 16, &other_pool, &runtime.disk(), &|| Ok(())).unwrap();
    assert!(Input::new(
        &other,
        "canvas-test",
        request.commands.clone(),
        48,
        None,
        false,
        None
    )
    .is_err());
    drop(request);
    assert_eq!(pool.used(), baseline);
    drop((other, document, runtime, connection));
}

#[test]
fn committed_and_transient_replies_retire_sources_before_texture_publication() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    for depth in [8, 16] {
        for variant in 0..3 {
            let transient = variant != 0;
            let authority = DocumentAuthority::from_native(
                NativeDocument::new_blank(
                    4,
                    2,
                    depth,
                    &runtime.resident(),
                    &runtime.disk(),
                    &|| Ok(()),
                )
                .unwrap(),
                8,
            )
            .unwrap();
            let document = authority.native().unwrap();
            let layer = authority.view().layer(0).unwrap().id().to_owned();
            let mut commands = vec![
                json!({"op":"paint.fill", "layer":layer, "rect":[0,0,4,2], "color":[20,101,201,177]}),
                json!({"op":"selection", "kind":"ellipse", "rect":[0,0,3,2]}),
            ];
            if variant == 2 {
                commands.push(json!({"op":"selection.clear"}));
            }
            let borrowed_commands: Vec<_> = commands.iter().collect();
            let input = request_input(
                document,
                if transient { &borrowed_commands } else { &[] },
                Some("mask"),
            );
            let stamp = input.stamp.clone();
            let epoch = Arc::new(AtomicU64::new(1));
            let mut worker = Worker::new(epoch.clone());
            let mut dispatch = Dispatch::default();
            let ctx = egui::Context::default();
            let session = Arc::new(runtime.begin().unwrap());
            let weak_session = Arc::downgrade(&session);
            let scene = Arc::new(
                authority
                    .capture_scene(None, "canvas-test".into(), vec![], None, None)
                    .unwrap()
                    .into_native()
                    .unwrap(),
            );
            let weak_scene = Arc::downgrade(&scene);
            let prepared = transient.then(|| {
                worker::prepare(
                    worker::tests::capture(&authority, &commands),
                    &session,
                    &|| Ok(()),
                )
                .unwrap()
            });
            let expected = prepared.as_ref().map_or(document, |p| p.preview());
            let expected_bounds = expected.metadata().selection();
            let expected_polygon = expected.metadata().selection_polygon().map(<[_]>::to_vec);
            let expected_contours = expected
                .metadata()
                .selection_coverage()
                .map(|coverage| coverage.contours().to_vec());
            let retirement = dispatch.retirement.clone();
            let disk_before = runtime.disk().used().unwrap();
            dispatch
                .queue(&mut worker, stamp, 1, epoch, &ctx, move |check| {
                    let (frame, origin) = if let Some(prepared) = prepared {
                        let result = native_preview::render_prepared(
                            scene,
                            prepared,
                            session,
                            None,
                            48,
                            None,
                            false,
                            Some("mask"),
                            check,
                        )?;
                        let (origin, frame, supplement) = result.into_parts();
                        drop(supplement);
                        (frame, Some(origin))
                    } else {
                        let request = Request::capture_selection(
                            scene,
                            session,
                            None,
                            48,
                            None,
                            false,
                            Some("mask"),
                        )?;
                        let frame = request.render(&mut || check())?;
                        drop(request);
                        (frame, None)
                    };
                    upload(frame, origin, &retirement, 64, check)
                })
                .unwrap();
            let uploads = wait(&mut dispatch, document, 1).unwrap().unwrap();
            assert!(!dispatch.pending());
            assert!(dispatch.unchanged(&input, 1));
            assert!(weak_scene.upgrade().is_none());
            assert!(weak_session.upgrade().is_none());
            assert!(runtime.disk().used().unwrap() <= disk_before);
            assert_eq!(authority.history_lengths(), [0, 0]);
            let output = Output {
                uploads,
                origin: None,
                baseline: None,
                text: None,
            };
            if !transient {
                assert!(output.validate(&input.stamp).is_ok());
                let different_mode = request_input(document, &[], Some("overlay"));
                assert!(output.validate(&different_mode.stamp).is_err());
            } else {
                // Candidate pixels without their baseline origin cannot pass
                // as a committed frame, even at the same document revision.
                assert!(output.validate(&input.stamp).is_err());
            }
            let uploads = output.uploads;
            let tag = uploads.tag().clone();
            let mut presenter = native_texture::Presenter::default();
            presenter
                .present(uploads, &tag, &ctx, &mut || Ok(()))
                .unwrap();
            let outline = presenter.current().unwrap().outline();
            assert_eq!(outline.bounds(), expected_bounds);
            assert_eq!(outline.polygon(), expected_polygon.as_deref());
            assert_eq!(outline.contours(), expected_contours.as_deref());
            worker.stop();
            dispatch.clear();
            drop((presenter, tag, ctx, input, authority));
        }
    }
    drop((runtime, connection));
}

#[test]
fn source_changes_superseded_epochs_and_panics_leave_no_pending_reply() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let document =
        NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let epoch = Arc::new(AtomicU64::new(1));
    let mut worker = Worker::new(epoch.clone());
    let mut dispatch = Dispatch::default();
    let ctx = egui::Context::default();
    let request = request_input(&document, &[], None);
    dispatch
        .queue(
            &mut worker,
            request.stamp.clone(),
            1,
            epoch.clone(),
            &ctx,
            |_| panic!("injected render failure"),
        )
        .unwrap();
    assert!(wait(&mut dispatch, &document, 1)
        .err()
        .unwrap()
        .contains("could not complete"));
    assert!(!dispatch.pending());
    dispatch.expected = Some((1, request.stamp.clone(), true));
    dispatch.replies.complete(Reply {
        job: 1,
        stamp: request.stamp.clone(),
        result: Err("stale".into()),
    });
    assert!(dispatch
        .take("different-project", &document, 1)
        .unwrap()
        .is_none());
    assert!(!dispatch.pending());
    dispatch.expected = Some((1, request.stamp.clone(), true));
    dispatch.replies.complete(Reply {
        job: 1,
        stamp: request.stamp.clone(),
        result: Err("superseded".into()),
    });
    assert!(dispatch
        .take("canvas-test", &document, 2)
        .unwrap()
        .is_none());
    assert!(!dispatch.pending());
    worker.stop();
    drop((dispatch, request, document, runtime, connection));
}

#[test]
fn ui_dispatch_refuses_a_resident_source_without_replacing_the_canvas_or_retrying_each_frame() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let document =
        NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let (mut app, ctx) = crate::ui::tests::fixture();
    let generation = app.canvas_generation;
    app.queue_native_canvas(request_input(&document, &[], None), &ctx)
        .unwrap();
    let job = app.preview_job;
    assert!(app.pending);
    let end = Instant::now() + Duration::from_secs(5);
    let error = loop {
        match app.present_native_canvas("canvas-test", &document, &ctx) {
            Err(error) => break error,
            Ok(shown) => assert!(!shown),
        }
        assert!(Instant::now() < end, "Native UI refusal did not complete");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert!(error.contains("no longer native"));
    assert!(!app.pending);
    assert_eq!(app.canvas_generation, generation);
    app.queue_native_canvas(request_input(&document, &[], None), &ctx)
        .unwrap();
    assert_eq!(app.preview_job, job);
    assert!(!app.pending);
    app.preview_worker.stop();
    app.native_dispatch.clear();
    drop((app, document, runtime, connection));
}

#[test]
fn ui_capture_preserves_command_order_and_shared_stroke_builders() {
    use crate::ui::{ParameterEdit, Tool};
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let document =
        NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let (mut app, ctx) = crate::ui::tests::fixture();
    app.doc_snapshot = document.clone().into();
    app.selected = document.metadata().layers().next().unwrap().id().into();
    app.transient = vec![
        json!({"op":"move", "dx":1}),
        json!({"op":"filter.update", "amount":3}),
    ];
    let source = json!({"op":"source.update", "source":{"kind":"svg", "markup":"<svg/>"}});
    app.native_source_preview = Some(
        SharedCommand::new(document.copy_value_metadata(&source, &|| Ok(())).unwrap()).unwrap(),
    );
    app.native_source_preview_at = 1;
    let parameter = json!({"op":"layer.update", "opacity":0.5});
    app.parameter_gesture = Some(ParameterEdit {
        control: egui::Id::new("native-capture"),
        gesture: "gesture".into(),
        engine: Arc::downgrade(&app.shared),
        document: document.metadata().id().into(),
        revision: document.metadata().revision(),
        commands: vec![parameter.clone()],
        preview: None,
        preview_runs: 0,
        label: "Parameter".into(),
        typing: None,
        dirty: true,
    });
    app.blend_hover = Some(crate::ui::BlendHover { layers: vec![app.selected.clone()], setting: crate::ui::BlendSetting::Mode("multiply") });
    app.points = vec![[1., 1.], [2., 1.]];
    app.stroke_pressures = vec![0.3, 0.7];
    app.stroke_has_pressure = true;
    app.drag_revision = Some(document.metadata().revision());
    for tool in [
        Tool::Brush,
        Tool::Eraser,
        Tool::Smudge,
        Tool::Clone,
        Tool::Heal,
        Tool::Liquify,
    ] {
        app.tool = tool;
        let captured = app
            .capture_native_canvas(&ctx, "canvas-test", &document)
            .unwrap()
            .unwrap();
        assert_eq!(captured.commands.len(), 5);
        assert!(std::ptr::eq(
            &**captured.source.as_ref().unwrap().1.value(),
            &**app.native_source_preview.as_ref().unwrap().value()
        ));
        let assembled = captured.assemble(&document, &|| Ok(())).unwrap();
        assert_eq!(assembled.len(), 6);
        assert_eq!(assembled[0], app.transient[0]);
        assert_eq!(assembled[1], source);
        assert_eq!(assembled[2], app.transient[1]);
        assert_eq!(assembled[3], parameter);
        assert_eq!(
            assembled[4],
            json!({"op":"layer.update", "layer":app.selected, "blend":"multiply"})
        );
        assert_eq!(
            assembled[5],
            if tool == Tool::Liquify {
                app.liquify_command(&app.points)
            } else {
                app.stroke_command(&app.points, app.color)
            }
        );
        let borrowed = captured.commands.iter().collect::<Vec<_>>();
        assert_eq!(
            captured.stamp.signature,
            command_fingerprint(
                &borrowed,
                captured.source.as_ref().map(|(at, source)| (*at, source)),
                8192,
                None,
                false,
                None
            )
            .unwrap()
        );
    }
    app.cancel_source();
    assert!(app.native_source_preview.is_none());
    drop((app, document, runtime, connection));
}

#[test]
fn ui_capture_binds_refinement_modes_and_avoids_recopying_unchanged_commands() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let pool = runtime.resident();
    let document = NativeDocument::new_blank(4, 2, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
    let (mut app, ctx) = crate::ui::tests::fixture();
    app.doc_snapshot = document.clone().into();
    app.selected = document.metadata().layers().next().unwrap().id().into();
    for target in [None, Some(app.selected.clone())] {
        app.open_refinement(&document, target.clone());
        for mode in ["cutout", "mask", "overlay"] {
            app.refinement.as_mut().unwrap().preview = mode.into();
            app.transient = vec![app.refinement.as_ref().unwrap().command()];
            let request = app
                .capture_native_canvas(&ctx, "canvas-test", &document)
                .unwrap()
                .unwrap();
            assert_eq!(
                request.stamp.selection_view.as_deref(),
                target.is_none().then_some(mode)
            );
            assert_eq!(request.stamp.mask, target.is_some() && mode == "mask");
            assert_eq!(
                request.stamp.target,
                target.clone().filter(|_| mode == "mask")
            );
            let epoch = app.preview_epoch.load(Ordering::Relaxed);
            app.native_dispatch.expected = Some((epoch, request.stamp.clone(), false));
            let baseline = pool.used();
            assert!(app
                .capture_native_canvas(&ctx, "canvas-test", &document)
                .unwrap()
                .is_none());
            assert_eq!(pool.used(), baseline);
            app.native_dispatch.clear();
        }
    }
    let before = pool.used();
    let held = pool
        .reserve_bytes(pool.limit() - before, pool.limit())
        .unwrap();
    assert!(app
        .capture_native_canvas(&ctx, "canvas-test", &document)
        .is_err());
    drop(held);
    assert_eq!(pool.used(), before);
    app.transient = vec![json!({"op":"layer.update"}); 101];
    assert!(app
        .capture_native_canvas(&ctx, "canvas-test", &document)
        .is_err());
    assert_eq!(pool.used(), before);
    drop((app, document, runtime, connection));
}

#[test]
fn native_wand_parameter_preview_keeps_cutout_on_the_full_and_opt_in_grids() {
    use crate::engine::scene::{CapturedScene, LegacyTileCache};
    use crate::engine::{Document, Engine};
    use crate::ui::ParameterEdit;

    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let mut original = Document::new_depth(768, 8, depth).unwrap();
        for y in 2..6 {
            for x in 4..8 {
                original.layers[0].pixels.set16(x, y, [49317, 7199, 29051, 65535]);
            }
            original.layers[0].pixels.set16(700, y, [5911, 31991, 44317, 65535]);
        }
        let layer = original.layers[0].id.clone();
        let pick = json!({"op":"selection","kind":"wand","layer":layer,"point":[5,3],"tolerance":0});
        let update = json!({"op":"selection.wand_update","tolerance":18,"feather":1});
        let oracle = Engine::preview_edits(original.clone(), &[pick.clone(), update.clone()]).unwrap();
        let mut expected = oracle.preview(None, 8192, None, false).unwrap().2;
        crate::selection::display::apply(&oracle, 768, 8, &mut expected, "cutout");
        assert_eq!(expected[(3 * 768 + 700) * 4 + 3], 0);
        assert_ne!(expected[(3 * 768 + 5) * 4 + 3], 0);

        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(original, "canvas-test".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = Arc::new(staged.session.take().unwrap());
        let authority = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let (mut app, ctx) = crate::ui::tests::fixture();
        app.preview_quality = crate::ui::preview_quality::Quality::load(std::path::Path::new(""));
        let viewport = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(384., 4.));
        app.view_rect = Some(viewport);
        app.canvas_viewport = Some(viewport);
        app.transient = vec![pick];
        app.parameter_gesture = Some(ParameterEdit {
            control: egui::Id::new("wand preview quality"), gesture: "wand".into(),
            engine: Arc::downgrade(&app.shared), document: native.metadata().id().into(),
            revision: native.metadata().revision(), commands: vec![update],
            preview: None, preview_runs: 0, label: "Wand feather".into(), typing: None, dirty: true,
        });
        let capture = |app: &mut crate::ui::PeerBrush, time| {
            ctx.begin_pass(egui::RawInput { time: Some(time), ..Default::default() });
            let input = app.capture_native_canvas(&ctx, "canvas-test", &native).unwrap().unwrap();
            let _ = ctx.end_pass();
            assert_eq!(input.stamp.selection_view.as_deref(), Some("cutout"));
            input
        };
        let full = capture(&mut app, 1.);
        assert_eq!(full.stamp.edge, 8192);
        app.set_preview_quality(true).unwrap();
        assert_eq!(capture(&mut app, 2.).stamp.edge, 512);
        assert_eq!(capture(&mut app, 2.2).stamp.edge, full.stamp.edge);
        let commands = full.assemble(&native, &|| Ok(())).unwrap();
        let prepared = worker::prepare(worker::tests::capture(&authority, &commands), &session, &|| Ok(())).unwrap();
        let scene = Arc::new(authority.capture_scene(None, "canvas-test".into(), vec![], None, None)
            .unwrap().into_native().unwrap());
        let (_, frame, _) = native_preview::render_prepared(
            scene, prepared, session.clone(), full.stamp.rect, full.stamp.edge,
            full.stamp.target.as_deref(), full.stamp.mask, full.stamp.selection_view.as_deref(), &|| Ok(()),
        ).unwrap().into_parts();
        assert_eq!((frame.key().width, frame.key().height), (768, 8));
        assert_eq!(&*frame.copy_rgba8(&mut || Ok(())).unwrap(), expected.as_slice());
        assert_eq!(authority.history_lengths(), [0, 0]);
        assert!(native.metadata().selection().is_none());
        drop((frame, app, authority, native, session, staged.seals, cache, runtime, connection));
    }
}

#[test]
fn cropped_native_gestures_reduce_only_when_visible_samples_exceed_the_display() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let document = NativeDocument::new_blank(
            8192, 8192, depth, &runtime.resident(), &runtime.disk(), &|| Ok(()),
        ).unwrap();
        for (canvas_side, viewport_size, expected_edge, expected_rect, full_rect) in [
            (8192., egui::vec2(32., 24.), 8192, [394, 295, 438, 329], [394, 295, 438, 329]),
            (4096., egui::vec2(800., 600.), 800, [798, 598, 2402, 1802], [766, 566, 2434, 1834]),
        ] {
            let (mut app, ctx) = crate::ui::tests::fixture();
            app.set_preview_quality(true).unwrap();
            app.transient = vec![json!({"op":"layer.update", "opacity":0.5})];
            app.view_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::Vec2::splat(canvas_side)));
            app.canvas_viewport = Some(egui::Rect::from_min_size(egui::pos2(400., 300.), viewport_size));
            ctx.begin_pass(egui::RawInput { time: Some(1.), ..Default::default() });
            let input = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
            let _ = ctx.end_pass();
            assert_eq!(input.stamp.edge, expected_edge);
            assert_eq!(input.stamp.rect, Some(expected_rect));
            app.set_preview_quality(false).unwrap();
            ctx.begin_pass(egui::RawInput { time: Some(1.01), ..Default::default() });
            let full = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
            let _ = ctx.end_pass();
            assert_eq!(full.stamp.edge, 8192);
            assert_eq!(full.stamp.rect, Some(full_rect));
            app.transient.clear();
            app.set_preview_quality(true).unwrap();
            let snapshot = crate::engine::view::DocumentSnapshot::from(document.clone());
            app.animation.observe(&snapshot, None, 0.);
            assert!(app.animation.observe(&snapshot, Some(crate::engine::AiChange {
                serial: 1, actor: "agent".into(), label: "Settled paint".into(), tool: "paint".into(),
                scopes: vec![], at: 1,
            }.into()), 0.1));
            assert!(app.animation.active(1.02));
            assert_eq!(app.animation.progress(1.02), 1.);
            ctx.begin_pass(egui::RawInput { time: Some(1.02), ..Default::default() });
            let settled = app.capture_native_canvas(&ctx, "canvas-test", &document).unwrap().unwrap();
            let _ = ctx.end_pass();
            assert_eq!(settled.stamp.edge, 8192);
            assert_eq!(settled.stamp.rect, Some(full_rect));
        }
        drop(connection);
    }
}

#[test]
fn native_geometry_preview_uses_the_draft_extent_and_keeps_far_edge_pixels() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let document = NativeDocument::new_blank(
        32,
        8,
        16,
        &runtime.resident(),
        &runtime.disk(),
        &|| Ok(()),
    )
    .unwrap();
    let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
    let layer = document.metadata().layers().next().unwrap().id();
    let (mut app, ctx) = crate::ui::tests::fixture();
    app.set_preview_quality(false).unwrap();
    app.transient = vec![
        json!({"op":"paint.fill", "layer":layer, "rect":[31,1,32,2], "color":[31,73,149,255]}),
        json!({"op":"canvas.resize", "width":16000, "height":8, "anchor":"top_right"}),
    ];
    let canvas = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(16000., 8.));
    app.view_rect = Some(canvas);
    app.canvas_viewport = Some(canvas);
    ctx.begin_pass(egui::RawInput::default());
    let full = app
        .capture_native_canvas(&ctx, "canvas-test", &document)
        .unwrap()
        .unwrap();
    assert_eq!(full.stamp.edge, 16000);
    assert_eq!(full.stamp.rect, None);

    app.canvas_viewport = Some(egui::Rect::from_min_max(egui::pos2(15976., 0.), canvas.max));
    let visible = app
        .capture_native_canvas(&ctx, "canvas-test", &document)
        .unwrap()
        .unwrap();
    let _ = ctx.end_pass();
    assert_eq!(visible.stamp.edge, 16000);
    assert_eq!(visible.stamp.rect, Some([15974, 0, 16000, 8]));
    assert!(request_input(&document, &[], None)
        .with_region(&document, visible.stamp.rect)
        .is_err());

    let session = Arc::new(runtime.begin().unwrap());
    let baseline = runtime.disk().used().unwrap();
    let scene = Arc::new(
        authority
            .capture_scene(None, "canvas-test".into(), vec![], None, None)
            .unwrap()
            .into_native()
            .unwrap(),
    );
    let commands = visible.assemble(&document, &|| Ok(())).unwrap();
    let prepared = worker::prepare(
        worker::tests::capture(&authority, &commands),
        &session,
        &|| Ok(()),
    )
    .unwrap();
    let rendered = native_preview::render_prepared_with(
        scene,
        prepared,
        session,
        visible.stamp.rect,
        visible.stamp.edge,
        None,
        false,
        None,
        &|| Ok(()),
        true,
        |request, check| request.render(&mut || check()),
    )
    .unwrap();
    let (origin, frame, _) = rendered.into_parts();
    assert_eq!(frame.key().rect, [15974, 0, 16000, 8]);
    assert_eq!((frame.key().width, frame.key().height), (26, 8));
    let bytes = frame.copy_rgba8(&mut || Ok(())).unwrap();
    assert_eq!(
        &bytes[(26 + 24) * 4..(26 + 26) * 4],
        &[0, 0, 0, 0, 31, 73, 149, 255]
    );
    let output = upload(frame, Some(origin), &Mutex::new(None), 64, &|| Ok(())).unwrap();
    output.validate(&visible.stamp).unwrap();
    assert_eq!(authority.native().unwrap().metadata().width(), 32);
    assert_eq!(authority.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    drop((output, app, authority, document, runtime, connection));
}

#[test]
fn ui_capture_requests_full_quality_after_an_unchanged_native_gesture_settles() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let document = NativeDocument::new_blank(
        8192,
        1,
        16,
        &runtime.resident(),
        &runtime.disk(),
        &|| Ok(()),
    )
    .unwrap();
    let (mut app, ctx) = crate::ui::tests::fixture();
    app.set_preview_quality(true).unwrap();
    app.transient = vec![json!({"op":"layer.update", "opacity":0.5})];
    for (time, expected_edge) in [
        (1.0, Some(1536)),
        (1.05, None),
        (1.2, Some(8192)),
        (1.21, Some(1536)),
    ] {
        if time == 1.21 {
            app.transient[0]["opacity"] = json!(0.6);
        }
        ctx.begin_pass(egui::RawInput {
            time: Some(time),
            ..Default::default()
        });
        let result = app
            .capture_native_canvas(&ctx, "canvas-test", &document)
            .unwrap();
        let _ = ctx.end_pass();
        assert_eq!(
            result.as_ref().map(|request| request.stamp.edge),
            expected_edge
        );
        if let Some(request) = result {
            app.native_dispatch.expected = Some((
                app.preview_epoch.load(Ordering::Relaxed),
                request.stamp.clone(),
                false,
            ));
        }
    }
    app.native_dispatch.clear();
    drop((app, document, runtime, connection));
}

#[test]
fn shared_source_commands_bind_content_order_pool_and_cancellable_assembly() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let pool = runtime.resident();
    let document = NativeDocument::new_blank(4, 2, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
    let source = SharedCommand::new(
        document
            .copy_value_metadata(
                &json!({"op":"source.update", "source":{"content":{"kind":"svg", "svg":"<svg/>"}}}),
                &|| Ok(()),
            )
            .unwrap(),
    )
    .unwrap();
    let changed = SharedCommand::new(document.copy_value_metadata(&json!({"op":"source.update", "source":{"content":{"kind":"svg", "svg":"<svg><path/></svg>"}}}), &|| Ok(())).unwrap()).unwrap();
    let command = json!({"op":"layer.update", "opacity":0.5});
    let commands = document
        .copy_commands_metadata(&[&command], &|| Ok(()))
        .unwrap();
    let draft = [&command];
    for source in [None, Some((1, &source))] {
        let target = Some("Paint \"detail\"");
        let mode = Some("overlay");
        let capture = CommandCapture::new(&draft, source, target, true, mode).unwrap();
        let expected = |edge: u32| -> [u8; 32] {
            let bytes = match source {
                Some((at, source)) => {
                    serde_json::to_vec(&((&draft, at, source.signature), edge, target, true, mode))
                }
                None => serde_json::to_vec(&(&draft, edge, target, true, mode)),
            }
            .unwrap();
            ring::digest::digest(&ring::digest::SHA256, &bytes)
                .as_ref()
                .try_into()
                .unwrap()
        };
        for edge in [512, 8192] {
            assert_eq!(capture.signature(edge).unwrap(), expected(edge));
        }
        let admitted = capture.admit(&document, "canvas-test", 512).unwrap();
        assert_eq!(admitted.stamp.signature, expected(512));
        assert_eq!(admitted.commands.as_slice(), &[command.clone()]);
    }
    let make = |at, source| {
        Input::with_source(
            &document,
            "canvas-test",
            commands.clone(),
            Some((at, source)),
            48,
            None,
            false,
            None,
        )
    };
    let first = make(0, source.clone()).unwrap();
    let last = make(1, source.clone()).unwrap();
    assert!(!first.stamp.same_request(&last.stamp));
    assert!(!first.stamp.same_request(&make(0, changed).unwrap().stamp));
    assert!(make(2, source.clone()).is_err());
    assert_eq!(
        first.assemble(&document, &|| Ok(())).unwrap()[0],
        **source.value()
    );
    assert_eq!(
        last.assemble(&document, &|| Ok(())).unwrap()[1],
        **source.value()
    );
    let baseline = pool.used();
    let checks = std::cell::Cell::new(0);
    assert!(first
        .assemble(&document, &|| {
            checks.set(checks.get() + 1);
            if checks.get() > 4 {
                Err("Cancelled".into())
            } else {
                Ok(())
            }
        })
        .is_err());
    assert_eq!(pool.used(), baseline);
    let held = pool
        .reserve_bytes(pool.limit() - baseline, pool.limit())
        .unwrap();
    assert!(first.assemble(&document, &|| Ok(())).is_err());
    drop(held);
    assert_eq!(pool.used(), baseline);
    let other = NativeDocument::new_blank(4, 2, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
    assert!(first.assemble(&other, &|| Ok(())).is_err());
    let foreign_pool = crate::large_document::ResidentBudget::new(32 * 1024 * 1024).unwrap();
    let foreign_doc =
        NativeDocument::new_blank(4, 2, 16, &foreign_pool, &runtime.disk(), &|| Ok(())).unwrap();
    let foreign = SharedCommand::new(
        foreign_doc
            .copy_value_metadata(&command, &|| Ok(()))
            .unwrap(),
    )
    .unwrap();
    assert!(make(0, foreign).is_err());
    let crowded = document
        .copy_commands_metadata(&vec![&command; 100], &|| Ok(()))
        .unwrap();
    assert!(Input::with_source(
        &document,
        "canvas-test",
        crowded,
        Some((0, source.clone())),
        48,
        None,
        false,
        None
    )
    .is_err());
    drop((
        first,
        last,
        source,
        commands,
        other,
        foreign_doc,
        document,
        runtime,
        connection,
    ));
}

#[test]
fn queued_source_assembly_renders_original_precision_and_releases_command_before_reply() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 16 * 1024 * 1024);
    for depth in [8, 16] {
        let authority = DocumentAuthority::from_native(
            NativeDocument::new_blank(
                4,
                2,
                depth,
                &runtime.resident(),
                &runtime.disk(),
                &|| Ok(()),
            )
            .unwrap(),
            8,
        )
        .unwrap();
        let document = authority.native().unwrap();
        let value = json!({"op":"source.add", "x":0, "y":0, "source":{
            "width":4, "height":2, "content":{"kind":"shape", "shape":"rectangle", "bounds":[0,0,4,2],
            "points":[], "closed":true, "fill":[12347,23459,34571,65535], "stroke":[0,0,0,0], "stroke_width":0}
        }});
        let oracle = crate::source::parse(&value)
            .unwrap()
            .render(4, 2, depth)
            .unwrap()
            .rgba();
        let source =
            SharedCommand::new(document.copy_value_metadata(&value, &|| Ok(())).unwrap()).unwrap();
        let weak_command = Arc::downgrade(source.value().owner());
        let input = Input::with_source(
            document,
            "canvas-test",
            document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            Some((0, source)),
            48,
            None,
            false,
            None,
        )
        .unwrap();
        let captured = document.clone();
        let scene = Arc::new(
            authority
                .capture_scene(None, "canvas-test".into(), vec![], None, None)
                .unwrap()
                .into_native()
                .unwrap(),
        );
        let weak_scene = Arc::downgrade(&scene);
        let session = Arc::new(runtime.begin().unwrap());
        let weak_session = Arc::downgrade(&session);
        let disk_before = runtime.disk().used().unwrap();
        let epoch = Arc::new(AtomicU64::new(1));
        let mut thread = Worker::new(epoch.clone());
        let mut dispatch = Dispatch::default();
        let retirement = dispatch.retirement.clone();
        let ctx = egui::Context::default();
        let ui_thread = std::thread::current().id();
        dispatch
            .queue(
                &mut thread,
                input.stamp.clone(),
                1,
                epoch,
                &ctx,
                move |check| {
                    assert_ne!(std::thread::current().id(), ui_thread);
                    let commands = input.assemble(&captured, check)?;
                    let state = DocumentAuthority::from_native(captured, 8)?;
                    let prepared =
                        worker::prepare(worker::tests::capture(&state, &commands), &session, check)
                            .map_err(|failure| format!("{failure:?}"))?;
                    drop((commands, input, state));
                    let result = native_preview::render_prepared(
                        scene, prepared, session, None, 48, None, false, None, check,
                    )?;
                    let (origin, frame, supplement) = result.into_parts();
                    assert_eq!(&*frame.copy_rgba8(&mut || check())?, &oracle);
                    drop(supplement);
                    upload(frame, Some(origin), &retirement, 64, check)
                },
            )
            .unwrap();
        let uploads = wait(&mut dispatch, document, 1).unwrap().unwrap();
        assert!(weak_command.upgrade().is_none());
        assert!(weak_scene.upgrade().is_none());
        assert!(weak_session.upgrade().is_none());
        assert_eq!(runtime.disk().used().unwrap(), disk_before);
        assert_eq!(authority.history_lengths(), [0, 0]);
        assert_eq!(document.metadata().depth(), depth);
        thread.stop();
        dispatch.clear();
        drop((uploads, authority));
    }
    drop((runtime, connection));
}
