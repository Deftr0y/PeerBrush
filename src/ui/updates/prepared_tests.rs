use super::*;
use crate::{
    engine::Document,
    source::{Content, Source},
};
use std::{fs, sync::Arc};

fn click(point: Pos2) -> Vec<egui::Event> {
    [true, false]
        .into_iter()
        .map(|pressed| egui::Event::PointerButton {
            pos: point,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        })
        .collect()
}
fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 1000.))),
            events,
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    )
}
fn click_button(app: &mut PeerBrush, ctx: &egui::Context, expected: &str, enabled: bool) {
    // Same current-pass eligibility/stable-ID contract as the App Installer
    // Later fixture. contains_pointer also retains actual disabled responses.
    let mut output = frame(app, ctx, vec![]);
    let mut last = None;
    let (id, at) = (0..16)
        .find_map(|_| {
            let Some(at) = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Text(text) if text.galley.job.text == expected => {
                    Some(text.pos + text.galley.size() * 0.5)
                }
                _ => None,
            }) else {
                last = None;
                output = frame(app, ctx, vec![]);
                return None;
            };
            let mut target = None;
            output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(ctx.screen_rect()),
                    events: vec![egui::Event::PointerMoved(at)],
                    ..Default::default()
                },
                |ctx| {
                    app.updates_tick(ctx);
                    app.updates_dialog(ctx);
                    let previous = last.take();
                    let candidates: Vec<_> = ctx.interaction_snapshot(|state| {
                        state.contains_pointer.iter().copied().collect()
                    });
                    if let Some(response) = candidates
                        .into_iter()
                        .filter_map(|id| ctx.read_response(id))
                        .find(|response| {
                            response.enabled() == enabled
                                && response.sense.senses_click()
                                && !response.sense.senses_drag()
                                && response.rect.contains(at)
                                && response.interact_rect.contains_rect(response.rect)
                                && ctx.screen_rect().contains_rect(response.rect)
                                && response.layer_id.id == egui::Id::new("peerbrush_updates")
                                && ctx.layer_id_at(at) == Some(response.layer_id)
                        })
                    {
                        let key = (response.id, response.rect, response.interact_rect);
                        last = Some(key);
                        target = (previous == Some(key)).then_some((response.id, at));
                    }
                },
            );
            target
        })
        .expect("The actual update button must be a stable, fully visible live response");
    frame(app, ctx, click(at));
    let clicked = ctx.interaction_snapshot(|state| state.clicked);
    if enabled {
        assert_eq!(clicked, Some(id));
    } else {
        assert_ne!(clicked, Some(id));
    }
}
fn history(engine: &crate::engine::Engine) -> serde_json::Value {
    let entries = |items: &[crate::engine::History]| {
        items.iter().map(|entry|json!({
        "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
        "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted
    })).collect::<Vec<_>>()
    };
    json!({"undo":entries(&engine.undo),"redo":entries(&engine.redo)})
}

#[test]
fn actual_preparation_progress_prepared_later_and_late_cancel_preserve_owned_native_projects() {
    for depth in [8, 16] {
        let directory =
            std::env::temp_dir().join(format!("pb-prepared-ui-{}", crate::engine::id()));
        fs::create_dir(&directory).unwrap();
        let (mut app, ctx) = crate::ui::tests::fixture();
        app.recovery.open = false;
        let (raster_id, source_id, source_before) = {
            let mut engine = app.shared.lock().unwrap();
            engine.doc = Document::new_depth(32, 24, depth).unwrap();
            let raster_id = engine.doc.layers[0].id.clone();
            engine.doc.layers[0]
                .pixels
                .set16(3, 4, [12347, 23459, 34567, 45679]);
            let source = Source {
                width: 16,
                height: 16,
                matrix: [1., 0., 0., 1., 0., 0.],
                content: Content::Shape {
                    shape: "rectangle".into(),
                    bounds: [1., 1., 12., 12.],
                    points: vec![],
                    closed: true,
                    fill: [12501, 23003, 34507, 45611],
                    stroke: [0; 4],
                    stroke_width: 0.,
                },
            };
            engine
                .edit(
                    "human",
                    &[json!({"op":"source.add","source":source,"x":8,"y":4})],
                    None,
                    None,
                    "Editable source",
                )
                .unwrap();
            let source_layer = engine
                .doc
                .layers
                .iter()
                .find(|layer| layer.source.is_some())
                .unwrap();
            let source_id = source_layer.id.clone();
            let source_before = serde_json::to_value(source_layer).unwrap();
            fs::write(
                directory.join("owned.psd"),
                crate::psd::encode(&engine.doc).unwrap(),
            )
            .unwrap();
            engine.saved_revision = engine.doc.revision;
            (raster_id, source_id, source_before)
        };
        crate::server::open(&app.shared, &directory.join("owned.psd")).unwrap();
        {
            let mut engine = app.shared.lock().unwrap();
            let raster = engine
                .doc
                .layers
                .iter()
                .find(|layer| layer.id == raster_id)
                .expect("Native PSD must retain the original raster layer ID");
            assert_eq!(raster.kind, "paint");
            assert!(raster.source.is_none());
            assert_eq!(raster.pixels.depth, depth);
            app.selected = raster.id.clone();
            for x in [4, 6] {
                engine.edit("human",&[json!({"op":"paint","layer":app.selected,"points":[[x,4]],"radius":1,"color":[231,73,31,255]})],None,None,"Human stroke").unwrap();
            }
            engine.undo("human").unwrap();
            assert!(!engine.undo.is_empty() && !engine.redo.is_empty());
            let source_layer = engine
                .doc
                .layers
                .iter()
                .find(|layer| layer.id == source_id)
                .expect("Native PSD must retain the editable vector source layer ID");
            assert_eq!(serde_json::to_value(source_layer).unwrap(), source_before);
        }
        let (document, png, path, version, ownership, marker, previous_history) = {
            let engine = app.shared.lock().unwrap();
            assert!(engine.doc.layers.iter().any(|layer| layer.source.is_some()));
            let png = engine.doc.export_png().unwrap();
            assert_eq!(
                image::load_from_memory(&png).unwrap().color(),
                if depth == 16 {
                    image::ColorType::Rgba16
                } else {
                    image::ColorType::Rgba8
                }
            );
            if depth == 16 {
                assert!(engine.doc.layers.iter().any(|layer| layer
                    .pixels
                    .rgba16()
                    .iter()
                    .any(|v| v % 257 != 0)));
            }
            (
                serde_json::to_value(&engine.doc).unwrap(),
                png,
                engine.path.clone(),
                engine.file_version,
                engine.file_ownership.clone().unwrap(),
                engine.saved_revision,
                history(&engine),
            )
        };
        let original = fs::read(directory.join("owned.psd")).unwrap();
        app.updates = Panel::load(&directory);
        app.updates.owner = Owner::WindowsAppInstaller;
        app.updates.open = true;
        app.updates.result = Some(Ok(Report {
            current_version: env!("CARGO_PKG_VERSION").into(),
            channel: Channel::Stable,
            target: updates::Target::WindowsX64,
            release_version: Some("999.0.0".into()),
            release_url: None,
            notes: "Synthetic public GitHub notes, not native target notes".into(),
            assets: vec![],
            checked_at: 1,
        }));
        let release = app.updates.windows_prepared.fake_prepare();
        click_button(&mut app, &ctx, "Prepare signed Windows update", true);
        assert!(app.updates.windows_prepared.view.busy);
        assert!(
            app.update_restart_blocker().is_some(),
            "Dirty native source remains protected"
        );
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.updates.windows_prepared.view.busy && Instant::now() < deadline {
            app.updates.windows_prepared.tick();
            std::thread::yield_now();
        }
        assert!(!app.updates.windows_prepared.view.busy);
        let output = frame(&mut app, &ctx, vec![]);
        crate::ui::workspace_tests::label(&output, "Prepared native package 1.30.1.0");
        crate::ui::workspace_tests::label(&output, "Published version 999.0.0");
        assert_eq!(
            app.updates.windows_prepared.view.version,
            Some([1, 30, 1, 0])
        );
        click_button(&mut app, &ctx, "Later", true);
        assert!(!app.updates.open);
        assert_eq!(
            app.updates.windows_prepared.view.version,
            Some([1, 30, 1, 0])
        );
        app.updates.open = true;
        let release = app.updates.windows_prepared.fake_prepare();
        click_button(&mut app, &ctx, "Prepare signed Windows update", true);
        click_button(&mut app, &ctx, "Later", true);
        assert!(!app.updates.open);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.updates.windows_prepared.view.busy && Instant::now() < deadline {
            app.updates.windows_prepared.tick();
            std::thread::yield_now();
        }
        assert!(app.updates.windows_prepared.view.version.is_none());
        assert!(!app.updates.app_installer_check.pending() && !app.updates.store_check.pending());
        for (case, uncertain, cancel) in [
            ("staged", false, false),
            ("late-cancel", false, true),
            ("uncertain", true, false),
        ] {
            let panel = directory.join(case);
            fs::create_dir(&panel).unwrap();
            app.updates = Panel::load(&panel);
            app.updates.owner = Owner::WindowsAppInstaller;
            app.updates.open = true;
            let prepared = app.updates.windows_prepared.fake_prepare();
            click_button(&mut app, &ctx, "Prepare signed Windows update", true);
            prepared.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.updates.windows_prepared.view.busy && Instant::now() < deadline {
                app.updates.windows_prepared.tick();
                std::thread::yield_now();
            }
            assert!(!app.updates.windows_prepared.view.busy);
            let (release, _done) = app.updates.windows_prepared.fake_staging(uncertain);
            click_button(&mut app, &ctx, "Stage verified Windows package", true);
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.updates.windows_prepared.view.percentage != Some(17)
                && Instant::now() < deadline
            {
                app.updates.windows_prepared.tick();
                std::thread::yield_now();
            }
            assert_eq!(app.updates.windows_prepared.view.percentage, Some(17));
            assert!(app.updates.windows_prepared.view.busy);
            assert!(app.update_restart_blocker().is_some());
            let output = frame(&mut app, &ctx, vec![]);
            crate::ui::workspace_tests::label(&output, "Native staging 17%");
            if cancel {
                click_button(&mut app, &ctx, "Later", true);
            }
            release.send(()).unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.updates.windows_prepared.view.busy && Instant::now() < deadline {
                app.updates.windows_prepared.tick();
                std::thread::yield_now();
            }
            assert!(!app.updates.windows_prepared.view.busy);
            assert_eq!(
                app.updates.windows_prepared.view.staged_version,
                (!cancel && !uncertain).then_some([1, 30, 1, 0])
            );
            assert!(app.updates.windows_prepared.view.version.is_none());
            if !cancel && !uncertain {
                let output = frame(&mut app, &ctx, vec![]);
                crate::ui::workspace_tests::label(&output, "Staged native package 1.30.1.0");
            }
            assert!(!app.exit_approved && app.update_restart_blocker().is_some());
        }
        let engine = app.shared.lock().unwrap();
        assert_eq!(serde_json::to_value(&engine.doc).unwrap(), document);
        assert_eq!(engine.doc.export_png().unwrap(), png);
        assert_eq!(
            (
                engine.path.clone(),
                engine.file_version,
                engine.saved_revision
            ),
            (path, version, marker)
        );
        assert_eq!(history(&engine), previous_history);
        assert!(Arc::ptr_eq(
            engine.file_ownership.as_ref().unwrap(),
            &ownership
        ));
        assert!(!engine.closed && !app.exit_approved);
        assert!(crate::file_ownership::Ownership::open(
            &directory.join("owned.psd"),
            "other project"
        )
        .is_err());
        assert_eq!(fs::read(directory.join("owned.psd")).unwrap(), original);
        drop(engine);
        drop(ownership);
        drop(app);
        fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn actual_default_disabled_control_cannot_create_a_cache_or_claim_the_github_target() {
    let directory =
        std::env::temp_dir().join(format!("pb-disabled-prepared-{}", crate::engine::id()));
    fs::create_dir(&directory).unwrap();
    let (mut app, ctx) = crate::ui::tests::fixture();
    app.recovery.open = false;
    app.updates = Panel::load(&directory);
    app.updates.owner = Owner::WindowsAppInstaller;
    app.updates.open = true;
    let before = serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap();
    click_button(&mut app, &ctx, "Prepare signed Windows update", false);
    click_button(&mut app, &ctx, "Stage verified Windows package", false);
    assert!(
        !app.updates.windows_prepared.view.busy
            && app.updates.windows_prepared.view.version.is_none()
    );
    assert!(!directory.join("windows-prepared").exists());
    for owner in [Owner::MicrosoftStore, Owner::WindowsPackage, Owner::Direct] {
        app.updates.owner = owner;
        let output = frame(&mut app, &ctx, vec![]);
        assert!(
            !output.shapes.iter().any(|shape| matches!(&shape.shape,
            egui::Shape::Text(text) if text.galley.job.text == "Stage verified Windows package")),
            "Direct staging must not appear for Store or other installation owners"
        );
    }
    assert_eq!(
        serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap(),
        before
    );
    drop(app);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn actual_reopened_stage_state_and_human_close_cancel_preserve_joint_native_projects() {
    fn close_cancel(app: &mut PeerBrush, ctx: &egui::Context) {
        app.request_close(crate::ui::lifecycle::Intent::Exit);
        for _ in 0..3 {
            frame(app, ctx, vec![]);
        }
        let at = app.lifecycle.as_ref().unwrap().buttons[2].unwrap().center();
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        assert!(app.lifecycle.is_none() && !app.exit_approved);
    }
    for cancel_live in [false, true] {
        let directory =
            std::env::temp_dir().join(format!("pb-retained-stage-ui-{}", crate::engine::id()));
        fs::create_dir(&directory).unwrap();
        let directory = fs::canonicalize(directory).unwrap();
        let legacy = directory.join("legacy");
        fs::create_dir(&legacy).unwrap();
        let profile = crate::settings::Profile::open(
            crate::settings::Locations {
                config: directory.join("config"),
                data: directory.join("data"),
                cache: directory.join("cache"),
            },
            &legacy,
        )
        .unwrap();
        let state_dir = directory.join("workspace");
        fs::create_dir(&state_dir).unwrap();
        let ctx = egui::Context::default();
        let mut app = PeerBrush::init(
            &ctx,
            Arc::new(std::sync::Mutex::new(crate::engine::Engine::new())),
            Connection {
                port: 0,
                token: String::new(),
                state_dir,
                instance_lock: None,
                availability: Default::default(),
            },
        );
        app.recovery.open = false;
        app.shared.lock().unwrap().doc = Document::new_depth(32, 24, 8).unwrap();
        app.selected = app.shared.lock().unwrap().doc.layers[0].id.clone();
        let second = crate::workspace::new_project(&app.shared, 32, 24, 16, None).unwrap();
        let projects = [app.shared.clone(), second];
        let mut hidden_samples = Vec::new();
        for (index, shared) in projects.iter().enumerate() {
            let mut engine = shared.lock().unwrap();
            let raster = engine.doc.layers[0].id.clone();
            engine.doc.layers[0]
                .pixels
                .set16(25, 20, [12347, 23459, 34567, 0]);
            let hidden = engine.doc.layers[0].pixels.get16(25, 20);
            assert_eq!(hidden[3], 0);
            assert!(hidden[..3].iter().any(|value| *value != 0));
            hidden_samples.push((raster.clone(), hidden));
            let source = Source {
                width: 16,
                height: 16,
                matrix: [1., 0., 0., 1., 0., 0.],
                content: Content::Shape {
                    shape: "rectangle".into(),
                    bounds: [1., 1., 12., 12.],
                    points: vec![],
                    closed: true,
                    fill: [12501, 23003, 34507, 45611],
                    stroke: [0; 4],
                    stroke_width: 0.,
                },
            };
            engine
                .edit(
                    "human",
                    &[json!({"op":"source.add","source":source,"x":8,"y":4})],
                    None,
                    None,
                    "Editable source",
                )
                .unwrap();
            drop(engine);
            crate::server::save(shared, &directory.join(format!("human-{index}.psd"))).unwrap();
            let mut engine = shared.lock().unwrap();
            for x in [4, 6] {
                engine.edit("human", &[json!({"op":"paint","layer":raster,"points":[[x,4]],"radius":1,"color":[231,73,31,255]})],None,None,"Human stroke").unwrap();
            }
            engine.undo("human").unwrap();
            assert!(!engine.undo.is_empty() && !engine.redo.is_empty());
            let paint = engine
                .doc
                .layers
                .iter()
                .find(|layer| layer.id == raster)
                .unwrap();
            assert_eq!(paint.pixels.get16(25, 20), hidden);
            engine
                .reserve(
                    "human",
                    "Keep native document scope",
                    vec![crate::engine::Scope {
                        target: Some(raster),
                        rect: None,
                    }],
                )
                .unwrap();
        }
        let before: Vec<_> = projects
            .iter()
            .map(|shared| {
                let engine = shared.lock().unwrap();
                assert!(engine.doc.layers.iter().any(|layer| layer.source.is_some()));
                assert!(!engine.leases.is_empty());
                if engine.doc.bit_depth == 16 {
                    assert!(engine.doc.layers.iter().any(|layer| layer
                        .pixels
                        .rgba16()
                        .iter()
                        .any(|v| v % 257 != 0)));
                }
                (
                    serde_json::to_value(&engine.doc).unwrap(),
                    engine.doc.export_png().unwrap(),
                    history(&engine),
                    serde_json::to_value(&engine.leases).unwrap(),
                    engine.path.clone(),
                    engine.file_version,
                    engine.saved_revision,
                    engine.file_ownership.clone().unwrap(),
                    fs::read(engine.path.as_ref().unwrap()).unwrap(),
                )
            })
            .collect();
        app.updates = Panel::load(&profile.locations.profile());
        app.updates.owner = Owner::WindowsAppInstaller;
        app.updates.store.preferences.automatic_checks = false;
        app.updates
            .set_update_profile(Some(profile.update_profile()));
        let (release, done) = app.updates.windows_prepared.fake_staging(false);
        app.updates.windows_prepared.start_staging().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.updates.windows_prepared.view.percentage != Some(17) && Instant::now() < deadline
        {
            app.updates.windows_prepared.tick();
            std::thread::yield_now();
        }
        assert_eq!(app.updates.windows_prepared.view.percentage, Some(17));
        assert!(app.update_restart_blocker().is_some());
        if cancel_live {
            close_cancel(&mut app, &ctx);
            assert!(app.updates.windows_prepared.view.busy);
        }
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.updates.windows_prepared.view.busy && Instant::now() < deadline {
            app.updates.windows_prepared.tick();
            std::thread::yield_now();
        }
        assert!(!app.updates.windows_prepared.view.busy);
        let cache = profile.locations.profile().join("windows-prepared");
        let attempt = fs::read_dir(&cache)
            .unwrap()
            .map(|v| v.unwrap().path())
            .find(|p| {
                p.file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("attempt-")
            })
            .unwrap();
        let original_outcome = fs::read(attempt.join("stage-outcome.json")).unwrap();
        // A new actual panel/controller represents process restart. It reads the
        // real prior stage record; only signature/source polling is controlled.
        app.updates = Panel::load(&profile.locations.profile());
        app.updates.owner = Owner::WindowsAppInstaller;
        app.updates.store.preferences.automatic_checks = false;
        app.updates
            .set_update_profile(Some(profile.update_profile()));
        app.updates.open = true;
        let (release, ready, done) = app.updates.windows_prepared.fake_reconciliation();
        frame(&mut app, &ctx, vec![]);
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(app.updates.windows_prepared.view.busy);
        assert!(app.update_restart_blocker().is_some());
        assert!(!app.exit_approved);
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.updates.windows_prepared.view.busy && Instant::now() < deadline {
            app.updates.windows_prepared.tick();
            std::thread::yield_now();
        }
        assert!(!app.updates.windows_prepared.view.busy);
        assert_eq!(
            app.updates.windows_prepared.view.recorded_staging,
            !cancel_live
        );
        assert_eq!(
            app.updates.windows_prepared.view.staged_version,
            (!cancel_live).then_some([1, 30, 1, 0])
        );
        assert_eq!(
            app.updates.windows_prepared.view.installed_version,
            Some([1, 30, 0, 0])
        );
        let (release, ready, done) = app.updates.windows_prepared.fake_reconciliation();
        click_button(&mut app, &ctx, "Recheck retained Windows state", true);
        ready.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(app.updates.windows_prepared.view.busy);
        release.send(()).unwrap();
        done.recv_timeout(Duration::from_secs(5)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.updates.windows_prepared.view.busy && Instant::now() < deadline {
            app.updates.windows_prepared.tick();
            std::thread::yield_now();
        }
        assert!(!app.updates.windows_prepared.view.busy);
        assert_eq!(
            app.updates.windows_prepared.view.recorded_staging,
            !cancel_live
        );
        let output = frame(&mut app, &ctx, vec![]);
        crate::ui::workspace_tests::label(&output, "Current registered native package 1.30.0.0");
        if !cancel_live {
            crate::ui::workspace_tests::label(
                &output,
                "Recorded staging result for native package 1.30.1.0",
            );
        }
        assert!(app.updates.windows_prepared.start().is_err());
        assert!(app.updates.windows_prepared.start_staging().is_err());
        close_cancel(&mut app, &ctx);
        assert!(app.update_restart_blocker().is_some());
        for (shared, expected) in projects.iter().zip(&before) {
            let engine = shared.lock().unwrap();
            assert!(!engine.closed);
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), expected.0);
            assert_eq!(engine.doc.export_png().unwrap(), expected.1);
            assert_eq!(history(&engine), expected.2);
            assert_eq!(serde_json::to_value(&engine.leases).unwrap(), expected.3);
            assert_eq!(
                (
                    engine.path.clone(),
                    engine.file_version,
                    engine.saved_revision
                ),
                (expected.4.clone(), expected.5, expected.6)
            );
            assert!(Arc::ptr_eq(
                engine.file_ownership.as_ref().unwrap(),
                &expected.7
            ));
            assert_eq!(fs::read(engine.path.as_ref().unwrap()).unwrap(), expected.8);
        }
        assert_eq!(
            fs::read(attempt.join("stage-outcome.json")).unwrap(),
            original_outcome
        );
        // Historical stage-only evidence is not an apply hold: ordinary Save,
        // image export, then clean human Exit remain available under real guards.
        for (shared, (raster, hidden)) in projects.iter().zip(&hidden_samples) {
            let path = shared.lock().unwrap().path.clone().unwrap();
            crate::server::save(shared, &path).unwrap();
            let engine = shared.lock().unwrap();
            let reopened = crate::psd::decode(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(reopened.bit_depth, engine.doc.bit_depth);
            let paint = reopened
                .layers
                .iter()
                .find(|layer| &layer.id == raster)
                .unwrap();
            assert_eq!(paint.pixels.depth, engine.doc.bit_depth);
            assert_eq!(paint.pixels.get16(25, 20), *hidden);
            assert_eq!(
                reopened.export_png().unwrap(),
                engine.doc.export_png().unwrap()
            );
            assert!(!engine.doc.export_png().unwrap().is_empty());
        }
        app.updates.open = false;
        app.request_close(crate::ui::lifecycle::Intent::Exit);
        frame(&mut app, &ctx, vec![]);
        assert!(app.exit_approved);
        for shared in &projects {
            assert!(shared.lock().unwrap().closed);
        }
        assert_eq!(
            fs::read(attempt.join("stage-outcome.json")).unwrap(),
            original_outcome
        );
        drop(before);
        drop(projects);
        drop(app);
        drop(profile);
        fs::remove_dir_all(directory).unwrap();
    }
}
