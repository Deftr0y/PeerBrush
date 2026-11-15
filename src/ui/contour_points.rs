use super::*;

pub(super) fn swatch(ui: &egui::Ui, rect: Rect, preset: &str, points: &Value) {
    let custom = (preset == "custom")
        .then(|| crate::effects::contours::Curve::decode(points).ok())
        .flatten();
    let position = |t: f64, value: f64| {
        egui::pos2(rect.left() + t as f32 * rect.width(), rect.bottom() - value as f32 * rect.height())
    };
    let samples = (0..=24)
        .map(|i| {
            let t = f64::from(i) / 24.;
            let value = custom.as_ref().map_or_else(
                || crate::effects::styles::profile(t, preset),
                |curve| curve.sample(t),
            );
            position(t, value)
        })
        .collect();
    let color = ui.visuals().text_color();
    ui.painter().add(egui::Shape::line(samples, Stroke::new(1.25, color)));
    if preset == "custom" {
        for t in [0., 1.] {
            let value = custom.as_ref().map_or(t, |curve| curve.sample(t));
            ui.painter().circle_filled(position(t, value), 1.8, color);
        }
    }
}

pub(super) fn controls(ui: &mut egui::Ui, settings: &mut Value) -> Option<egui::Response> {
    if settings["contour"] != "custom" {
        return None;
    }
    let mut changed = None;
    egui::CollapsingHeader::new("Contour points")
        .default_open(true)
        .show(ui, |ui| {
            if let Some(response) = graph(ui, settings) {
                changed = Some(response);
            }
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
                        let position = bounded_position(f64::from(position) / 100., previous, next);
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

fn graph(ui: &mut egui::Ui, settings: &mut Value) -> Option<egui::Response> {
    let points = settings["contour_points"].as_array_mut()?;
    let (outer, _) = ui.allocate_exact_size(Vec2::new(ui.available_width().min(540.), 106.), egui::Sense::hover());
    let rect = outer.shrink(9.);
    if rect.width() <= 1. {
        return None;
    }
    let to_screen = |point: &Value| {
        egui::pos2(
            rect.left() + rect.width() * point["position"].as_f64().unwrap() as f32,
            rect.bottom() - rect.height() * point["value"].as_f64().unwrap() as f32,
        )
    };
    let mut changed = None;
    for index in 0..points.len() {
        let screen = to_screen(&points[index]);
        let (mut response, position) = controls::point_handle(
            ui, ui.make_persistent_id(("contour graph", index)), screen,
            &format!("Contour point {}. Drag sideways for input and up or down for output.", index + 1),
        );
        if let Some(point) = position {
            let old_x = points[index]["position"].as_f64().unwrap();
            let old_y = points[index]["value"].as_f64().unwrap();
            let x = if index == 0 || index + 1 == points.len() || (point.x - screen.x).abs() < 0.001 {
                old_x
            } else {
                let left = points[index - 1]["position"].as_f64().unwrap();
                let right = points[index + 1]["position"].as_f64().unwrap();
                bounded_position(f64::from((point.x - rect.left()) / rect.width()), left, right)
            };
            let y = if (point.y - screen.y).abs() < 0.001 {
                old_y
            } else {
                f64::from((rect.bottom() - point.y) / rect.height()).clamp(0., 1.)
            };
            if old_x != x || old_y != y {
                points[index]["position"] = json!(x);
                points[index]["value"] = json!(y);
                response.mark_changed();
                changed = Some(response);
            }
        }
    }
    let painter = ui.painter();
    let grid = Stroke::new(1., ui.visuals().weak_text_color().gamma_multiply(0.28));
    for fraction in [0., 0.25, 0.5, 0.75, 1.] {
        let y = rect.bottom() - rect.height() * fraction;
        painter.hline(rect.x_range(), y, grid);
    }
    let curve = points.iter().map(to_screen).collect::<Vec<_>>();
    let accent = ui.visuals().selection.stroke.color;
    painter.add(egui::Shape::line(curve, Stroke::new(1.7, accent)));
    for point in points {
        painter.circle_filled(to_screen(point), 3.5, accent);
    }
    changed
}

fn bounded_position(position: f64, previous: f64, next: f64) -> f64 {
    let epsilon = (next - previous) * 1e-6;
    position.clamp(
        (previous + epsilon).max(previous.next_up()),
        (next - epsilon).min(next.next_down()),
    )
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
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
                for y in 0..24 {
                    for x in 0..32 {
                        engine.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(x, y, [12347, 23459, 34571, 65535]);
                    }
                }
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                engine.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"contour","settings":{"contour":"custom","contour_points":[{"position":0.,"value":0.37},{"position":1.,"value":1.}] }})], None, None, "Contour").unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                let before = engine.resident_state().expect("resident fixture").document.clone();
                let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
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
                    assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                    assert_eq!(engine.resident_state().expect("resident fixture").document.revision, before.revision);
                }
                draw(&mut app, vec![button(end, false)]);
                let mut engine = shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(native(&engine.resident_state().expect("resident fixture").document), native(&draft));
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                engine.undo("human").unwrap();
                assert_eq!(native(&engine.resident_state().expect("resident fixture").document), native(&before));
            }
        }
    }
}
