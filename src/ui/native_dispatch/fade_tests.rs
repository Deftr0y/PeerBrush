use super::*;
use crate::{
    engine::{
        authority::DocumentAuthority,
        native::worker,
        scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
        Document,
    },
    render::native_preview::DisplayRequest,
    workspace::source_runtime::SourceRuntime,
};

fn frame(runtime: &Arc<SourceRuntime>, document: &NativeDocument, edge: u32) -> Frame {
    let scene = Arc::new(
        SceneSnapshot::from_native(
            SceneIdentity {
                project_id: "canvas-test".into(),
                document_id: document.metadata().id().into(),
                revision: document.metadata().revision(),
            },
            document.clone(),
            vec![],
            None,
            None,
        )
        .unwrap(),
    );
    let weak_scene = Arc::downgrade(&scene);
    let session = Arc::new(runtime.begin().unwrap());
    let weak_session = Arc::downgrade(&session);
    let request = Request::capture(scene, session, None, edge, None, false).unwrap();
    let frame = request.render(&mut || Ok(())).unwrap();
    drop(request);
    assert!(weak_scene.upgrade().is_none());
    assert!(weak_session.upgrade().is_none());
    frame
}

fn publish(
    app: &mut super::super::PeerBrush,
    ctx: &egui::Context,
    document: &NativeDocument,
    input: &Input,
    output: Result<Output>,
    job: u64,
) -> Result<bool> {
    app.preview_epoch.store(job, Ordering::Relaxed);
    app.preview_job = job;
    app.native_dispatch
        .endpoint
        .watch(input.stamp.source, &app.preview_epoch);
    app.native_dispatch.expected = Some((job, input.stamp.clone(), true));
    app.native_dispatch.replies.complete(Reply {
        job,
        stamp: input.stamp.clone(),
        result: output,
    });
    app.present_native_canvas("canvas-test", document, ctx)
}

#[test]
fn uncached_fades_keep_the_observed_start_across_published_steps() {
    for (depth, width, initial_edge) in [(8, 21, 8192), (16, 21, 8192), (8, 2048, 1536), (16, 2048, 1536)] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let before = Document::new_depth(width, 17, depth).unwrap();
        let mut after = before.clone();
        after.revision += 1;
        after.layers[0].pixels.set16(4, 5, [12347, 23459, 61237, 32771]);
        let stage = |document| {
            let staged = CapturedScene::from_parts(document, "canvas-test".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(())).unwrap();
            (NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap(), staged.seals)
        };
        let (old, _old_seals) = stage(before);
        let (current, _current_seals) = stage(after);
        let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
        app.set_preview_quality(initial_edge < width).unwrap();
        app.project_id = "canvas-test".into();
        app.doc_snapshot = old.clone().into();
        app.selected = old.metadata().layers().next().unwrap().id().into();
        let initial = Input::new(&old, "canvas-test", old.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            initial_edge, None, false, None).unwrap();
        let output = upload(frame(&runtime, &old, initial_edge), None, &app.native_dispatch.retirement, 64, &|| Ok(())).unwrap();
        assert!(output.baseline.is_none());
        assert!(publish(&mut app, &ctx, &old, &initial, Ok(output), 1).unwrap());
        let observed = app.native_canvas.current().unwrap().tag().clone();
        assert_eq!(observed.key().width, width.min(initial_edge));
        app.animation.observe(&app.doc_snapshot, None, 0.);
        app.doc_snapshot = current.clone().into();
        app.animation.observe(&app.doc_snapshot, Some(crate::engine::AiChange {
            serial: 1, actor: "agent".into(), label: "Paint".into(), tool: "paint".into(),
            scopes: vec![crate::engine::Scope::layer(&app.selected)], at: 1,
        }.into()), 1.);
        let request = |document: &NativeDocument, edge| {
            let scene = Arc::new(SceneSnapshot::from_native(SceneIdentity {
                project_id: "canvas-test".into(), document_id: document.metadata().id().into(),
                revision: document.metadata().revision(),
            }, document.clone(), vec![], None, None).unwrap());
            let session = runtime.begin_read(&scene).unwrap();
            Request::capture(scene, session, None, edge, None, false).unwrap()
        };
        for step in 0..3 {
            let _ = ctx.run(egui::RawInput { time: Some(1. + f64::from(step) * 0.04), ..Default::default() }, |_| {});
            let input = app.capture_native_canvas(&ctx, "canvas-test", &current).unwrap().unwrap();
            assert!(input.blend.is_none());
            assert_eq!(input.stamp.edge, 8192);
            let (blend, before, shown) = input.streamed_blend.as_ref().expect("Continue the streamed fade after publication");
            assert!(shown.same_allocation(&observed));
            assert_eq!(before.key(), old.key());
            let streamed = blend.streamed(request(before, input.stamp.edge), request(&current, input.stamp.edge), &|| Ok(())).unwrap();
            assert_eq!(streamed.key().width, width);
            let owner = upload_retirement(&pool, &app.native_dispatch.retirement).unwrap();
            let output = Output {
                uploads: native_texture::Prepared::from_request(&streamed, &owner, 64, None, None, &mut || Ok(())).unwrap(),
                origin: Some(streamed.origin()), baseline: None, text: None,
            };
            drop(streamed);
            assert!(publish(&mut app, &ctx, &current, &input, Ok(output), 2 + step as u64).unwrap());
            assert!(!app.canvas_capture_ready((&current).into()));
            assert!(app.native_dispatch.fade_baseline(&old, "canvas-test", None, false).is_none());
        }
        let painted = app.native_canvas.current().unwrap().tag();
        let receipt = app.native_receipt.as_ref().unwrap();
        assert!(receipt.streamed_before("other-project", &current, painted).is_none());
        assert!(receipt.streamed_before("canvas-test", &old, painted).is_none());
        assert!(receipt.streamed_before("canvas-test", &current, &observed).is_none());
        assert!(app.capture_native_canvas(&ctx, "other-project", &current).unwrap().unwrap().streamed_blend.is_none());
        app.animation.stop_native_canvas();
        assert!(app.capture_native_canvas(&ctx, "canvas-test", &current).unwrap().unwrap().streamed_blend.is_none());
    }
}

#[test]
fn optional_baseline_leaves_room_for_upload_and_retains_normal_fades() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) =
            fixture.runtime_with_budgets(64 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let document = NativeDocument::new_blank(
            257, 1025, depth, &pool, &runtime.disk(), &|| Ok(()),
        ).unwrap();
        let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        let metadata = serde_json::to_value(document.metadata()).unwrap();
        let input = Input::new(
            &document, "canvas-test",
            document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            8192, None, false, None,
        ).unwrap();
        let endpoint = cache::Endpoint::default();
        let retirement = Mutex::new(Some(native_texture::Retirement::new(&pool).unwrap()));
        let pixels = frame(&runtime, &document, 8192);
        assert!(native_texture::Prepared::retained_upload_bytes(&pixels, 64, false).unwrap() > 800 * 1024);
        let pressure = pool.reserve_bytes(
            pool.limit() - pool.used() - 800 * 1024, pool.limit(),
        ).unwrap();
        let output = finish_frame(
            &input, &document, pixels, None, None, &endpoint, endpoint.generation(),
            &retirement, 64, None, &|| Ok(()),
        ).unwrap();
        assert!(output.baseline.is_none());
        drop((output, pressure));
        let ctx = egui::Context::default();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let output = std::thread::scope(|scope| {
            let worker = scope.spawn(|| {
                finish_frame(
                    &input, &document, frame(&runtime, &document, 8192), None, None,
                    &endpoint, endpoint.generation(), &retirement, 64, Some((&ctx, None)),
                    &|| if std::time::Instant::now() < deadline { Ok(()) } else { Err("Test upload stalled".into()) },
                )
            });
            let mut uploads = 0;
            while !worker.is_finished() {
                uploads += ctx.tex_manager().write().take_delta().set.len();
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            assert!(uploads > 0);
            worker.join().unwrap().unwrap()
        });
        assert!(output.baseline.is_some());
        assert_eq!(serde_json::to_value(document.metadata()).unwrap(), metadata);
        assert_eq!(authority.history_lengths(), [0, 0]);
    }
}

#[test]
fn raster_fades_use_published_baselines_reuse_endpoints_and_finish_on_committed_capture() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 32 * 1024 * 1024);
        let pool = runtime.resident();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut before = Document::new_depth(21, 17, depth).unwrap();
        before.layers[0]
            .pixels
            .set16(4, 5, [61237, 23459, 12347, 49157]);
        let mut after = before.clone();
        after.revision += 1;
        after.layers[0]
            .pixels
            .set16(4, 5, [12347, 23459, 61237, 32771]);
        let original = serde_json::to_value(&after).unwrap();
        let a = CapturedScene::from_parts(before, "canvas-test".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
        let old =
            NativeDocument::from_pending(a.scene.into_provisional_pending().unwrap()).unwrap();
        let b = CapturedScene::from_parts(after.clone(), "canvas-test".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
        let current =
            NativeDocument::from_pending(b.scene.into_provisional_pending().unwrap()).unwrap();
        let authority = DocumentAuthority::from_native(current.clone(), 4).unwrap();
        let metadata = serde_json::to_value(current.metadata()).unwrap();
        let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
        app.project_id = "canvas-test".into();
        app.doc_snapshot = old.clone().into();
        app.selected = old.metadata().layers().next().unwrap().id().into();
        let initial = Input::new(
            &old,
            "canvas-test",
            old.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            48,
            None,
            false,
            None,
        )
        .unwrap();
        app.native_dispatch
            .endpoint
            .watch(old.key(), &app.preview_epoch);
        assert!(app.native_dispatch.endpoint.bind(old.budget(), old.key()));
        let output = finish_frame(
            &initial,
            &old,
            frame(&runtime, &old, 48),
            None,
            None,
            &app.native_dispatch.endpoint,
            app.native_dispatch.endpoint.generation(),
            &app.native_dispatch.retirement,
            2048,
            None,
            &|| Ok(()),
        )
        .unwrap();
        assert!(
            app.native_dispatch
                .fade_baseline(&old, "canvas-test", None, false)
                .is_none(),
            "A completed worker result is not yet a published baseline"
        );
        assert!(publish(&mut app, &ctx, &old, &initial, Ok(output), 1).unwrap());
        assert!(app
            .native_dispatch
            .fade_baseline(&old, "canvas-test", None, false)
            .is_some());
        app.animation.observe(&app.doc_snapshot, None, 0.);
        app.doc_snapshot = current.clone().into();
        app.animation.observe(
            &app.doc_snapshot,
            Some(
                crate::engine::AiChange {
                    serial: 1,
                    actor: "agent".into(),
                    label: "Paint".into(),
                    tool: "paint".into(),
                    scopes: vec![crate::engine::Scope::layer(&app.selected)],
                    at: 1,
                }
                .into(),
            ),
            1.,
        );
        let _ = ctx.run(
            egui::RawInput {
                time: Some(1.),
                ..Default::default()
            },
            |_| {},
        );
        let input = app
            .capture_native_canvas(&ctx, "canvas-test", &current)
            .unwrap()
            .unwrap();
        assert!(input.blend.is_some() && input.presentation.is_none());
        let output = finish_frame(
            &input,
            &current,
            frame(&runtime, &current, input.stamp.edge),
            None,
            None,
            &app.native_dispatch.endpoint,
            app.native_dispatch.endpoint.generation(),
            &app.native_dispatch.retirement,
            2048,
            None,
            &|| Ok(()),
        )
        .unwrap();
        output.validate(&input.stamp).unwrap();
        assert!(
            output.baseline.is_none(),
            "An intermediate display cannot become the next committed baseline"
        );
        assert!(publish(&mut app, &ctx, &current, &input, Ok(output), 2).unwrap());
        assert!(!app.canvas_capture_ready((&current).into()));
        assert!(app
            .native_dispatch
            .fade_baseline(&old, "canvas-test", None, false)
            .is_some());
        assert!(app
            .native_dispatch
            .fade_baseline(&current, "canvas-test", None, false)
            .is_none());
        for step in 1..12 {
            let time = 1. + f64::from(step) * 0.04;
            let _ = ctx.run(
                egui::RawInput {
                    time: Some(time),
                    ..Default::default()
                },
                |_| {},
            );
            let input = app
                .capture_native_canvas(&ctx, "canvas-test", &current)
                .unwrap()
                .unwrap();
            assert!(input.blend.is_some());
            let cached = app
                .native_dispatch
                .endpoint
                .get(&input.stamp)
                .expect("Reuse the admitted current endpoint without rendering sources again");
            let output = finish_frame(
                &input,
                &current,
                cached.frame(&|| Ok(())).unwrap(),
                None,
                Some(cached),
                &app.native_dispatch.endpoint,
                app.native_dispatch.endpoint.generation(),
                &app.native_dispatch.retirement,
                2048,
                None,
                &|| Ok(()),
            )
            .unwrap();
            output.validate(&input.stamp).unwrap();
            assert!(publish(
                &mut app,
                &ctx,
                &current,
                &input,
                Ok(output),
                2 + step as u64
            )
            .unwrap());
            assert!(!app.canvas_capture_ready((&current).into()));
        }
        let _ = ctx.run(
            egui::RawInput {
                time: Some(1.5),
                ..Default::default()
            },
            |_| {},
        );
        let final_input = app
            .capture_native_canvas(&ctx, "canvas-test", &current)
            .unwrap()
            .unwrap();
        assert!(final_input.blend.is_none() && final_input.stamp.presentation.is_none());
        let output = finish_frame(
            &final_input,
            &current,
            frame(&runtime, &current, final_input.stamp.edge),
            None,
            None,
            &app.native_dispatch.endpoint,
            app.native_dispatch.endpoint.generation(),
            &app.native_dispatch.retirement,
            2048,
            None,
            &|| Ok(()),
        )
        .unwrap();
        assert!(publish(&mut app, &ctx, &current, &final_input, Ok(output), 20).unwrap());
        assert!(app.canvas_capture_ready((&current).into()));
        assert!(app
            .native_dispatch
            .fade_baseline(&current, "canvas-test", None, false)
            .is_some());
        assert_eq!(serde_json::to_value(current.metadata()).unwrap(), metadata);
        assert_eq!(serde_json::to_value(&after).unwrap(), original);
        assert_eq!(authority.history_lengths(), [0, 0]);
        let entry = cache::Entry {
            stamp: final_input.stamp.clone(),
            pixels: app
                .native_dispatch
                .fade_baseline(&current, "canvas-test", None, false)
                .unwrap()
                .1,
        };
        let generation = app.native_dispatch.endpoint.generation();
        app.native_dispatch.endpoint.clear();
        assert!(app
            .native_dispatch
            .endpoint
            .store(generation, entry, &|| Ok(()))
            .is_err());
        assert!(app
            .native_dispatch
            .endpoint
            .get(&final_input.stamp)
            .is_none());
        assert!(
            finish_frame(
                &input,
                &current,
                frame(&runtime, &current, input.stamp.edge),
                None,
                None,
                &app.native_dispatch.endpoint,
                app.native_dispatch.endpoint.generation(),
                &app.native_dispatch.retirement,
                2048,
                None,
                &|| Ok(()),
            )
            .is_err(),
            "A previously captured fade cannot use a retired baseline generation"
        );
        // A failed animation retires optional caches and retries this source once without caching.
        assert!(publish(
            &mut app,
            &ctx,
            &current,
            &input,
            Err("Injected fade failure".into()),
            21
        )
        .is_err());
        assert_eq!(app.native_dispatch.skip_cache, Some(current.key()));
        assert!(app.native_dispatch.expected.is_none());
        assert!(app
            .native_dispatch
            .fade_baseline(&current, "canvas-test", None, false)
            .is_none());
        assert!(app.animation.native_before(&current).is_none());
        let painted = app.native_canvas.current().unwrap().tag().clone();
        assert!(publish(
            &mut app,
            &ctx,
            &current,
            &final_input,
            Err("Still unavailable".into()),
            22
        )
        .is_err());
        assert!(
            app.native_dispatch.expected.is_some(),
            "Repeated errors cannot create an automatic retry loop"
        );
        assert!(painted.same_allocation(app.native_canvas.current().unwrap().tag()));
        assert_eq!(authority.history_lengths(), [0, 0]);
        drop((
            app, ctx, authority, old, current, a.session, a.seals, b.session, b.seals, cache,
            runtime, connection,
        ));
    }
}

#[test]
fn pressure_retires_both_display_caches_and_cancels_only_presentation_work() {
    for depth in [8, 16] {
        let fixture = worker::tests::Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let pool = runtime.resident();
        let document =
            NativeDocument::new_blank(9, 7, depth, &pool, &runtime.disk(), &|| Ok(())).unwrap();
        let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
        let metadata = serde_json::to_value(document.metadata()).unwrap();
        let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
        let input = Input::new(
            &document,
            "canvas-test",
            document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
            48,
            None,
            false,
            None,
        )
        .unwrap();
        app.native_dispatch
            .endpoint
            .watch(document.key(), &app.preview_epoch);
        assert!(app.native_dispatch.endpoint.bind(&pool, document.key()));
        let generation = app.native_dispatch.endpoint.generation();
        let output = finish_frame(
            &input,
            &document,
            frame(&runtime, &document, 48),
            None,
            None,
            &app.native_dispatch.endpoint,
            generation,
            &app.native_dispatch.retirement,
            2048,
            None,
            &|| Ok(()),
        )
        .unwrap();
        assert!(publish(&mut app, &ctx, &document, &input, Ok(output), 1).unwrap());
        let (_, pixels, _) = app
            .native_dispatch
            .fade_baseline(&document, "canvas-test", None, false)
            .unwrap();
        let weak = Arc::downgrade(&pixels);
        app.native_dispatch
            .endpoint
            .store(
                generation,
                cache::Entry {
                    stamp: input.stamp.clone(),
                    pixels,
                },
                &|| Ok(()),
            )
            .unwrap();
        let used = pool.used();
        let request = pool.limit() - used + 1;
        let mandatory = pool.reserve_bytes(request, pool.limit()).unwrap();
        assert!(
            weak.upgrade().is_none(),
            "Both display-only owners must release their pixels"
        );
        assert!(pool.used() < used + request);
        assert!(pool.used() <= pool.limit());
        assert_eq!(app.preview_epoch.load(Ordering::Relaxed), 2);
        assert_ne!(app.native_dispatch.endpoint.generation(), generation);
        assert!(app.native_dispatch.endpoint.get(&input.stamp).is_none());
        assert!(app
            .native_dispatch
            .fade_baseline(&document, "canvas-test", None, false)
            .is_none());
        assert!(app
            .native_dispatch
            .take("canvas-test", &document, 2)
            .unwrap()
            .is_none());
        drop(mandatory);
        assert!(!app.native_dispatch.endpoint.cache_allowed(document.key()));
        assert!(
            !app.native_dispatch.endpoint.bind(&pool, document.key()),
            "No same-source cache rebuild loop"
        );
        let next =
            NativeDocument::new_blank(9, 7, depth, &pool, &runtime.disk(), &|| Ok(())).unwrap();
        assert_ne!(next.key(), document.key());
        assert!(app.native_dispatch.endpoint.bind(&pool, next.key()));
        assert_eq!(serde_json::to_value(document.metadata()).unwrap(), metadata);
        assert_eq!(authority.history_lengths(), [0, 0]);
    }
}

#[test]
fn pressure_preserves_charges_for_pinned_frames_and_rejects_late_endpoints() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let pool = runtime.resident();
    let document = NativeDocument::new_blank(9, 7, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
    let input = Input::new(
        &document,
        "canvas-test",
        document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
        48,
        None,
        false,
        None,
    )
    .unwrap();
    let endpoint = cache::Endpoint::default();
    let epoch = Arc::new(AtomicU64::new(17));
    endpoint.watch(document.key(), &epoch);
    assert!(endpoint.bind(&pool, document.key()));
    let generation = endpoint.generation();
    let frame = frame(&runtime, &document, 48);
    let pixels = Arc::new(Baseline::capture(&frame, &document, "canvas-test", &|| Ok(())).unwrap());
    let weak = Arc::downgrade(&pixels);
    endpoint
        .store(
            generation,
            cache::Entry {
                stamp: input.stamp.clone(),
                pixels: pixels.clone(),
            },
            &|| Ok(()),
        )
        .unwrap();
    let used = pool.used();
    // The controller is evictable; the escaped frame and baseline remain real owners.
    let request = pool.limit() - used + 513;
    assert!(pool.reserve_bytes(request, pool.limit()).is_err());
    assert_eq!(pool.used(), used - 512);
    assert_eq!(epoch.load(Ordering::Relaxed), 18);
    assert!(endpoint
        .store(
            generation,
            cache::Entry {
                stamp: input.stamp.clone(),
                pixels,
            },
            &|| Ok(())
        )
        .is_err());
    assert!(weak.upgrade().is_none());
    let request = pool.limit() - pool.used() + 1;
    assert!(
        pool.reserve_bytes(request, pool.limit()).is_err(),
        "The escaped frame is still charged"
    );
    drop(frame);
    drop(pool.reserve_bytes(request, pool.limit()).unwrap());
    assert_eq!(
        epoch.load(Ordering::Relaxed),
        18,
        "Retired registration cannot cancel again"
    );
}

#[test]
fn an_unpublished_reply_keeps_its_charge_until_retired_and_cannot_repopulate_the_cache() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let pool = runtime.resident();
    let document = NativeDocument::new_blank(9, 7, 16, &pool, &runtime.disk(), &|| Ok(())).unwrap();
    let input = Input::new(
        &document,
        "canvas-test",
        document.copy_commands_metadata(&[], &|| Ok(())).unwrap(),
        48,
        None,
        false,
        None,
    )
    .unwrap();
    let endpoint = cache::Endpoint::default();
    let epoch = Arc::new(AtomicU64::new(1));
    endpoint.watch(document.key(), &epoch);
    assert!(endpoint.bind(&pool, document.key()));
    let frame = frame(&runtime, &document, 48);
    let pixels = Arc::new(Baseline::capture(&frame, &document, "canvas-test", &|| Ok(())).unwrap());
    let weak = Arc::downgrade(&pixels);
    let mut receipt = Receipt {
        job: 1,
        stamp: input.stamp.clone(),
        tag: frame.tag(),
        baseline: Some(pixels),
        text: None,
    };
    drop(frame);
    let used = pool.used();
    let request = pool.limit() - used + 513;
    assert!(pool.reserve_bytes(request, pool.limit()).is_err());
    assert_eq!(
        pool.used(),
        used - 512,
        "Pending reply pixels remain physically retained"
    );
    assert_eq!(epoch.load(Ordering::Relaxed), 2);
    endpoint.published(&mut receipt, &epoch);
    assert!(receipt.baseline.is_none());
    assert!(weak.upgrade().is_none());
    assert!(endpoint
        .fade_baseline(&document, "canvas-test", None, false)
        .is_none());
    drop(pool.reserve_bytes(request, pool.limit()).unwrap());
}
