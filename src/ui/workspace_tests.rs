use super::*;

pub(super) fn draw(
    app: &mut PeerBrush,
    ctx: &egui::Context,
    size: Vec2,
    events: Vec<egui::Event>,
) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
            time: Some(ctx.input(|input| input.time) + 0.05),
            events,
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    )
}
pub(super) fn label(output: &egui::FullOutput, expected: &str) -> Pos2 {
    output
        .shapes
        .iter()
        .find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing visible label: {expected}"))
}
pub(super) fn click(app: &mut PeerBrush, ctx: &egui::Context, size: Vec2, at: Pos2) {
    for pressed in [true, false] {
        draw(
            app,
            ctx,
            size,
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
}
pub(super) fn key(
    app: &mut PeerBrush,
    ctx: &egui::Context,
    size: Vec2,
    key: egui::Key,
    modifiers: egui::Modifiers,
) {
    for pressed in [true, false] {
        draw(
            app,
            ctx,
            size,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }],
        );
    }
}
pub(super) fn source(app: &PeerBrush) -> Value {
    let engine = app.shared.lock().unwrap();
    let history = |entries: &[crate::engine::History]| {
        entries.iter().map(|entry| json!({
        "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
        "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted,
    })).collect::<Vec<_>>()
    };
    json!({"document":engine.doc,"png":engine.doc.export_png().unwrap(),
        "undo":history(&engine.undo),"redo":history(&engine.redo),"closed":engine.closed})
}
pub(super) fn fixture(depth: u16) -> (PeerBrush, egui::Context) {
    let (mut app, ctx) = tests::fixture();
    app.recovery.open = false;
    app.clipboard = crate::clipboard::Worker::memory(ctx.clone());
    ctx.style_mut(|style| style.animation_time = 0.);
    {
        let mut engine = app.shared.lock().unwrap();
        engine.doc = Document::new_depth(32, 24, depth).unwrap();
        app.selected = engine.doc.layers[0].id.clone();
        if depth == 16 {
            engine.doc.layers[0]
                .pixels
                .set16(1, 1, [12347, 33559, 51237, 65535]);
        }
        for point in [[8, 8], [16, 16]] {
            engine
                .edit(
                    "human",
                    &[json!({"op":"paint","layer":app.selected,
                "points":[point],"radius":3,"color":[233,84,32,255]})],
                    None,
                    None,
                    "Human artwork",
                )
                .unwrap();
        }
        engine.undo("human").unwrap();
    }
    app.selection_layers = [app.selected.clone()].into_iter().collect();
    (app, ctx)
}

fn tab_to(app: &mut PeerBrush, ctx: &egui::Context, size: Vec2, target: Pos2) {
    for _ in 0..80 {
        key(app, ctx, size, egui::Key::Tab, Default::default());
        if ctx
            .memory(|memory| memory.focused())
            .and_then(|id| ctx.read_response(id))
            .is_some_and(|response| response.rect.contains(target))
        {
            return;
        }
    }
    panic!("Keyboard navigation did not reach the menu action");
}

#[test]
fn connected_menus_preserve_pointer_keyboard_actions_and_native_sources_at_display_scales() {
    for depth in [8, 16] {
        for (size, scale) in [(Vec2::new(980., 640.), 1.), (Vec2::new(1360., 900.), 1.5)] {
            let (mut app, ctx) = fixture(depth);
            ctx.set_pixels_per_point(scale);
            let before = source(&app);
            for _ in 0..3 {
                draw(&mut app, &ctx, size, vec![]);
            }
            let output = draw(&mut app, &ctx, size, vec![]);
            assert_eq!(output.pixels_per_point, scale);
            let rectangles = ["File", "View", "Image", "Layer", "Select"].map(|name| {
                let center = label(&output, name);
                output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect)
                            if rect.rect.contains(center) && rect.rect.height() < 40. =>
                        {
                            Some(rect.rect)
                        }
                        _ => None,
                    })
                    .min_by(|a, b| a.area().total_cmp(&b.area()))
                    .expect("Painted menu control")
            });
            for pair in rectangles.windows(2) {
                assert!(
                    (pair[0].right() - pair[1].left()).abs() < 1.,
                    "Main menus must connect"
                );
            }
            assert!(rectangles[0].left() >= 0. && rectangles[4].right() <= size.x);
            click(&mut app, &ctx, size, label(&output, "File"));
            let output = draw(&mut app, &ctx, size, vec![]);
            label(&output, "New canvas");
            label(&output, "Updates…");
            label(&output, "About PeerBrush…");
            key(&mut app, &ctx, size, egui::Key::Escape, Default::default());
            draw(
                &mut app,
                &ctx,
                size,
                vec![egui::Event::PointerMoved(Pos2::new(900., 600.))],
            );
            tab_to(&mut app, &ctx, size, rectangles[0].center());
            key(&mut app, &ctx, size, egui::Key::Enter, Default::default());
            let output = draw(&mut app, &ctx, size, vec![]);
            tab_to(&mut app, &ctx, size, label(&output, "New canvas"));
            key(&mut app, &ctx, size, egui::Key::Enter, Default::default());
            assert!(
                app.show_new,
                "Actual keyboard menu action must remain available"
            );
            key(&mut app, &ctx, size, egui::Key::Escape, Default::default());
            assert!(!app.show_new);
            assert_eq!(source(&app), before);
        }
    }
}

#[test]
fn duplicate_file_warning_wraps_and_actual_acknowledgement_preserves_native_work() {
    for depth in [8, 16] {
        for (size, scale) in [(Vec2::new(980., 640.), 1.5), (Vec2::new(1360., 900.), 1.)] {
            let (mut app, ctx) = fixture(depth);
            ctx.set_pixels_per_point(scale);
            let before = source(&app);
            let warning = "This file is already open in another project. The existing project remains available; no duplicate project or automatic overwrite was created. Keep the newer human artwork and use Save As when a separate copy is needed.";
            app.open_file_warning = Some(warning.into());
            for _ in 0..3 {
                draw(&mut app, &ctx, size, vec![]);
            }
            let output = draw(&mut app, &ctx, size, vec![]);
            label(&output, "File already open");
            label(&output, warning);
            let text = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.job.text == warning => {
                        Some(Rect::from_min_size(text.pos, text.galley.size()))
                    }
                    _ => None,
                })
                .unwrap();
            assert!(text.width() <= 440. && text.right() <= size.x && text.bottom() <= size.y);
            assert!(app.open_file_warning.is_some());
            click(&mut app, &ctx, size, label(&output, "OK"));
            assert!(app.open_file_warning.is_none());
            assert_eq!(source(&app), before);
        }
    }
}
