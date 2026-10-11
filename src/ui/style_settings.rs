use super::*;
pub(super) fn settings(ui: &mut egui::Ui, kind: &str, value: &mut Value) -> Option<egui::Response> {
    let defaults = crate::effects::styles::defaults(kind);
    let mut changed = None;
    for (key, options) in [
        (
            "blend",
            crate::raster::BLENDS
                .iter()
                .map(|(key, label)| (*key, *label))
                .collect::<Vec<_>>(),
        ),
        (
            "contour",
            vec![
                ("linear", "Linear"),
                ("cone", "Cone"),
                ("ring", "Ring"),
                ("smooth", "Smooth"),
            ],
        ),
        (
            "pattern",
            vec![
                ("checker", "Checker"),
                ("stripes", "Stripes"),
                ("dots", "Dots"),
                ("noise", "Noise"),
            ],
        ),
        (
            "position",
            vec![
                ("inside", "Inside"),
                ("outside", "Outside"),
                ("center", "Center"),
            ],
        ),
        ("source", vec![("edge", "Edge"), ("center", "Center")]),
        (
            "style",
            if kind == "bevel_emboss" {
                vec![
                    ("inner", "Inner bevel"),
                    ("outer", "Outer bevel"),
                    ("emboss", "Emboss"),
                    ("pillow", "Pillow emboss"),
                ]
            } else {
                vec![
                    ("linear", "Linear"),
                    ("radial", "Radial"),
                    ("angle", "Angle"),
                    ("reflected", "Reflected"),
                    ("diamond", "Diamond"),
                ]
            },
        ),
    ] {
        if defaults.get(key).is_none() {
            continue;
        }
        let mut selected = value[key]
            .as_str()
            .or_else(|| defaults[key].as_str())
            .unwrap_or("")
            .to_string();
        ui.horizontal(|ui| {
            controls::label(
                ui,
                match key {
                    "blend" => "Blend",
                    "contour" => "Contour",
                    "pattern" => "Pattern",
                    "position" => "Position",
                    "source" => "Source",
                    _ => "Style",
                },
            );
            let label = options
                .iter()
                .find(|(id, _)| *id == selected)
                .map_or(selected.as_str(), |(_, label)| *label);
            egui::ComboBox::from_id_salt((ui.id(), key))
                .selected_text(label)
                .show_ui(ui, |ui| {
                    for (id, label) in options {
                        let r = ui.selectable_value(&mut selected, id.into(), label);
                        if r.changed() {
                            value[key] = json!(selected);
                            changed = Some(r);
                        }
                    }
                });
        });
    }
    egui::Grid::new((ui.id(), "style controls"))
        .spacing(Vec2::new(8., 6.))
        .show(ui, |ui| {
            for (key, label, min, max, suffix) in [
                ("opacity", "Opacity", 0., 1., "%"),
                ("size", "Size", 0., 64., " px"),
                ("soften", "Soften", 0., 64., " px"),
                ("distance", "Distance", 0., 128., " px"),
                ("angle", "Angle", -180., 180., "°"),
                ("altitude", "Altitude", 0., 90., "°"),
                (
                    "spread",
                    if kind == "inner_shadow" {
                        "Choke"
                    } else {
                        "Spread"
                    },
                    0.,
                    1.,
                    "%",
                ),
                ("depth", "Depth", 0., 10., "%"),
                ("range", "Range", 0.01, 1., "%"),
                (
                    "scale",
                    "Scale",
                    if kind == "gradient_overlay" { 0.1 } else { 1. },
                    if kind == "gradient_overlay" {
                        10.
                    } else {
                        128.
                    },
                    if kind == "gradient_overlay" {
                        "%"
                    } else {
                        " px"
                    },
                ),
            ] {
                if defaults.get(key).is_none() {
                    continue;
                }
                controls::label(ui, label);
                let multiplier = if suffix == "%" { 100. } else { 1. };
                let mut n = value[key]
                    .as_f64()
                    .or_else(|| defaults[key].as_f64())
                    .unwrap_or(0.) as f32
                    * multiplier;
                let r = controls::range(
                    ui,
                    key,
                    &mut n,
                    min * multiplier..=max * multiplier,
                    160.,
                    suffix,
                    if suffix == " px" { 1 } else { 0 },
                    false,
                );
                if r.changed() {
                    value[key] = json!(n / multiplier);
                    changed = Some(r);
                }
                ui.end_row();
            }
        });
    for (key, label) in [
        (
            "color",
            if kind == "bevel_emboss" {
                "Highlight"
            } else {
                "Color"
            },
        ),
        (
            "color2",
            if kind == "bevel_emboss" {
                "Shadow"
            } else {
                "Second color"
            },
        ),
    ] {
        if defaults.get(key).is_none() {
            continue;
        }
        let mut rgb: [f32; 3] = std::array::from_fn(|i| {
            value[key][i]
                .as_f64()
                .or_else(|| defaults[key][i].as_f64())
                .unwrap_or(0.) as f32
        });
        ui.horizontal(|ui| {
            controls::label(ui, label);
            for (index, label) in ["R", "G", "B"].into_iter().enumerate() {
                let mut n = rgb[index] * 100.;
                let r = controls::range(ui, (key, index), &mut n, 0.0..=100., 60., "%", 1, false)
                    .on_hover_text(label);
                if r.changed() {
                    rgb[index] = n / 100.;
                    value[key] = json!(rgb);
                    changed = Some(r);
                }
            }
        });
    }
    for (key, label) in [
        ("reverse", "Reverse"),
        ("invert", "Invert"),
        ("down", "Down"),
    ] {
        if defaults.get(key).is_some() {
            let mut enabled = value[key]
                .as_bool()
                .or_else(|| defaults[key].as_bool())
                .unwrap_or(false);
            let r = ui.checkbox(&mut enabled, label);
            if r.changed() {
                value[key] = json!(enabled);
                changed = Some(r);
            }
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn style_opacity_drag_previews_native_pixels_and_commits_one_undo() {
        for depth in [8, 16] {
            for kind in ["color_overlay", "drop_shadow"] {
                let ctx = egui::Context::default();
                let mut e = crate::engine::Engine::new();
                e.doc = Document::new_depth(32, 32, depth).unwrap();
                for y in 8..24 {
                    for x in 8..24 {
                        e.doc.layers[0]
                            .pixels
                            .set16(x, y, [10001, 30003, 50007, 65535]);
                    }
                }
                let layer = e.doc.layers[0].id.clone();
                e.edit(
                    "human",
                    &[json!({"op":"effect.add","layer":layer,"kind":kind})],
                    None,
                    None,
                    "Style",
                )
                .unwrap();
                e.undo.clear();
                let before = e.doc.clone();
                let effect = e.doc.layers[0].effects[0].id.clone();
                let shared = Arc::new(std::sync::Mutex::new(e));
                let connection = crate::server::Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: std::path::PathBuf::new(),
                    instance_lock: None,
                    availability: Default::default(),
                };
                let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
                app.selected = layer.clone();
                app.selection_layers = [layer.clone()].into_iter().collect();
                app.effect_selected = Some((layer, false, effect));
                let native = |doc: &Document| {
                    if depth == 16 {
                        crate::depth16::render(doc).unwrap().words
                    } else {
                        doc.preview(None, 32, None, false)
                            .unwrap()
                            .2
                            .into_iter()
                            .map(u16::from)
                            .collect()
                    }
                };
                let mut time = 0.;
                let mut draw = |app: &mut PeerBrush, events| {
                    time += 0.05;
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1100., 1000.),
                            )),
                            time: Some(time),
                            events,
                            ..Default::default()
                        },
                        |ctx| app.draw(ctx),
                    )
                };
                draw(&mut app, vec![]);
                let output = draw(&mut app, vec![]);
                let start = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "75%" => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()).center())
                        }
                        _ => None,
                    })
                    .expect("Compact style opacity control");
                let end = start - Vec2::new(28., 0.);
                let button = |pos, pressed| egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                };
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                draw(&mut app, vec![egui::Event::PointerMoved(end)]);
                let commands = app
                    .parameter_gesture
                    .as_ref()
                    .expect("Style gesture")
                    .commands
                    .clone();
                let draft =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                assert_ne!(native(&draft), native(&before));
                {
                    let e = shared.lock().unwrap();
                    assert!(e.undo.is_empty());
                    assert_eq!(e.doc.revision, before.revision);
                    assert_eq!(
                        e.doc.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                }
                draw(&mut app, vec![button(end, false)]);
                let mut e = shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1);
                assert_eq!(native(&e.doc), native(&draft));
                assert_eq!(
                    e.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                e.undo("human").unwrap();
                assert_eq!(native(&e.doc), native(&before));
            }
        }
    }
}
