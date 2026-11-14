use super::*;
use crate::engine::{native::NativeDocument, Engine};
use crate::large_document::{DiskBudget, ResidentBudget};
use std::time::{Duration, Instant};

fn request(app: &PeerBrush, doc: &Document, point: [f32; 2]) -> Arc<Request> {
    Arc::new(
        Request::capture(
            &app.shared,
            &app.project_id,
            doc.into(),
            point,
            &app.selected,
            app.mask,
            app.mask_step.as_deref(),
        )
        .unwrap(),
    )
}
fn wait(sampler: &mut Sampler, ticket: Ticket, app: &PeerBrush) -> Result<Pixel, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(sample) = sampler.take(
            ticket,
            &app.shared,
            &app.project_id,
            &app.selected,
            app.mask,
            app.mask_step.as_deref(),
        )? {
            return Ok(sample.display());
        }
        assert!(Instant::now() < deadline, "Sampling did not finish");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn worker_matches_shared_sampling_and_preserves_source_and_history_at_both_depths() {
    for depth in [8, 16] {
        for mask in [false, true] {
            let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
            app.mask = mask;
            let before = super::super::workspace_tests::source(&app);
            let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
            let expected = crate::render::sample::resident(
                &doc,
                [1, 1],
                mask.then_some(app.selected.as_str()),
            )
            .unwrap();
            let mut sampler = Sampler::default();
            // Submission and polling cannot wait on the engine on the UI thread.
            let held = app.shared.lock().unwrap();
            let ticket = sampler
                .request(
                    &app.shared,
                    &app.project_id,
                    (&doc).into(),
                    [1.9, 1.1],
                    &app.selected,
                    mask,
                    None,
                    &ctx,
                )
                .unwrap();
            assert_eq!(
                ticket,
                sampler
                    .request(
                        &app.shared,
                        &app.project_id,
                        (&doc).into(),
                        [1.1, 1.9],
                        &app.selected,
                        mask,
                        None,
                        &ctx
                    )
                    .unwrap()
            );
            assert!(sampler
                .take(
                    ticket,
                    &app.shared,
                    &app.project_id,
                    &app.selected,
                    mask,
                    None
                )
                .unwrap()
                .is_none());
            assert!(sampler.pending.as_ref().unwrap().ready.is_none());
            drop(held);
            let sample = wait(&mut sampler, ticket, &app).unwrap();
            assert_eq!(sample, expected);
            assert!(sampler.pending.is_none());
            sampler.stop();
            assert_eq!(super::super::workspace_tests::source(&app), before);
        }
    }
}

#[test]
fn ready_reply_survives_busy_engine_and_latest_request_retires_source_owners() {
    let (app, ctx) = super::super::workspace_tests::fixture(16);
    let doc = Arc::new(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone());
    let weak = Arc::downgrade(&doc);
    let input = request(&app, &doc, [1., 1.]);
    let mut sampler = Sampler::default();
    let (release, gate) = mpsc::channel();
    let (entered, running) = mpsc::channel();
    let held = doc.clone();
    let old = sampler
        .queue(input.clone(), &ctx, move |check| {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            check()?;
            crate::render::sample::resident_color(&held, [1, 1], None, check)
        })
        .unwrap();
    running.recv_timeout(Duration::from_secs(5)).unwrap();
    let current = sampler
        .queue(input, &ctx, |_| Ok(Color::Byte([11, 22, 33, 255])))
        .unwrap();
    assert_ne!(old, current);
    drop(doc);
    release.send(()).unwrap();
    // Waiting for source retirement proves the active obsolete work returned.
    let deadline = Instant::now() + Duration::from_secs(5);
    while weak.upgrade().is_some() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    }
    // Hold a completed reply to exercise polling under engine contention.
    let reply = sampler
        .pending
        .as_ref()
        .unwrap()
        .receiver
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    sender.send(reply).unwrap();
    sampler.pending.as_mut().unwrap().receiver = receiver;
    let held = app.shared.lock().unwrap();
    assert!(sampler
        .take(
            current,
            &app.shared,
            &app.project_id,
            &app.selected,
            false,
            None
        )
        .unwrap()
        .is_none());
    assert!(sampler.pending.as_ref().unwrap().ready.is_some());
    drop(held);
    assert!(sampler
        .take(
            old,
            &app.shared,
            &app.project_id,
            &app.selected,
            false,
            None
        )
        .unwrap()
        .is_none());
    assert_eq!(
        wait(&mut sampler, current, &app).unwrap(),
        [11, 22, 33, 255]
    );
    sampler.stop();
}

#[test]
fn source_target_replacement_cancellation_and_worker_failure_never_publish_color() {
    for change in [
        "revision", "document", "project", "engine", "layer", "mask", "step", "closed", "cancel",
        "panic",
    ] {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let mut sampler = Sampler::default();
        let ticket = sampler
            .queue(request(&app, &doc, [1., 1.]), &ctx, move |_| {
                if change == "panic" {
                    panic!("injected sampling failure");
                }
                Ok(Color::Byte([9, 8, 7, 255]))
            })
            .unwrap();
        match change {
            "revision" => app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.revision += 1,
            "document" => app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.id = "replacement".into(),
            "project" => app.project_id = "other".into(),
            "engine" => app.shared = Arc::new(std::sync::Mutex::new(Engine::new())),
            "layer" => app.selected = "other".into(),
            "mask" => app.mask = true,
            "step" => app.mask_step = Some("other".into()),
            "closed" => app.shared.lock().unwrap().closed = true,
            "cancel" => sampler.cancel(),
            _ => (),
        }
        if change == "cancel" {
            assert!(sampler
                .take(
                    ticket,
                    &app.shared,
                    &app.project_id,
                    &app.selected,
                    false,
                    None
                )
                .unwrap()
                .is_none());
        } else {
            let error = wait(&mut sampler, ticket, &app).err().unwrap();
            assert!(error.contains(if change == "panic" {
                "could not complete"
            } else {
                "changed"
            }));
        }
        sampler.stop();
        assert!(sampler.pending.is_none());
    }
}

#[test]
fn native_request_identity_and_metadata_admission_are_bounded() {
    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let doc = NativeDocument::new_blank(
        32,
        24,
        16,
        &pool,
        &DiskBudget::new(1024 * 1024, 4).unwrap(),
        &|| Ok(()),
    )
    .unwrap();
    let shared = Arc::new(std::sync::Mutex::new(Engine::new()));
    let layer = doc.metadata().layers().next().unwrap().id().to_owned();
    let baseline = pool.used();
    let stamp = Request::capture(
        &shared,
        "sample",
        (&doc).into(),
        [1., 1.],
        &layer,
        false,
        None,
    )
    .unwrap();
    assert!(pool.used() > baseline);
    assert!(stamp.source_matches(&shared, "sample", (&doc).into()));
    let replacement = doc
        .adopt(
            doc.capture_edit()
                .unwrap()
                .visibility(&layer, false, &|| Ok(()))
                .unwrap(),
        )
        .unwrap();
    assert!(!stamp.source_matches(&shared, "sample", (&replacement).into()));
    let full = pool
        .reserve_bytes(pool.limit() - pool.used(), pool.limit())
        .unwrap();
    assert!(Request::capture(
        &shared,
        "sample",
        (&doc).into(),
        [1., 1.],
        &layer,
        false,
        None
    )
    .is_err());
    drop((full, stamp, replacement));
    assert_eq!(pool.used(), baseline);
    drop(doc);
    assert_eq!(pool.used(), 0);
}

#[test]
fn picker_publishes_only_while_input_context_is_current() {
    for action in [
        "ready",
        "tool",
        "manual",
        "mask_value",
        "focus",
        "escape",
        "target",
        "revision",
    ] {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        app.select_tool(Tool::Picker);
        let before = super::super::workspace_tests::source(&app);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let expected = crate::render::sample::resident(&doc, [1, 1], None).unwrap();
        let original = app.color;
        app.queue_color_pick(&ctx, (&doc).into(), [1., 1.]);
        assert!(app.color_pick.is_some());
        assert_eq!(app.color, original);
        match action {
            "tool" => app.select_tool(Tool::Brush),
            "manual" => app.color = [10, 20, 30, 255],
            "mask_value" => app.mask_value ^= 255,
            "target" => app.mask = true,
            "revision" => app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.revision += 1,
            _ => (),
        }
        let unchanged = app.color;
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.color_pick.is_some() {
            let _ = ctx.run(
                egui::RawInput {
                    focused: action != "focus",
                    events: if action == "escape" {
                        vec![egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        }]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
                |ctx| app.poll_color_pick(ctx),
            );
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        }
        app.color_sampler.stop();
        assert_eq!(
            app.color,
            if action == "ready" {
                expected
            } else {
                unchanged
            }
        );
        if action != "revision" {
            assert_eq!(super::super::workspace_tests::source(&app), before);
        }
    }
}

#[test]
fn invalid_new_point_cancels_an_older_sample() {
    let (app, ctx) = super::super::workspace_tests::fixture(8);
    let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
    let mut sampler = Sampler::default();
    sampler
        .queue(request(&app, &doc, [1., 1.]), &ctx, |_| Ok(Color::Byte([1, 2, 3, 255])))
        .unwrap();
    assert!(sampler
        .request(
            &app.shared,
            &app.project_id,
            (&doc).into(),
            [f32::NAN, 1.],
            &app.selected,
            false,
            None,
            &ctx
        )
        .is_err());
    assert!(sampler.pending.is_none());
    sampler.stop();
}
