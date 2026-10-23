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
    assert!(
        !app.updates.windows_prepared.view.busy
            && app.updates.windows_prepared.view.version.is_none()
    );
    assert!(!directory.join("windows-prepared").exists());
    assert_eq!(
        serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap(),
        before
    );
    drop(app);
    fs::remove_dir_all(directory).unwrap();
}
