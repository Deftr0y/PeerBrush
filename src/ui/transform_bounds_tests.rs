use super::*;
use crate::{
    engine::{
        native::worker::tests::Fixture,
        scene::{CapturedScene, LegacyTileCache},
        Engine, Layer,
    },
    large_document::{DiskBudget, ResidentBudget},
    raster::Raster,
};
use std::time::{Duration, Instant};

fn wait(query: &mut Query) {
    let end = Instant::now() + Duration::from_secs(5);
    while query.result.is_none() {
        query.poll();
        assert!(Instant::now() < end, "Geometry query did not complete");
        std::thread::sleep(Duration::from_millis(2));
    }
}
fn blank(pool: &ResidentBudget) -> NativeDocument {
    NativeDocument::new_blank(
        32,
        24,
        16,
        pool,
        &DiskBudget::new(1024 * 1024, 4).unwrap(),
        &|| Ok(()),
    )
    .unwrap()
}
fn value() -> Geometry {
    Geometry {
        bounds: [1, 2, 9, 10],
        warp_domains: [Ok([-1., 0., 11., 12.]), Ok([-1., 0., 11., 12.])],
    }
}

#[test]
fn worker_geometry_matches_resident_hidden_descendants_empty_frames_and_native_low_alpha() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) = fixture.runtime();
        let pool = runtime.resident();
        let mut doc = Document::new_depth(269, 19, depth).unwrap();
        let mut folder = Layer::new("Folder", "group", 269, 19);
        folder.pixels = Raster::new_depth(269, 19, depth);
        let layer = &mut doc.layers[0];
        layer.parent = Some(folder.id.clone());
        layer.visible = false;
        layer.x = -5;
        layer.y = 7;
        layer.pixels = Raster::new_depth(257, 3, depth);
        layer.pixels.set16(0, 0, [65535, 0, 0, 0]);
        layer.pixels.set16(
            256,
            2,
            [12347, 23459, 34571, if depth == 16 { 1 } else { 257 }],
        );
        let painted = layer.id.clone();
        let mut empty = Layer::new("Empty", "paint", 2, 3);
        empty.pixels = Raster::new_depth(2, 3, depth);
        empty.parent = Some(folder.id.clone());
        empty.x = -9;
        empty.y = -8;
        doc.layers.push(empty);
        let root = folder.id.clone();
        doc.layers.push(folder);
        doc.selection = Some([1, 1, 4, 4]);
        let original = serde_json::to_value(&doc).unwrap();
        let cache = LegacyTileCache::new(&pool, 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(doc.clone(), "geometry".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let shared = Arc::new(std::sync::Mutex::new(Engine::new()));
        for roots in [
            vec![painted.clone()],
            vec![root.clone()],
            vec![root.clone(), painted.clone()],
        ] {
            let stamp = Stamp::capture(&shared, "geometry", &native, &roots, false, None).unwrap();
            let actual = geometry(&native, &roots, false, None, &|| {
                session.check().map_err(|_| Error::Changed)
            })
            .unwrap();
            assert_eq!(
                actual.bounds,
                crate::transform::tree_bounds(&doc, &roots).unwrap()
            );
            assert_eq!(
                actual.bounds,
                if roots[0] == painted {
                    [251, 9, 252, 10]
                } else {
                    [-9, -8, 252, 10]
                }
            );
            for (whole, actual) in [false, true].into_iter().zip(actual.warp_domains) {
                let commands = roots
                    .iter()
                    .map(|id| json!({"layer":id,"selection_only":!whole}))
                    .collect::<Vec<_>>();
                assert_eq!(actual, crate::warp::domain(&doc, &commands, &|| Ok(())));
            }
            let baseline = pool.used();
            assert!(geometry(&native, &roots, false, None, &|| Err(Error::Cancelled)).is_err());
            assert!(crate::transform::native::tree_bounds(
                &native,
                &roots,
                &mut crate::transform::native::Work::new(1).unwrap(),
                &|| Ok(())
            )
            .is_err());
            assert_eq!(pool.used(), baseline);
            drop(stamp);
        }
        assert_eq!(serde_json::to_value(&doc).unwrap(), original);
        drop((native, staged.seals, session, cache, runtime, connection));
    }
}

#[test]
fn queued_geometry_is_nonblocking_superseded_and_has_no_source_owner_in_its_reply() {
    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let doc = Arc::new(blank(&pool));
    let roots = vec![doc.metadata().layers().next().unwrap().id().to_owned()];
    let shared = Arc::new(std::sync::Mutex::new(Engine::new()));
    let ctx = egui::Context::default();
    let stamp = Stamp::capture(&shared, "geometry", &doc, &roots, false, None).unwrap();
    let mut query = Query::default();
    let (entered, running) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let held = doc.clone();
    let weak = Arc::downgrade(&held);
    let targets = roots.clone();
    query
        .queue(stamp.clone(), &ctx, move |check| {
            entered.send(()).unwrap();
            gate.recv().unwrap();
            geometry(&held, &targets, false, None, check)
        })
        .unwrap();
    running.recv_timeout(Duration::from_secs(5)).unwrap();
    {
        let _lock = shared.lock().unwrap();
        assert!(query
            .get(&shared, "geometry", &doc, &roots, false, None, &ctx)
            .unwrap()
            .is_none());
    }
    query.clear();
    query.queue(stamp, &ctx, |_| Ok(value())).unwrap();
    drop(doc);
    release.send(()).unwrap();
    wait(&mut query);
    assert_eq!(
        query.result.as_ref().unwrap().as_ref().unwrap().bounds,
        value().bounds
    );
    assert!(weak.upgrade().is_none());
    query.stop();
    assert_eq!(pool.used(), 0);
}

#[test]
fn transform_geometry_retries_a_busy_project_without_changing_the_selection() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let root = Arc::new(std::sync::Mutex::new(Engine::new()));
    let workspace = crate::workspace::attach(&root);
    workspace.lock().unwrap().source_runtime = Some(runtime);
    let shared = crate::workspace::native_registration::new_blank(
        &workspace, 32, 24, 16, &Default::default(), None,
    )
    .unwrap();
    let ctx = egui::Context::default();
    let mut query = Query::default();
    let (project, document, roots) = {
        let engine = shared.lock().unwrap();
        let project = engine.project_id.clone();
        let document = engine.native_document().unwrap().clone();
        let roots = vec![document.metadata().layer(0).unwrap().id().to_owned()];
        assert!(query.get(&shared, &project, &document, &roots, false, None, &ctx)
            .unwrap().is_none());
        let deadline = Instant::now() + Duration::from_secs(5);
        while query.receiver.is_some() {
            query.poll();
            assert!(Instant::now() < deadline, "Geometry worker did not observe the lock");
            std::thread::yield_now();
        }
        assert!(query.retry_at.is_some());
        assert!(query.result.is_none());
        (project, document, roots)
    };
    query.retry_at = Some(Instant::now());
    assert!(query.get(&shared, &project, &document, &roots, false, None, &ctx)
        .unwrap().is_none());
    wait(&mut query);
    let geometry = query.get(&shared, &project, &document, &roots, false, None, &ctx)
        .unwrap().unwrap();
    assert_eq!(geometry.bounds, [0, 0, 32, 24]);
    assert!(query.retry_at.is_none());
    let engine = shared.lock().unwrap();
    assert_eq!(engine.native_document().unwrap().key(), document.key());
    assert_eq!(engine.history_lengths(), [0, 0]);
}

#[test]
fn source_identity_and_target_changes_refuse_cached_geometry_and_panics_finish() {
    let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let doc = blank(&pool);
    let roots = vec![doc.metadata().layers().next().unwrap().id().to_owned()];
    let shared = Arc::new(std::sync::Mutex::new(Engine::new()));
    let other = Arc::new(std::sync::Mutex::new(Engine::new()));
    let stamp = Stamp::capture(&shared, "geometry", &doc, &roots, false, None).unwrap();
    assert!(!stamp.matches(&other, "geometry", &doc, &roots, false, None));
    assert!(!stamp.matches(&shared, "other", &doc, &roots, false, None));
    assert!(!stamp.matches(&shared, "geometry", &doc, &[], false, None));
    assert!(!stamp.matches(&shared, "geometry", &doc, &roots, true, None));
    assert!(!stamp.matches(&shared, "geometry", &doc, &roots, false, Some("step")));
    let full = pool
        .reserve_bytes(pool.limit() - pool.used(), pool.limit())
        .unwrap();
    assert!(Stamp::capture(&shared, "geometry", &doc, &roots, false, None).is_err());
    assert!(crate::transform::native::tree_bounds(
        &doc,
        &roots,
        &mut crate::transform::native::Work::new(128_000_000).unwrap(),
        &|| Ok(())
    )
    .is_err());
    drop(full);
    let replacement = doc
        .adopt(
            doc.capture_edit()
                .unwrap()
                .visibility(&roots[0], false, &|| Ok(()))
                .unwrap(),
        )
        .unwrap();
    assert!(!stamp.matches(&shared, "geometry", &replacement, &roots, false, None));
    let ctx = egui::Context::default();
    let mut query = Query::default();
    query
        .queue(stamp.clone(), &ctx, |_| panic!("injected geometry failure"))
        .unwrap();
    wait(&mut query);
    assert!(query
        .result
        .as_ref()
        .unwrap()
        .as_ref()
        .err()
        .unwrap()
        .contains("could not complete"));
    assert!(query.retry_at.is_none());
    query.result = Some(Ok(value()));
    let _lock = shared.lock().unwrap();
    assert!(query
        .get(&shared, "other", &doc, &roots, false, None, &ctx)
        .ok()
        .flatten()
        .is_none());
    drop(_lock);
    query.stop();
    drop((doc, replacement, stamp));
    assert_eq!(pool.used(), 0);
}

#[test]
fn native_free_transform_waits_for_geometry_and_cancels_without_editing() {
    for action in ["ready", "cancel", "escape", "source"] {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
        let doc = blank(&pool);
        let roots = vec![doc.metadata().layers().next().unwrap().id().to_owned()];
        app.selection_layers = roots.iter().cloned().collect();
        app.selected = roots[0].clone();
        let before = serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
        let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
        app.open_free_transform(&ctx, &doc);
        assert!(app
            .free_transform
            .as_ref()
            .unwrap()
            .commands()
            .unwrap_err()
            .contains("Preparing"));
        let stamp =
            Stamp::capture(&app.shared, &app.project_id, &doc, &roots, false, None).unwrap();
        let (release, gate) = mpsc::channel();
        let (entered, running) = mpsc::channel();
        app.transform_bounds
            .queue(stamp, &ctx, move |_| {
                entered.send(()).unwrap();
                gate.recv().unwrap();
                Ok(value())
            })
            .unwrap();
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800., 600.))),
                ..Default::default()
            },
            |ctx| {
                app.free_transform_keys(ctx, &doc);
                app.free_transform_window(ctx, &doc, 0);
            },
        );
        assert!(!output.shapes.is_empty());
        assert!(app.free_transform.as_ref().unwrap().commands().is_err());
        match action {
            "cancel" => app.cancel_free_transform(),
            "escape" => {
                let _ = ctx.run(
                    egui::RawInput {
                        events: vec![egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: Some(egui::Key::Escape),
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        }],
                        ..Default::default()
                    },
                    |ctx| app.free_transform_keys(ctx, &doc),
                );
                assert!(app.free_transform.is_none());
            }
            "source" => {
                let replacement = blank(&pool);
                app.sync_free_transform(&ctx, &replacement);
                assert!(app.free_transform.is_none());
            }
            _ => {}
        }
        release.send(()).unwrap();
        if action == "ready" {
            wait(&mut app.transform_bounds);
            app.sync_free_transform(&ctx, &doc);
            assert!(app
                .free_transform
                .as_ref()
                .unwrap()
                .commands()
                .unwrap()
                .is_empty());
            app.cancel_free_transform();
        }
        app.transform_bounds.stop();
        assert!(app.free_transform.is_none());
        assert!(app.transient.is_empty());
        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
        assert_eq!(
            serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
            before
        );
        drop(doc);
        assert_eq!(pool.used(), 0);
    }
}
