use super::*;

pub(super) fn controls(ui: &mut egui::Ui, settings: &mut Value) -> Option<egui::Response> {
    if settings["contour"] != "custom" {
        return None;
    }
    let mut changed = None;
    egui::CollapsingHeader::new("Contour points")
        .default_open(true)
        .show(ui, |ui| {
            let count = settings["contour_points"].as_array().unwrap().len();
            let add = ui.add_enabled(count < 16, egui::Button::new("Add contour point"));
            if add.clicked() {
                let curve =
                    crate::effects::contours::Curve::decode(&settings["contour_points"]).unwrap();
                let points = settings["contour_points"].as_array_mut().unwrap();
                let index = points
                    .windows(2)
                    .enumerate()
                    .max_by(|(_, a), (_, b)| {
                        let gap = |p: &[Value]| {
                            p[1]["position"].as_f64().unwrap() - p[0]["position"].as_f64().unwrap()
                        };
                        gap(a).total_cmp(&gap(b))
                    })
                    .map(|(index, _)| index)
                    .unwrap();
                let position = (points[index]["position"].as_f64().unwrap()
                    + points[index + 1]["position"].as_f64().unwrap())
                    / 2.;
                points.insert(
                    index + 1,
                    json!({"position":position,"value":curve.sample(position)}),
                );
                changed = Some(add);
            }
            let points = settings["contour_points"].as_array_mut().unwrap();
            let count = points.len();
            let mut remove = None;
            for index in 0..count {
                let previous = if index > 0 {
                    points[index - 1]["position"].as_f64().unwrap()
                } else {
                    0.
                };
                let next = if index + 1 < count {
                    points[index + 1]["position"].as_f64().unwrap()
                } else {
                    1.
                };
                ui.horizontal(|ui| {
                    controls::label(ui, &format!("Point {}", index + 1));
                    let mut position = points[index]["position"].as_f64().unwrap() as f32 * 100.;
                    let response = ui
                        .add_enabled_ui(index > 0 && index + 1 < count, |ui| {
                            controls::range(
                                ui,
                                (index, "contour position"),
                                &mut position,
                                0.0..=100.,
                                75.,
                                "%",
                                1,
                                false,
                            )
                        })
                        .inner;
                    if response.changed() {
                        let epsilon = (next - previous) * 1e-6;
                        let position =
                            (f64::from(position) / 100.).clamp(previous + epsilon, next - epsilon);
                        if position > previous && position < next {
                            points[index]["position"] = json!(position);
                            changed = Some(response);
                        }
                    }
                    controls::label(ui, "Value");
                    let mut value = points[index]["value"].as_f64().unwrap() as f32 * 100.;
                    let response = controls::range(
                        ui,
                        (index, "contour value"),
                        &mut value,
                        0.0..=100.,
                        75.,
                        "%",
                        1,
                        false,
                    );
                    if response.changed() {
                        points[index]["value"] = json!((f64::from(value) / 100.).clamp(0., 1.));
                        changed = Some(response);
                    }
                    let response = ui
                        .add_enabled(index > 0 && index + 1 < count, egui::Button::new("×"))
                        .on_hover_text("Remove this contour point");
                    if response.clicked() {
                        remove = Some(index);
                        changed = Some(response);
                    }
                });
            }
            if let Some(index) = remove {
                points.remove(index);
            }
        });
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contour_value_limit_drags_preview_native_pixels_and_commit_once() {
        for depth in [8, 16] {
            for (delta, endpoint) in [(-300., 0.), (300., 1.)] {
                let ctx = egui::Context::default();
                let mut engine = crate::engine::Engine::new();
                engine.doc = Document::new_depth(32, 24, depth).unwrap();
                for y in 0..24 {
                    for x in 0..32 {
                        engine.doc.layers[0]
                            .pixels
                            .set16(x, y, [12347, 23459, 34571, 65535]);
                    }
                }
                let layer = engine.doc.layers[0].id.clone();
                engine.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"contour","settings":{"contour":"custom","contour_points":[{"position":0.,"value":0.37},{"position":1.,"value":1.}] }})], None, None, "Contour").unwrap();
                engine.undo.clear();
                let before = engine.doc.clone();
                let effect = engine.doc.layers[0].effects[0].id.clone();
                let shared = Arc::new(std::sync::Mutex::new(engine));
                let connection = crate::server::Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: Default::default(),
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
                        doc.preview_native8(None, 32, None, false)
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
                                Vec2::new(1100., 1100.),
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
                    .find_map(|s| match &s.shape {
                        egui::Shape::Text(t) if t.galley.job.text == "37.0%" => {
                            Some(Rect::from_min_size(t.pos, t.galley.size()).center())
                        }
                        _ => None,
                    })
                    .expect("Compact contour value control");
                let end = start + Vec2::new(delta, 0.);
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
                    .expect("Valid contour gesture")
                    .commands
                    .clone();
                let draft =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                assert_eq!(
                    draft.layers[0].effects[0].settings["contour_points"][0]["value"],
                    json!(endpoint)
                );
                assert_ne!(native(&draft), native(&before));
                {
                    let engine = shared.lock().unwrap();
                    assert!(engine.undo.is_empty());
                    assert_eq!(engine.doc.revision, before.revision);
                }
                draw(&mut app, vec![button(end, false)]);
                let mut engine = shared.lock().unwrap();
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(native(&engine.doc), native(&draft));
                assert_eq!(
                    engine.doc.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                engine.undo("human").unwrap();
                assert_eq!(native(&engine.doc), native(&before));
            }
        }
    }
}
