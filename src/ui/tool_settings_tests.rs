use super::*;
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
