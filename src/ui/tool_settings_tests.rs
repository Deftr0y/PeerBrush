use super::*;
#[test]
fn textured_strokes_keep_native_pixels_across_pan_and_zoom_and_preview_the_shared_result() {
    for depth in [8, 16] {
        for tip in [
            crate::brush::TipKind::Grain,
            crate::brush::TipKind::Chalk,
            crate::brush::TipKind::Dry,
            crate::brush::TipKind::Bristle,
        ] {
            let mut reference = None;
            for (zoom, pan) in [
                (0.5, Vec2::ZERO),
                (1., Vec2::new(21., -17.)),
                (2., Vec2::new(-25., 13.)),
            ] {
                let (mut app, ctx) = super::tests::fixture();
                app.recovery.open = false;
                {
                    let mut e = app.shared.lock().unwrap();
                    e.doc = Document::new_depth(64, 64, depth).unwrap();
                    let l = &mut e.doc.layers[0];
                    l.x = -13;
                    l.y = -11;
                    l.pixels = crate::raster::Raster::new_depth(77, 75, depth);
                    for y in 0..64 {
                        for x in 0..64 {
                            l.pixels.set16(
                                x + 13,
                                y + 11,
                                [10001 + x as u16 * 137, 20003 + y as u16 * 157, 30007, 65535],
                            );
                        }
                    }
                    e.undo.clear();
                }
                frame(&mut app, &ctx, 0., vec![]);
                app.select_tool(Tool::Brush);
                app.brush.tip = tip;
                app.brush.seed = 71;
                app.brush.grain = 2.75;
                app.brush.density = 0.42;
                app.brush.spacing = 0.2;
                app.brush.hardness = 0.6;
                app.radius = 7.;
                app.brush.opacity = 0.7;
                app.brush.flow = 0.8;
                app.zoom = zoom;
                app.pan = pan;
                frame(&mut app, &ctx, 0.1, vec![]);
                frame(&mut app, &ctx, 0.2, vec![]);
                let before = app.shared.lock().unwrap().doc.clone();
                let rect = app.view_rect.unwrap();
                let scale = rect.width() / 64.;
                let start = rect.min + Vec2::new(16., 32.) * scale;
                let end = rect.min + Vec2::new(48., 32.) * scale;
                let button = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                };
                frame(
                    &mut app,
                    &ctx,
                    0.3,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                frame(&mut app, &ctx, 0.4, vec![egui::Event::PointerMoved(end)]);
                assert!(app.points.len() >= 2);
                let commands = vec![app.stroke_command(app.points.clone(), app.color)];
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0]["tip"], tip.name());
                for (actual, expected) in commands[0]["points"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .zip([[16., 32.], [48., 32.]])
                {
                    for axis in 0..2 {
                        assert!((actual[axis].as_f64().unwrap() - expected[axis]).abs() < 0.0001);
                    }
                }
                let preview =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                let mut time = 0.5;
                loop {
                    frame(&mut app, &ctx, time, vec![]);
                    time += 0.05;
                    if app.canvas_pixels.as_ref().is_some_and(|(w, h, p)| {
                        preview.preview(None, (*w).max(*h), None, false).unwrap().2 == *p
                    }) {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "Textured preview missing"
                    );
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(app.shared.lock().unwrap().undo.is_empty());
                frame(&mut app, &ctx, time, vec![button(end, false)]);
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1, "{}", app.message);
                assert_eq!(e.doc.bit_depth, depth);
                let actual = e.doc.export_png().unwrap();
                assert_eq!(actual, preview.export_png().unwrap());
                if let Some(expected) = &reference {
                    assert_eq!(&actual, expected, "{depth}, {}, zoom {zoom}", tip.name());
                } else {
                    reference = Some(actual);
                }
                assert_eq!(
                    e.doc.layers[0].pixels.get16(17, 15),
                    before.layers[0].pixels.get16(17, 15)
                );
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
}
fn frame(
    app: &mut PeerBrush,
    ctx: &egui::Context,
    time: f64,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            time: Some(time),
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
            events,
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    )
}
#[test]
fn switching_all_brush_tools_restores_complete_profiles_and_ui_commands_match_engine_profiles_at_native_depth(
) {
    for depth in [8, 16] {
        let (mut app, ctx) = super::tests::fixture();
        app.recovery.open = false;
        {
            let mut e = app.shared.lock().unwrap();
            e.doc = Document::new_depth(64, 64, depth).unwrap();
            for y in 0..64 {
                for x in 0..64 {
                    e.doc.layers[0].pixels.set16(
                        x,
                        y,
                        [12345 + x as u16 * 53, 23457 + y as u16 * 61, 45679, 65535],
                    );
                }
            }
        }
        frame(&mut app, &ctx, 0., vec![]);
        let tools = [
            Tool::Brush,
            Tool::Eraser,
            Tool::Smudge,
            Tool::Clone,
            Tool::Heal,
        ];
        let mut profiles = vec![];
        for (index, tool) in tools.into_iter().enumerate() {
            app.select_tool(tool);
            app.radius = 3. + index as f32;
            app.brush.opacity = 0.3 + index as f32 * 0.1;
            app.brush.flow = 0.8;
            app.brush.hardness = 0.6;
            app.brush.roundness = 0.8;
            app.brush.angle = index as f32 * 15.;
            app.brush.tip = crate::brush::TipKind::Grain;
            app.brush.seed = 37 + index as u32;
            app.brush.pressure_gamma = 1.2;
            app.brush.taper_end = 2.;
            frame(&mut app, &ctx, 0.1 + index as f64 * 0.1, vec![]);
            profiles.push(
                app.tool_settings_store
                    .lock()
                    .unwrap()
                    .get(tool_settings::key(tool).unwrap())
                    .unwrap()
                    .profile,
            );
        }
        for (index, tool) in tools.into_iter().enumerate() {
            app.select_tool(tool);
            assert_eq!(app.brush, profiles[index].settings);
            assert_eq!(app.radius, profiles[index].settings.radius);
            frame(&mut app, &ctx, 1. + index as f64 * 0.1, vec![]);
            let before = app.shared.lock().unwrap().doc.clone();
            app.retouch_source = Some(retouch::Anchor {
                project: before.id.clone(),
                layer: app.selected.clone(),
                mask: false,
                point: [8., 10.],
                destination: None,
            });
            app.retouch_revision = Some(before.revision);
            let points = vec![[20., 30.], [38., 35.]];
            let command = app.stroke_command(points, [233, 84, 32, 255]);
            assert_eq!(
                crate::brush::Settings::from_command(&command).unwrap(),
                profiles[index].settings
            );
            let mut oracle = crate::engine::Engine::new();
            oracle.doc = before.clone();
            let mut expected = serde_json::to_value(profiles[index].settings).unwrap();
            for (key, value) in command.as_object().unwrap() {
                if !expected.as_object().unwrap().contains_key(key) {
                    expected[key] = value.clone();
                }
            }
            oracle
                .edit(
                    "human",
                    &[expected],
                    None,
                    None,
                    "Explicit protocol profile",
                )
                .unwrap();
            let mut e = app.shared.lock().unwrap();
            e.edit("human", &[command], None, None, "Native tool stroke")
                .unwrap();
            assert_eq!(
                e.doc.export_png().unwrap(),
                oracle.doc.export_png().unwrap()
            );
            assert_ne!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            assert_eq!(e.undo.len(), 1);
            let saved = crate::psd::decode(&crate::psd::encode(&e.doc).unwrap()).unwrap();
            assert_eq!(saved.bit_depth, depth);
            assert_eq!(
                saved.layers[0].pixels.rgba16(),
                e.doc.layers[0].pixels.rgba16()
            );
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        }
        app.select_tool(Tool::Liquify);
        app.radius = 39.;
        app.liquify.strength = 0.7;
        app.liquify.mode = liquify::Mode::Pinch;
        app.select_tool(Tool::Brush);
        assert_eq!(app.brush, profiles[0].settings);
        app.activate_liquify(None);
        assert_eq!(app.radius, 39.);
        assert_eq!(app.liquify.strength, 0.7);
        assert!(app.liquify.mode == liquify::Mode::Pinch);
    }
}

#[test]
fn presets_are_snapshots_per_tool_including_preset_induced_tool_switches() {
    let (mut app, ctx) = super::tests::fixture();
    app.recovery.open = false;
    let presets = crate::brush_library::curated();
    let ink = presets.iter().find(|p| p.id == "brush-pen").unwrap();
    let blend = presets.iter().find(|p| p.category == "Blend").unwrap();
    app.select_brush_preset(ink);
    app.brush.opacity = 0.37;
    app.radius = 19.25;
    app.select_brush_preset(blend);
    assert!(app.tool == Tool::Smudge);
    app.brush.flow = 0.42;
    let smudge = app.brush;
    app.select_tool(Tool::Brush);
    assert_eq!(app.brush_preset.as_deref(), Some(ink.id.as_str()));
    assert_eq!(app.brush.opacity, 0.37);
    assert_eq!(app.radius, 19.25);
    app.select_tool(Tool::Smudge);
    assert_eq!(app.brush, smudge);
    assert_eq!(app.brush_preset.as_deref(), Some(blend.id.as_str()));
    app.select_tool(Tool::Eraser);
    assert_eq!(app.brush, crate::brush::Settings::default());
    frame(&mut app, &ctx, 0., vec![]);
    assert!(app.shared.lock().unwrap().undo.is_empty());
}

#[test]
fn agent_preferences_commit_immediately_animate_only_controls_and_human_input_interrupts_without_history(
) {
    let (mut app, ctx) = super::tests::fixture();
    app.recovery.open = false;
    frame(&mut app, &ctx, 0., vec![]);
    let before = crate::psd::encode(&app.shared.lock().unwrap().doc).unwrap();
    server::dispatch(&app.shared,"tool_settings",&json!({"action":"set","tool":"brush","actor":"artist","expected_tool_revision":0,"settings":{"radius":31,"opacity":0.4,"flow":0.25}})).unwrap();
    assert_eq!(
        app.tool_settings_store
            .lock()
            .unwrap()
            .get("brush")
            .unwrap()
            .profile
            .settings
            .radius,
        31.
    );
    let output = frame(&mut app, &ctx, 0.1, vec![]);
    assert_eq!(app.radius, 31.);
    assert_eq!(app.brush.opacity, 0.4);
    assert_eq!(app.tool_display_number("radius", app.radius, 0.1), 18.);
    let halfway = app.tool_display_number("radius", app.radius, 0.28);
    assert!(halfway > 18. && halfway < 31.);
    assert!(output
        .shapes
        .iter()
        .any(|s| matches!(&s.shape,egui::Shape::Rect(r) if r.fill==AI_BLUE && r.rect.top()<140.)));
    app.show_brush = true;
    frame(&mut app, &ctx, 0.46, vec![]);
    assert_eq!(app.brush.opacity, 0.4);
    assert_eq!(app.radius, 31.);
    frame(
        &mut app,
        &ctx,
        0.5,
        vec![egui::Event::Key {
            key: egui::Key::ArrowRight,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }],
    );
    assert!(!app.tool_preferences_ai(0.5));
    assert_eq!(
        crate::psd::encode(&app.shared.lock().unwrap().doc).unwrap(),
        before
    );
    assert!(app.shared.lock().unwrap().undo.is_empty());
    let current = app
        .tool_settings_store
        .lock()
        .unwrap()
        .get("brush")
        .unwrap();
    server::dispatch(&app.shared,"tool_settings",&json!({"action":"set","tool":"brush","actor":"artist","expected_tool_revision":current.revision,"settings":{"radius":41,"flow":0.6}})).unwrap();
    app.brush.opacity = 0.7;
    frame(&mut app, &ctx, 0.6, vec![]);
    assert_eq!(app.radius, 41.);
    assert_eq!(app.brush.flow, 0.6);
    assert_eq!(app.brush.opacity, 0.7);
}

#[test]
fn update_profile_flush_merges_deferred_human_controls_with_newer_agent_settings_without_document_edits(
) {
    for depth in [8, 16] {
        let directory =
            std::env::temp_dir().join(format!("peerbrush-profile-flush-{}", crate::engine::id()));
        std::fs::create_dir(&directory).unwrap();
        let (mut app, _) = super::tests::fixture();
        *app.tool_settings_store.lock().unwrap() =
            crate::tool_settings::Store::load(directory.join("tool-settings.json"));
        app.restore_tool_profile(Tool::Brush);
        {
            let mut e = app.shared.lock().unwrap();
            e.doc = Document::new_depth(4, 3, depth).unwrap();
            let layer = e.doc.layers[0].id.clone();
            e.doc.layers[0].pixels.set16(0, 0, [1001, 3003, 5007, 0]);
            e.edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Undo retained"})],
                None,
                None,
                "Undo retained",
            )
            .unwrap();
            e.edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Redo retained"})],
                None,
                None,
                "Redo retained",
            )
            .unwrap();
            e.undo("human").unwrap();
        }
        let before = serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap();
        let brush = app
            .tool_settings_store
            .lock()
            .unwrap()
            .get("brush")
            .unwrap();
        server::dispatch(&app.shared, "tool_settings", &json!({"action":"set","tool":"brush","actor":"artist","expected_tool_revision":brush.revision,"settings":{"radius":43,"flow":0.29}})).unwrap();
        // UI still displays its older cached snapshot. Only opacity changed by
        // the human, so merging cannot replace the newer agent radius or flow.
        app.brush.opacity = 0.61;
        app.flush_update_tool_profile().unwrap();
        let current = app
            .tool_settings_store
            .lock()
            .unwrap()
            .get("brush")
            .unwrap();
        assert_eq!(current.profile.settings.radius, 43.);
        assert_eq!(current.profile.settings.flow, 0.29);
        assert_eq!(current.profile.settings.opacity, 0.61);
        app.tool_settings_store
            .lock()
            .unwrap()
            .authenticate_update_profile(&directory)
            .unwrap();
        let disk = crate::tool_settings::Store::load(directory.join("tool-settings.json"));
        assert_eq!(disk.get("brush").unwrap().profile, current.profile);
        let e = app.shared.lock().unwrap();
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert_eq!((e.undo.len(), e.redo.len()), (1, 1));
        assert!(!e.closed);
        drop(e);
        std::fs::remove_dir_all(directory).unwrap();
    }
}

#[test]
fn update_profile_flush_reports_protected_disk_bytes_and_pending_values_without_closing_unsaved_work(
) {
    let directory = std::env::temp_dir().join(format!(
        "peerbrush-profile-flush-error-{}",
        crate::engine::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let file = directory.join("tool-settings.json");
    let (mut app, _) = super::tests::fixture();
    *app.tool_settings_store.lock().unwrap() = crate::tool_settings::Store::load(file.clone());
    app.restore_tool_profile(Tool::Brush);
    app.radius = 43.;
    app.flush_update_tool_profile().unwrap();
    let mut changed: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    changed["tools"]["brush"]["settings"]["radius"] = json!(44.);
    let external = serde_json::to_vec_pretty(&changed).unwrap();
    std::fs::write(&file, &external).unwrap();
    app.brush.opacity = 0.63;
    app.shared.lock().unwrap().doc.revision += 1;
    let before = serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap();
    assert!(app.flush_update_tool_profile().is_err());
    assert!(app
        .tool_settings_store
        .lock()
        .unwrap()
        .update_checkpoint_ready()
        .is_err());
    assert_eq!(app.brush.opacity, 0.63);
    assert_eq!(std::fs::read(&file).unwrap(), external);
    assert_eq!(
        serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap(),
        before
    );
    assert!(!app.shared.lock().unwrap().closed);
    assert!(!app.workspace.lock().unwrap().exiting);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn ordinary_on_exit_still_flushes_its_last_ui_delta_and_preserves_document_sources() {
    let directory = std::env::temp_dir().join(format!(
        "peerbrush-profile-normal-exit-{}",
        crate::engine::id()
    ));
    std::fs::create_dir(&directory).unwrap();
    let (mut app, _) = super::tests::fixture();
    app.connection.state_dir = directory.clone();
    *app.tool_settings_store.lock().unwrap() =
        crate::tool_settings::Store::load(directory.join("tool-settings.json"));
    app.restore_tool_profile(Tool::Brush);
    app.radius = 47.;
    app.brush.opacity = 0.67;
    let before = serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap();
    eframe::App::on_exit(&mut app);
    let disk = crate::tool_settings::Store::load(directory.join("tool-settings.json"));
    assert!(disk.error.is_none());
    let current = disk.get("brush").unwrap().profile;
    assert_eq!(current.settings.radius, 47.);
    assert_eq!(current.settings.opacity, 0.67);
    assert_eq!(
        serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap(),
        before
    );
    assert!(!app.shared.lock().unwrap().closed);
    std::fs::remove_dir_all(directory).unwrap();
}

fn update_exit_fixture() -> (
    PeerBrush,
    egui::Context,
    Vec<Shared>,
    crate::settings::Profile,
    PathBuf,
) {
    use crate::source::{Content, Source};
    let temporary =
        std::env::temp_dir().join(format!("peerbrush-final-writers-{}", crate::engine::id()));
    std::fs::create_dir(&temporary).unwrap();
    let directory = temporary.canonicalize().unwrap();
    let legacy = directory.join("legacy");
    std::fs::create_dir(&legacy).unwrap();
    let profile = crate::settings::Profile::open(
        crate::settings::Locations {
            config: directory.join("config"),
            data: directory.join("data"),
            cache: directory.join("cache"),
        },
        &legacy,
    )
    .unwrap();
    let (mut app, ctx) = super::tests::fixture();
    app.recovery.open = false;
    app.connection.state_dir = directory.join("runtime");
    std::fs::create_dir(&app.connection.state_dir).unwrap();
    let preferences = profile.locations.profile();
    *app.tool_settings_store.lock().unwrap() =
        crate::tool_settings::Store::load(preferences.join("tool-settings.json"));
    app.restore_tool_profile(Tool::Brush);
    app.updates = updates::Panel::load(&preferences);
    app.updates
        .set_update_profile(Some(profile.update_profile()));
    app.shared.lock().unwrap().doc = Document::new_depth(32, 24, 8).unwrap();
    app.selected = app.shared.lock().unwrap().doc.layers[0].id.clone();
    let second = crate::workspace::new_project(&app.shared, 32, 24, 16, None).unwrap();
    let projects = vec![app.shared.clone(), second];
    crate::workspace::select_in(&app.workspace, &app.project_id).unwrap();
    for (index, shared) in projects.iter().enumerate() {
        let mut engine = shared.lock().unwrap();
        let raster = engine.doc.layers[0].id.clone();
        engine.doc.layers[0]
            .pixels
            .set16(25, 20, [12347, 23459, 34567, 0]);
        let hidden = engine.doc.layers[0].pixels.get16(25, 20);
        assert_eq!(hidden[3], 0);
        assert!(hidden[..3].iter().any(|value| *value != 0));
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
                "Keep editable source",
            )
            .unwrap();
        for x in [4, 6] {
            engine.edit("human", &[json!({"op":"paint","layer":raster,"points":[[x,4]],"radius":1,"color":[231,73,31,255]})],None,None,"Keep native history").unwrap();
        }
        engine.undo("human").unwrap();
        engine
            .reserve(
                "human",
                "Keep project ownership",
                vec![crate::engine::Scope::layer(&raster)],
            )
            .unwrap();
        assert!(!engine.undo.is_empty() && !engine.redo.is_empty());
        assert_eq!(
            engine
                .doc
                .layers
                .iter()
                .find(|layer| layer.id == raster)
                .unwrap()
                .pixels
                .get16(25, 20),
            hidden
        );
        drop(engine);
        update_exit_save(shared, &directory.join(format!("art-{index}.psd")));
    }
    (app, ctx, projects, profile, directory)
}

fn update_exit_save(shared: &Shared, path: &Path) {
    let (project, document, revision) = {
        let engine = shared.lock().unwrap();
        (
            engine.project_id.clone(),
            engine.doc.id.clone(),
            engine.doc.revision,
        )
    };
    let report = server::dispatch(
        shared,
        "document",
        &json!({"action":"save_report","project_id":project,
        "document_id":document,"expected_revision":revision}),
    )
    .unwrap();
    assert_eq!(report["document_id"], document);
    assert_eq!(report["revision"], revision);
    assert_eq!(report["save_allowed"], true);
    assert!(report["losses"]
        .as_array()
        .unwrap()
        .iter()
        .any(|loss| loss["code"] == "editable_source_rasterized"));
    server::save_accepted_source(shared, path, &document, revision, false).unwrap();
}

fn update_exit_documents(projects: &[Shared]) -> Vec<Value> {
    projects.iter().map(|shared| {
        let engine=shared.lock().unwrap();
        let history = |entries: &[crate::engine::History]| entries.iter().map(|entry|json!({
            "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,"task":entry.task,
            "scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted})).collect::<Vec<_>>();
        assert!(!engine.undo.is_empty() && !engine.redo.is_empty() && !engine.leases.is_empty());
        assert!(engine.doc.layers.iter().any(|layer|layer.source.is_some()));
        let raster=engine.doc.layers.iter().find(|layer|layer.source.is_none()).unwrap();
        assert_eq!(raster.pixels.depth,engine.doc.bit_depth);
        json!({"document":engine.doc,"png":engine.doc.export_png().unwrap(),"hidden":raster.pixels.get16(25,20),
            "undo":history(&engine.undo),"redo":history(&engine.redo),"leases":engine.leases,
            "saved_revision":engine.saved_revision,"path":engine.path})
    }).collect()
}

#[test]
fn final_update_profile_transaction_blocks_same_future_frame_on_exit_and_late_mcp_writers() {
    let (mut app, ctx, projects, profile, directory) = update_exit_fixture();
    let preferences = profile.locations.profile();
    let tool_file = preferences.join("tool-settings.json");
    let reviewed = crate::workspace::lifecycle::update_readiness(&app.workspace).unwrap();
    let expected = update_exit_documents(&projects);
    let files: Vec<_> = projects
        .iter()
        .map(|shared| {
            let engine = shared.lock().unwrap();
            let path = engine.path.clone().unwrap();
            let bytes = std::fs::read(&path).unwrap();
            (path, bytes, engine.file_version)
        })
        .collect();
    // Deferred human UI settings and a newer independent MCP delta both belong
    // to the final bytes, before the exact shared transaction takes its locks.
    let brush = app
        .tool_settings_store
        .lock()
        .unwrap()
        .get("brush")
        .unwrap();
    server::dispatch(
        &app.shared,
        "tool_settings",
        &json!({"action":"set","tool":"brush","actor":"artist",
        "expected_tool_revision":brush.revision,"settings":{"radius":43,"flow":0.29}}),
    )
    .unwrap();
    app.brush.opacity = 0.61;
    let tools = app.tool_settings_store.clone();
    let workspace = app.workspace.clone();
    let published = std::cell::Cell::new(false);
    app.finish_update_exit(&reviewed, |store| {
        assert!(projects.iter().all(|shared| shared.try_lock().is_err()));
        assert!(workspace.try_lock().is_err() && tools.try_lock().is_err());
        profile.update_profile().validate()?;
        store.authenticate_update_profile(&preferences)?;
        let current = store.get("brush")?;
        assert_eq!(current.profile.settings.radius, 43.);
        assert_eq!(current.profile.settings.flow, 0.29);
        assert_eq!(current.profile.settings.opacity, 0.61);
        assert!(!store.update_exit_committed());
        published.set(true);
        Ok(())
    })
    .unwrap();
    assert!(published.get() && app.update_exit_committed() && app.exit_approved);
    assert!(app.workspace.lock().unwrap().exiting);
    assert_eq!(update_exit_documents(&projects), expected);
    let persisted = std::fs::read(&tool_file).unwrap();
    let preference_manifest = std::fs::read(preferences.join("profile.json")).unwrap();
    for (shared, (path, bytes, version)) in projects.iter().zip(&files) {
        let engine = shared.lock().unwrap();
        assert!(engine.closed && engine.file_ownership.is_none());
        assert_eq!(engine.file_version, *version);
        assert_eq!(std::fs::read(path).unwrap(), *bytes);
        assert!(crate::file_ownership::local_project(path).is_none());
    }
    for (method, parameters) in [
        ("tool_settings", json!({"action":"set"})),
        ("brushes", json!({"action":"save"})),
        ("brushes", json!({"action":"delete"})),
        ("filters", json!({"action":"save"})),
        ("filters", json!({"action":"rename"})),
        ("filters", json!({"action":"delete"})),
        ("filters", json!({"action":"import"})),
        ("exports", json!({"action":"save_preset"})),
        ("exports", json!({"action":"delete_preset"})),
    ] {
        assert!(server::dispatch(&app.shared, method, &parameters)
            .unwrap_err()
            .contains("Workspace is exiting"));
    }
    assert!(server::FileOperation::begin_workspace(&app.workspace).is_err());
    assert!(server::FileOperation::begin(&app.shared).is_err());
    let recovery = directory.join("late-recovery");
    std::fs::create_dir(&recovery).unwrap();
    crate::recovery::checkpoint(&app.workspace, &recovery, &mut Default::default());
    assert_eq!(std::fs::read_dir(&recovery).unwrap().count(), 0);
    {
        let mut store = tools.lock().unwrap();
        let before = store.get("brush").unwrap().profile;
        let mut after = before.clone();
        after.settings.radius = 97.;
        assert!(store.human_delta("brush", &before, &after).is_err());
        assert!(store.set("brush", after, "human", None).is_err());
        assert!(store.flush().is_err() && store.update_checkpoint_ready().is_err());
    }
    app.radius = 97.;
    app.brush.opacity = 0.17;
    app.store_current_tool_profile();
    app.sync_tool_profile(&ctx);
    app.exit_tool_profile();
    // These are actual full egui frames after the same committed close. No
    // preference dialog, native update tick, shortcut or second sync may run.
    for time in [0.1, 0.2] {
        let output = frame(
            &mut app,
            &ctx,
            time,
            vec![egui::Event::Key {
                key: egui::Key::Comma,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
        );
        assert!(output.shapes.is_empty());
    }
    eframe::App::on_exit(&mut app);
    assert_eq!(std::fs::read(&tool_file).unwrap(), persisted);
    assert_eq!(
        std::fs::read(preferences.join("profile.json")).unwrap(),
        preference_manifest
    );
    assert_eq!(update_exit_documents(&projects), expected);
    drop(app);
    drop(projects);
    drop(profile);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_final_update_transaction_keeps_newer_work_cancel_and_ordinary_on_exit_writable() {
    let (mut app, ctx, projects, profile, directory) = update_exit_fixture();
    let reviewed = crate::workspace::lifecycle::update_readiness(&app.workspace).unwrap();
    let layer = projects[1].lock().unwrap().doc.layers[0].id.clone();
    projects[1]
        .lock()
        .unwrap()
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":layer,"name":"Newer human work"})],
            None,
            None,
            "Keep newer work",
        )
        .unwrap();
    let newer = serde_json::to_value(&projects[1].lock().unwrap().doc).unwrap();
    let invoked = std::cell::Cell::new(false);
    assert!(app
        .finish_update_exit(&reviewed, |_| {
            invoked.set(true);
            Ok(())
        })
        .is_err());
    assert!(!invoked.get() && !app.update_exit_committed() && !app.exit_approved);
    assert!(!app.workspace.lock().unwrap().exiting);
    assert!(projects.iter().all(|shared| !shared.lock().unwrap().closed));
    assert_eq!(
        serde_json::to_value(&projects[1].lock().unwrap().doc).unwrap(),
        newer
    );
    // The real Don't Save review is still cancelable; no update-close receipt
    // can be manufactured from an old read or from merely requesting the exit.
    app.request_close(lifecycle::Intent::Exit);
    for time in [0.1, 0.2, 0.3] {
        frame(&mut app, &ctx, time, vec![]);
    }
    let at = app.lifecycle.as_ref().unwrap().buttons[2].unwrap().center();
    for (time, pressed) in [(0.4, true), (0.5, false)] {
        frame(
            &mut app,
            &ctx,
            time,
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
    assert_eq!(
        serde_json::to_value(&projects[1].lock().unwrap().doc).unwrap(),
        newer
    );
    let path = projects[1].lock().unwrap().path.clone().unwrap();
    update_exit_save(&projects[1], &path);
    assert!(app
        .finish_update_exit(&reviewed, |_| {
            invoked.set(true);
            Ok(())
        })
        .is_err());
    assert!(!invoked.get());
    let current = crate::workspace::lifecycle::update_readiness(&app.workspace).unwrap();
    let operation = server::FileOperation::begin_workspace(&app.workspace).unwrap();
    assert!(app
        .finish_update_exit(&current, |_| {
            invoked.set(true);
            Ok(())
        })
        .is_err());
    assert!(!invoked.get());
    drop(operation);
    app.radius = 51.;
    assert_eq!(
        app.finish_update_exit(&current, |_| Err("Final preservation failed".into()))
            .unwrap_err(),
        "Final preservation failed"
    );
    assert!(!app.update_exit_committed() && !app.workspace.lock().unwrap().exiting);
    assert!(projects.iter().all(|shared| {
        let engine = shared.lock().unwrap();
        !engine.closed && engine.file_ownership.is_some()
    }));
    app.radius = 59.;
    eframe::App::on_exit(&mut app);
    let disk =
        crate::tool_settings::Store::load(profile.locations.profile().join("tool-settings.json"));
    assert_eq!(disk.get("brush").unwrap().profile.settings.radius, 59.);
    assert_eq!(
        serde_json::to_value(&projects[1].lock().unwrap().doc).unwrap(),
        newer
    );
    drop(app);
    drop(projects);
    drop(profile);
    std::fs::remove_dir_all(directory).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn unknown_native_owner_cannot_use_the_dormant_preference_close_foundation() {
    let (mut app, _, projects, profile, directory) = update_exit_fixture();
    let reviewed = crate::workspace::lifecycle::update_readiness(&app.workspace).unwrap();
    let expected = update_exit_documents(&projects);
    app.native_check_starting_for_test();
    let invoked = std::cell::Cell::new(false);
    assert!(app
        .finish_update_exit(&reviewed, |_| {
            invoked.set(true);
            Ok(())
        })
        .unwrap_err()
        .contains("native update owner remains held"));
    assert!(!invoked.get() && !app.update_exit_committed() && !app.exit_approved);
    assert!(!app.workspace.lock().unwrap().exiting);
    assert!(projects.iter().all(|shared| !shared.lock().unwrap().closed));
    assert_eq!(update_exit_documents(&projects), expected);
    drop(app);
    drop(projects);
    drop(profile);
    std::fs::remove_dir_all(directory).unwrap();
}
