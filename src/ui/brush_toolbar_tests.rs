use super::*;
fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
            events,
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    )
}
fn button(pos: Pos2, pressed: bool) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: Default::default(),
    }
}
fn label(out: &egui::FullOutput, text: &str) -> Pos2 {
    out.shapes
        .iter()
        .find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.job.text == text => {
                Some(t.pos + t.galley.size() * 0.5)
            }
            _ => None,
        })
        .unwrap_or_else(|| panic!("Missing visible toolbar value {text}"))
}
#[test]
fn toolbar_opacity_edits_shared_brush_settings_and_real_native_strokes_without_extra_history() {
    for depth in [8, 16] {
        let (mut app, ctx) = super::tests::fixture();
        app.recovery.open = false;
        {
            let mut e = app.shared.lock().unwrap();
            e.doc = Document::new_depth(64, 64, depth).unwrap();
            app.selected = e.doc.layers[0].id.clone();
        }
        app.brush = crate::brush::Settings {
            opacity: 0.73,
            hardness: 1.,
            flow: 1.,
            smoothing: 0.,
            ..Default::default()
        };
        app.radius = 6.;
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![]);
        }
        app.shared.lock().unwrap().edit("ai", &[json!({"op":"paint","layer":app.selected,"points":[[12,12]],"radius":3,"color":[233,84,32,255],"opacity":0.2})], None, None, "AI stroke").unwrap();
        let out = frame(&mut app, &ctx, vec![]);
        let start = label(&out, "73%");
        assert!(start.y < 140. && !app.show_brush);
        assert!(out.shapes.iter().any(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill == AI_BLUE && r.rect.top() < 140. && r.rect.width() > 50. && r.rect.height() >= 20.)), "AI brush feedback must be blue");
        let before = app.shared.lock().unwrap().doc.export_png().unwrap();
        let revision = app.shared.lock().unwrap().doc.revision;
        // The compact value is painted inside the filled portion of the bar.
        let end = start - Vec2::new(22., 0.);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(end)]);
        frame(&mut app, &ctx, vec![button(end, false)]);
        assert!(app.brush.opacity > 0. && app.brush.opacity < 0.73);
        assert!(app.animation.tool(ctx.input(|i| i.time)).is_none());
        assert_eq!(app.shared.lock().unwrap().doc.revision, revision);
        assert_eq!(app.shared.lock().unwrap().undo.len(), 1);
        assert_eq!(app.shared.lock().unwrap().doc.export_png().unwrap(), before);
        let percent = format!("{:.0}%", app.brush.opacity * 100.);
        app.show_brush = true;
        let mut out = frame(&mut app, &ctx, vec![]);
        for _ in 0..3 {
            out = frame(&mut app, &ctx, vec![]);
        }
        assert!(
            out.shapes
                .iter()
                .filter(
                    |s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.job.text == percent)
                )
                .count()
                >= 2,
            "Toolbar and brush menu must show the same opacity"
        );
        app.show_brush = false;
        for tool in [
            Tool::Brush,
            Tool::Eraser,
            Tool::Smudge,
            Tool::Clone,
            Tool::Heal,
        ] {
            app.select_tool(tool);
            let out = frame(&mut app, &ctx, vec![]);
            let active_percent = format!("{:.0}%", app.brush.opacity * 100.);
            assert!(label(&out, &active_percent).y < 140.);
            let command = app.stroke_command(vec![[32., 32.]], [233, 84, 32, 255]);
            assert!(
                (command["opacity"].as_f64().unwrap() as f32 - app.brush.opacity).abs() < 0.00001
            );
        }
        app.select_tool(Tool::Brush);
        frame(&mut app, &ctx, vec![]);
        let point = app.view_rect.unwrap().center();
        let moved = point + Vec2::new(26., 0.);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::PointerMoved(point), button(point, true)],
        );
        frame(&mut app, &ctx, vec![egui::Event::PointerMoved(moved)]);
        frame(&mut app, &ctx, vec![button(moved, false)]);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(
            e.undo.len(),
            2,
            "A completed stroke commits exactly one undo step"
        );
        assert_eq!(e.doc.bit_depth, depth);
        let alpha = e.doc.layers[0].pixels.get16(32, 32)[3];
        let expected = (app.brush.opacity * 65535.).round() as u16;
        assert!(
            alpha.abs_diff(expected) <= if depth == 8 { 257 } else { 2 },
            "Native alpha {alpha} differs from toolbar opacity {expected}"
        );
        let saved = crate::psd::decode(&crate::psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(saved.bit_depth, depth);
        assert_eq!(
            saved.layers[0].pixels.get16(32, 32),
            e.doc.layers[0].pixels.get16(32, 32)
        );
        assert_eq!(saved.export_png().unwrap(), e.doc.export_png().unwrap());
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before);
    }
}
