use super::*;
use crate::{effects, engine::Layer};
pub(super) fn settings(
    ui: &mut egui::Ui,
    kind: &str,
    settings: &mut Value,
) -> Option<egui::Response> {
    let mut changed = None;
    let params: Vec<(&str, &str, f32, f32, &str)> = match kind {
        "blur" | "gaussian" => vec![("radius", "Radius", 0.0, 64.0, " px")],
        "levels" => vec![
            ("black", "Black", 0.0, 0.99, ""),
            ("white", "White", 0.01, 1.0, ""),
            ("gamma", "Gamma", 0.1, 5.0, ""),
        ],
        "adjust" => vec![
            ("brightness", "Brightness", -1.0, 1.0, ""),
            ("contrast", "Contrast", 0.0, 4.0, ""),
            ("saturation", "Saturation", 0.0, 3.0, ""),
        ],
        _ => vec![],
    };
    egui::Grid::new((ui.id(), "settings"))
        .spacing(Vec2::new(8.0, 6.0))
        .show(ui, |ui| {
            for (key, label, min, max, suffix) in params {
                controls::label(ui, label);
                let mut value = effects::number(
                    settings,
                    key,
                    if key == "radius" {
                        8.0
                    } else if ["white", "gamma", "contrast", "saturation"].contains(&key) {
                        1.0
                    } else {
                        0.0
                    },
                );
                let response = controls::range(
                    ui,
                    key,
                    &mut value,
                    min..=max,
                    160.0,
                    suffix,
                    if key == "radius" { 0 } else { 2 },
                    false,
                );
                if response.changed() {
                    settings[key] = json!(value);
                    changed = Some(response);
                }
                ui.end_row();
            }
        });
    if kind == "curves" {
        let (rect, response) =
            ui.allocate_exact_size(Vec2::new(240.0, 110.0), egui::Sense::click_and_drag());
        for i in 0..5 {
            let f = i as f32 / 4.0;
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() + f * rect.width(), rect.top()),
                    egui::pos2(rect.left() + f * rect.width(), rect.bottom()),
                ],
                Stroke::new(1.0_f32, MUTED.gamma_multiply(0.12)),
            );
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left(), rect.top() + f * rect.height()),
                    egui::pos2(rect.right(), rect.top() + f * rect.height()),
                ],
                Stroke::new(1.0_f32, MUTED.gamma_multiply(0.12)),
            );
        }
        let to_screen = |x: f32, y: f32| {
            egui::pos2(
                rect.left() + x * rect.width(),
                rect.bottom() - y * rect.height(),
            )
        };
        let points = settings["points"].as_array().cloned().unwrap_or_default();
        let line = points
            .iter()
            .map(|p| to_screen(p[0].as_f64().unwrap() as f32, p[1].as_f64().unwrap() as f32))
            .collect();
        ui.painter().add(egui::Shape::line(
            line,
            Stroke::new(1.8_f32, ui.visuals().selection.stroke.color),
        ));
        for p in &points {
            ui.painter().circle_filled(
                to_screen(p[0].as_f64().unwrap() as f32, p[1].as_f64().unwrap() as f32),
                3.0,
                Color32::WHITE,
            );
        }
        if response.dragged() || response.clicked() {
            if let Some(pointer) = response.interact_pointer_pos() {
                let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pointer);
                let nearest = points
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        to_screen(a[0].as_f64().unwrap() as f32, a[1].as_f64().unwrap() as f32)
                            .distance(origin)
                            .total_cmp(
                                &to_screen(
                                    b[0].as_f64().unwrap() as f32,
                                    b[1].as_f64().unwrap() as f32,
                                )
                                .distance(origin),
                            )
                    })
                    .map(|(i, _)| i);
                let drag_id = response.id.with("curve point");
                if response.drag_started() {
                    if let Some(index) = nearest {
                        ui.ctx().data_mut(|d| d.insert_temp(drag_id, index));
                    }
                }
                let index = if response.dragged() {
                    ui.ctx().data(|d| d.get_temp::<usize>(drag_id)).or(nearest)
                } else {
                    nearest
                };
                if let Some(index) = index {
                    let x = ((pointer.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                    let y = ((rect.bottom() - pointer.y) / rect.height()).clamp(0.0, 1.0);
                    let selected_x = if index == 0 {
                        0.0
                    } else if index + 1 == points.len() {
                        1.0
                    } else {
                        bounded_curve_x(
                            x as f64,
                            points[index - 1][0].as_f64().unwrap(),
                            points[index + 1][0].as_f64().unwrap(),
                            points[index][0].as_f64().unwrap(),
                        )
                    };
                    settings["points"][index] = json!([selected_x, y]);
                    changed = Some(response.clone());
                }
            }
        }
        if response.drag_stopped() {
            ui.ctx()
                .data_mut(|d| d.remove::<usize>(response.id.with("curve point")));
        }
        response.on_hover_text("Drag a curve point to reshape the tones");
    }
    changed
}
// Imported curves can have arbitrarily close, strictly ordered points.
fn bounded_curve_x(x: f64, previous: f64, next: f64, original: f64) -> f64 {
    let margin = 0.01_f64.min((next - previous) * 0.25);
    let low = previous + margin;
    let high = next - margin;
    if low > previous && high < next && low <= high {
        x.clamp(low, high)
    } else {
        original
    }
}
impl PeerBrush {
    pub(super) fn color_stack(&mut self, ui: &mut egui::Ui, l: &Layer) {
        ui.horizontal(|ui| {
            controls::label(ui, "COLOR EFFECTS");
            let add = ui.menu_button("+ Add effect", |ui| {
                for kind in effects::KINDS {
                    if ui.button(effect_name(kind)).clicked() {
                        self.layer_cmd("effect.add", json!({"kind":kind}), "Add color effect");
                        ui.close_menu();
                    }
                }
            });
            self.effect_add_rect = Some(add.response.rect);
        });
        egui::ScrollArea::vertical()
            .id_salt("color effects")
            .max_height(
                (ui.available_height() - if l.mask.is_none() { 38.0 } else { 8.0 }).max(24.0),
            )
            .show(ui, |ui| {
                for (index, effect) in l.effects.iter().enumerate().rev() {
                    ui.push_id(&effect.id, |ui| {
                        ui.horizontal(|ui| {
                            let mut enabled = effect.enabled;
                            if ui.checkbox(&mut enabled, "").changed() {
                                self.layer_cmd(
                                    "effect.update",
                                    json!({"effect":effect.id,"enabled":enabled}),
                                    "Toggle color effect",
                                );
                            }
                            ui.label(effect_name(&effect.kind));
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if icons::small_button(ui, Icon::Trash, "Remove effect")
                                        .clicked()
                                    {
                                        self.layer_cmd(
                                            "effect.delete",
                                            json!({"effect":effect.id}),
                                            "Remove effect",
                                        );
                                    }
                                    if index > 0
                                        && icons::small_button(ui, Icon::Down, "Move effect down")
                                            .clicked()
                                    {
                                        self.layer_cmd(
                                            "effect.reorder",
                                            json!({"effect":effect.id,"index":index-1}),
                                            "Reorder effects",
                                        );
                                    }
                                    if index + 1 < l.effects.len()
                                        && icons::small_button(ui, Icon::Up, "Move effect up")
                                            .clicked()
                                    {
                                        self.layer_cmd(
                                            "effect.reorder",
                                            json!({"effect":effect.id,"index":index+1}),
                                            "Reorder effects",
                                        );
                                    }
                                },
                            );
                        });
                        let mut values = effect.settings.clone();
                        if let Some(response) = settings(ui, &effect.kind, &mut values) {
                            self.layer_parameter(
                                json!({"effect":effect.id,"settings":values}),
                                "Effect parameter",
                                &response,
                            );
                        }
                        ui.add_space(5.0);
                    });
                }
            });
    }
}
pub(super) fn effect_name(kind: &str) -> &str {
    match kind {
        "blur" | "gaussian" => "Gaussian blur",
        "adjust" => "Color adjustment",
        "grayscale" => "Grayscale",
        "levels" => "Levels",
        "curves" => "Curves",
        "invert" => "Invert",
        _ => kind,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn top_color_effect_is_last_without_reinterpreting_saved_sources() {
        let ctx = egui::Context::default();
        let mut e = crate::engine::Engine::new();
        e.doc = Document::new(4, 4).unwrap();
        e.doc.layers[0].kind = "fill".into();
        e.doc.layers[0].color = [64, 64, 64, 255];
        let id = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[
                json!({"op":"effect.add","layer":id,"kind":"invert"}),
                json!({"op":"effect.add","layer":id,"kind":"adjust","settings":{"brightness":0.2}}),
            ],
            None,
            None,
            "Effects",
        )
        .unwrap();
        let source = serde_json::to_value(&e.doc.layers[0].effects).unwrap();
        let layer = e.doc.layers[0].clone();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
        let connection = crate::server::Connection {
            port: 0,
            token: String::new(),
            state_dir: std::path::PathBuf::new(),
            instance_lock: None,
        };
        let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 600.))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.color_stack(ui, &layer));
            },
        );
        let y = |label: &str| {
            output
                .shapes
                .iter()
                .find_map(|s| match &s.shape {
                    egui::Shape::Text(t) if t.galley.job.text == label => Some(t.pos.y),
                    _ => None,
                })
                .unwrap()
        };
        assert!(y("Color adjustment") < y("Invert"));
        let e = shared.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&e.doc.layers[0].effects).unwrap(),
            source
        );
        assert_eq!(
            &e.doc.preview(None, 4, None, false).unwrap().2[..4],
            &[242, 242, 242, 255]
        );
    }
    #[test]
    fn dragging_a_curve_point_keeps_its_identity_near_another_point() {
        let ctx = egui::Context::default();
        let mut values = effects::defaults("curves");
        let mut draw = |events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        settings(ui, "curves", &mut values);
                    });
                },
            )
        };
        let output = draw(vec![]);
        let points = output
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Circle(c) if c.radius == 3.0 && c.fill == Color32::WHITE => {
                    Some(c.center)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(points.len(), 3);
        let start = points[1];
        let end = points[2] + Vec2::new(-4.0, 4.0);
        draw(vec![
            egui::Event::PointerMoved(start),
            egui::Event::PointerButton {
                pos: start,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
        ]);
        draw(vec![egui::Event::PointerMoved(end)]);
        draw(vec![egui::Event::PointerMoved(end + Vec2::new(-2.0, 0.0))]);
        drop(draw);
        assert_eq!(values["points"][2], json!([1.0, 1.0]));
        assert!(values["points"][1][0].as_f64().unwrap() > 0.9);
    }
    #[test]
    fn tight_imported_curve_points_remain_strictly_ordered_when_dragged() {
        for (previous, next, original) in [
            (0.499, 0.501, 0.5),
            (0.5, 0.5000000000000002, 0.5000000000000001),
        ] {
            for x in [0.0, 0.5, 1.0] {
                let adjusted = bounded_curve_x(x, previous, next, original);
                assert!(adjusted > previous && adjusted < next);
            }
        }
    }
}
