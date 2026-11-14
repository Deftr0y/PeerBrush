use super::*;

#[test]
fn viewing_each_levels_channel_preserves_source_settings_and_history_intent() {
    for original in [json!({"gamma":1.25}), json!({"input_mapping":"exact","channels":[
        {"black":17./255.,"white":19./255.,"gamma":9.99},{},{}]})] {
        let ctx = egui::Context::default();
        let mut value = original.clone();
        let mut frame = |events| {
            let mut selected = None;
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(174., 350.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        let id = ui.id().with(("levels", "channel"));
                        assert!(settings(ui, "levels", &mut value).is_none());
                        selected = ctx.data(|data| data.get_temp::<usize>(id));
                    });
                },
            );
            (output, selected)
        };
        let (mut output, selected) = frame(vec![]);
        assert_eq!(selected, Some(0));
        for (index, color) in controls::RGB_COLORS.into_iter().enumerate() {
            let pos = output.shapes.iter().find_map(|shape| match &shape.shape {
                egui::Shape::Rect(rect) if rect.fill == color && rect.rect.width() > 12. => {
                    Some(rect.rect.center())
                }
                _ => None,
            }).expect("Levels channel swatch");
            let button = |pressed| egui::Event::PointerButton {
                pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
            };
            let _ = frame(vec![egui::Event::PointerMoved(pos), button(true)]);
            let (next, selected) = frame(vec![button(false)]);
            assert_eq!(selected, Some(index + 1));
            output = next;
        }
        assert_eq!(value, original);
    }
}

fn draw(ctx: &egui::Context, settings: &mut Value, events: Vec<egui::Event>) -> Pos2 {
    let mut origin = Pos2::ZERO;
    let _ = ctx.run(egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 100.))),
        events,
        ..Default::default()
    }, |ctx| {
        egui::CentralPanel::default().show(ctx, |ui| {
            origin = ui.cursor().min;
            levels_slider(ui, settings, false);
        });
    });
    origin
}

#[test]
fn gamma_handle_preserves_unedited_native_input_endpoints() {
    let ctx = egui::Context::default();
    let mut settings = json!({"input_mapping":"exact","black":17./255.,"white":19./255.,
        "gamma":1.2,"output_black":0.13,"output_white":0.87});
    let original = settings.clone();
    let origin = draw(&ctx, &mut settings, vec![]);
    let position = 17. / 255. + (2. / 255.) * 0.5_f32.powf(1.2);
    let start = origin + Vec2::new(position * 240., 27.);
    let end = start - Vec2::new(16., 0.);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
    };
    draw(&ctx, &mut settings, vec![egui::Event::PointerMoved(start), button(start, true)]);
    draw(&ctx, &mut settings, vec![egui::Event::PointerMoved(end)]);
    draw(&ctx, &mut settings, vec![button(end, false)]);
    assert!(settings["gamma"].as_f64().unwrap() > 9.9);
    settings["gamma"] = original["gamma"].clone();
    assert_eq!(settings, original);
}
