use super::*;
use crate::{effects, engine::Layer};
pub(super) fn settings(
    ui: &mut egui::Ui,
    kind: &str,
    settings: &mut Value,
) -> Option<egui::Response> {
    if crate::effects::styles::contains(kind) {
        return style_settings::settings(ui, kind, settings);
    }
    if kind == "channel_clamp" {
        return channel_clamp_settings(ui, settings);
    }
    let mut changed = None;
    if kind == "levels" {
        changed = levels_slider(ui, settings);
    }
    let params: Vec<(&str, &str, f32, f32, &str)> = match kind {
        "posterize" => vec![("levels", "Levels", 2.0, 256.0, "")],
        "blur" | "gaussian" => vec![("radius", "Radius", 0.0, 64.0, " px")],
        "levels" => vec![
            ("black", "Black", 0.0, 0.99, ""),
            ("white", "White", 0.01, 1.0, ""),
            ("gamma", "Gamma", 0.1, 5.0, ""),
        ],
        "hsl" => vec![
            ("hue", "Hue", -180.0, 180.0, "°"),
            ("saturation", "Saturation", -1.0, 1.0, "%"),
            ("lightness", "Lightness", -1.0, 1.0, "%"),
        ],
        "bloom" => vec![
            ("threshold", "Threshold", 0.0, 1.0, "%"),
            ("spread", "Spread", 0.0, 64.0, " px"),
            ("strength", "Strength", 0.0, 3.0, "%"),
        ],
        "liquify" => vec![
            ("radius", "Default radius", 0.5, 512.0, " px"),
            ("strength", "Amount", 0.0, 1.0, "%"),
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
                let defaults = effects::defaults(kind);
                let default = defaults[key]
                    .as_f64()
                    .map(|x| x as f32)
                    .unwrap_or(if kind == "gaussian" { 8.0 } else { 0.0 });
                let scale = if suffix == "%" { 100.0 } else { 1.0 };
                let mut value = effects::number(settings, key, default) * scale;
                let response = controls::range(
                    ui,
                    key,
                    &mut value,
                    min * scale..=max * scale,
                    160.0,
                    suffix,
                    if scale == 100.0 || ["radius", "spread", "hue", "levels"].contains(&key) {
                        0
                    } else {
                        2
                    },
                    false,
                );
                if response.changed() || response.double_clicked() {
                    settings[key] = if key == "levels" {
                        json!(value.round() as u16)
                    } else {
                        json!(value / scale)
                    };
                    changed = Some(response);
                }
                ui.end_row();
            }
        });
    if kind == "color_balance" {
        let band_id = ui.id().with("color balance band");
        let mut selected = ui
            .ctx()
            .data(|d| d.get_temp::<usize>(band_id))
            .unwrap_or(1)
            .min(2);
        ui.horizontal(|ui| {
            for (i, label) in ["Shadows", "Midtones", "Highlights"]
                .into_iter()
                .enumerate()
            {
                ui.selectable_value(&mut selected, i, label);
            }
        });
        ui.ctx().data_mut(|d| d.insert_temp(band_id, selected));
        let band = ["shadows", "midtones", "highlights"][selected];
        let mut values: [f32; 3] =
            std::array::from_fn(|i| settings[band][i].as_f64().unwrap_or(0.0) as f32);
        egui::Grid::new((ui.id(), "balance axes"))
            .spacing(Vec2::new(8.0, 6.0))
            .show(ui, |ui| {
                for (i, label) in ["Cyan ↔ Red", "Magenta ↔ Green", "Yellow ↔ Blue"]
                    .into_iter()
                    .enumerate()
                {
                    controls::label(ui, label);
                    let mut value = values[i] * 100.0;
                    let response = controls::range(
                        ui,
                        (band, i),
                        &mut value,
                        -100.0..=100.0,
                        160.0,
                        "",
                        0,
                        false,
                    );
                    if response.changed() || response.double_clicked() {
                        values[i] = value / 100.0;
                        settings[band] = json!(values);
                        changed = Some(response);
                    }
                    ui.end_row();
                }
            });
        let mut preserve = settings["preserve_luminosity"].as_bool().unwrap_or(true);
        let response = ui.checkbox(&mut preserve, "Preserve luminosity");
        if response.changed() || response.double_clicked() {
            settings["preserve_luminosity"] = json!(preserve);
            changed = Some(response);
        }
    }
    if kind == "liquify" {
        let mut strokes = settings["strokes"].as_array().cloned().unwrap_or_default();
        let mut removed = None;
        let mut edited = false;
        for (index, stroke) in strokes.iter_mut().enumerate() {
            ui.push_id(("liquify stroke", index), |ui| {
                let mode = stroke["mode"].as_str().unwrap_or("push");
                let label = match mode {
                    "expand" => "Expand",
                    "pinch" => "Pinch",
                    "restore" => "Restore",
                    _ => "Push",
                };
                ui.horizontal(|ui| {
                    egui::CollapsingHeader::new(format!("{} · {}", index + 1, label))
                        .id_salt("stroke")
                        .show(ui, |ui| {
                            egui::Grid::new("stroke params")
                                .spacing(Vec2::new(8.0, 6.0))
                                .show(ui, |ui| {
                                    for (key, label, min, max, suffix, default) in [
                                        (
                                            "radius",
                                            "Radius",
                                            0.5,
                                            512.0,
                                            " px",
                                            effects::number(settings, "radius", 40.0),
                                        ),
                                        ("strength", "Strength", 0.0, 1.0, "%", 1.0),
                                    ] {
                                        controls::label(ui, label);
                                        let scale = if key == "strength" { 100.0 } else { 1.0 };
                                        let mut value =
                                            effects::number(stroke, key, default) * scale;
                                        let response = controls::range(
                                            ui,
                                            key,
                                            &mut value,
                                            min * scale..=max * scale,
                                            160.0,
                                            suffix,
                                            0,
                                            false,
                                        );
                                        if response.changed() || response.double_clicked() {
                                            stroke[key] = json!(value / scale);
                                            edited = true;
                                            changed = Some(response);
                                        }
                                        ui.end_row();
                                    }
                                });
                        });
                    let response = icons::small_button(ui, Icon::Trash, "Remove liquify stroke");
                    if response.clicked() {
                        removed = Some(index);
                        changed = Some(response);
                        edited = true;
                    }
                });
            });
        }
        if let Some(index) = removed {
            strokes.remove(index);
        }
        if edited {
            settings["strokes"] = json!(strokes);
        }
    }
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
        let line = (0..=256)
            .map(|i| {
                let x = i as f32 / 256.0;
                to_screen(x, effects::curve_value(settings, x))
            })
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
        if response.double_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let x = ((p.x - rect.left()) / rect.width()).clamp(0.001, 0.999);
                let y = ((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0);
                if points.len() < 16
                    && points
                        .iter()
                        .all(|p| (p[0].as_f64().unwrap() - x as f64).abs() > 0.001)
                {
                    let mut updated = points.clone();
                    updated.push(json!([x, y]));
                    updated
                        .sort_by(|a, b| a[0].as_f64().unwrap().total_cmp(&b[0].as_f64().unwrap()));
                    settings["interpolation"] = json!("smooth");
                    settings["points"] = json!(updated);
                    changed = Some(response.clone());
                }
            }
        } else if response.secondary_clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                if let Some(index) = points.iter().enumerate().find_map(|(i, v)| {
                    (i > 0
                        && i + 1 < points.len()
                        && to_screen(v[0].as_f64().unwrap() as f32, v[1].as_f64().unwrap() as f32)
                            .distance(p)
                            < 10.0)
                        .then_some(i)
                }) {
                    let mut updated = points.clone();
                    updated.remove(index);
                    settings["interpolation"] = json!("smooth");
                    settings["points"] = json!(updated);
                    changed = Some(response.clone());
                }
            }
        } else if response.dragged() || response.clicked() || response.drag_started() {
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
                    .filter(|(_, p)| {
                        to_screen(p[0].as_f64().unwrap() as f32, p[1].as_f64().unwrap() as f32)
                            .distance(origin)
                            < 10.0
                    })
                    .map(|(i, _)| i);
                let drag_id = response.id.with("curve point");
                if response.drag_started() {
                    if let Some(index) = nearest {
                        ui.ctx().data_mut(|d| d.insert_temp(drag_id, index));
                    }
                }
                let index = if response.dragged() {
                    ui.ctx().data(|d| d.get_temp::<usize>(drag_id))
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
                    settings["interpolation"] = json!("smooth");
                    settings["points"][index] = json!([selected_x, y]);
                    changed = Some(response.clone());
                }
            }
        }
        if response.drag_stopped() {
            ui.ctx()
                .data_mut(|d| d.remove::<usize>(response.id.with("curve point")));
        }
        response.on_hover_text(
            "Double-click to add a point · Drag to shape · Right-click a point to remove",
        );
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
pub(super) fn effect_icon(kind: &str) -> Icon {
    match kind {
        "paint" => Icon::Brush,
        "fill" => Icon::Fill,
        "levels" | "posterize" | "channel_clamp" => Icon::Levels,
        "curves" => Icon::Curves,
        "blur" | "gaussian" => Icon::Blur,
        "adjust" => Icon::Adjust,
        "color_balance" => Icon::ColorBalance,
        "hsl" => Icon::Hue,
        "bloom" => Icon::Bloom,
        "liquify" => Icon::Liquify,
        "invert" => Icon::Invert,
        "grayscale" => Icon::Grayscale,
        _ => Icon::Adjust,
    }
}
pub(super) fn menu_effect(ui: &mut egui::Ui, kind: &str, label: &str) -> egui::Response {
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
        icons::paint(ui.painter(), rect, effect_icon(kind), ui.is_enabled());
        ui.button(label)
    })
    .inner
}
impl PeerBrush {
    pub(super) fn effect_picker(&mut self, ui: &mut egui::Ui, mask: bool, adjustment: bool) {
        let search_id = ui.id().with(("effect search", mask));
        let mut search = ui
            .ctx()
            .data_mut(|data| data.get_temp::<String>(search_id).unwrap_or_default());
        ui.set_min_width(230.0);
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut search)
                    .desired_width(195.0)
                    .hint_text("Search effects…"),
            );
            if !search.is_empty() && ui.small_button("×").on_hover_text("Clear search").clicked() {
                search.clear();
            }
        });
        ui.ctx()
            .data_mut(|data| data.insert_temp(search_id, search.clone()));
        let entries = effects::catalog::ENTRIES
            .iter()
            .filter(|entry| entry.matches(mask, &search))
            .filter(|entry| {
                !(adjustment
                    && (entry.kind == "channel_clamp"
                        || crate::effects::styles::contains(entry.kind)))
            })
            .collect::<Vec<_>>();
        if entries.is_empty() {
            ui.label(RichText::new("No matching effects").color(MUTED));
        }
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                for category in effects::catalog::CATEGORIES {
                    let category_entries = entries
                        .iter()
                        .filter(|entry| entry.category == *category)
                        .collect::<Vec<_>>();
                    if category_entries.is_empty() {
                        continue;
                    }
                    controls::label(ui, category);
                    for entry in category_entries {
                        if menu_effect(ui, entry.kind, entry.name(mask)).clicked() {
                            self.layer_cmd(
                                if mask { "mask.step.add" } else { "effect.add" },
                                json!({"kind":entry.kind}),
                                if mask {
                                    "Add mask step"
                                } else {
                                    "Add color effect"
                                },
                            );
                            ui.close_menu();
                        }
                    }
                }
            });
    }
    pub(super) fn effect_is_selected(&self, layer: &str, mask: bool, effect: &str) -> bool {
        self.effect_selected
            .as_ref()
            .is_some_and(|(l, m, e)| l == layer && *m == mask && e == effect)
    }
    pub(super) fn select_effect(&mut self, layer: &str, mask: bool, effect: &str) {
        self.effect_selected = if self.effect_is_selected(layer, mask, effect) {
            None
        } else {
            Some((layer.into(), mask, effect.into()))
        };
    }
    pub(super) fn effect_weight(
        &mut self,
        ui: &mut egui::Ui,
        effect: &str,
        weight: f32,
        mask: bool,
    ) -> egui::Response {
        let mut value = weight * 100.0;
        let width = ui.available_width().clamp(70.0, 210.0);
        let response = controls::range(
            ui,
            (effect, "weight"),
            &mut value,
            0.0..=100.0,
            width,
            "%",
            0,
            false,
        )
        .on_hover_text("Effect strength");
        if response.changed() || response.double_clicked() {
            let extra = if mask {
                json!({"step":effect,"weight":value/100.0})
            } else {
                json!({"effect":effect,"weight":value/100.0})
            };
            self.layer_parameter(extra, "Effect weight", &response);
        }
        response
    }
    pub(super) fn effect_title(ui: &mut egui::Ui, name: &str, selected: bool) -> egui::Response {
        // Reserve room for the inline strength even when an effect has a long name.
        let width = (ui.available_width() - 120.0).clamp(24.0, 125.0);
        ui.add_sized(
            [width, 22.0],
            egui::Button::new(RichText::new(name).size(12.0))
                .selected(selected)
                .frame(false)
                .truncate(),
        )
        .on_hover_text(format!(
            "{name} · Select to edit settings · click again to close · right-click for actions"
        ))
    }
    pub(super) fn effect_bake_menu(
        &mut self,
        response: &egui::Response,
        l: &Layer,
        mask: bool,
        effect: &str,
        index: usize,
        revision: u64,
    ) {
        response.context_menu(|ui| self.effect_options(ui, l, mask, effect, index, revision));
    }
    pub(super) fn effect_actions(
        &mut self,
        ui: &mut egui::Ui,
        l: &Layer,
        mask: bool,
        effect: &str,
        index: usize,
        revision: u64,
    ) -> egui::Response {
        let menu = egui::menu::menu_custom_button(ui, egui::Button::new("⋯").frame(false), |ui| {
            self.effect_options(ui, l, mask, effect, index, revision);
        })
        .response
        .on_hover_text("Effect actions · Top effects run last");
        menu.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), "Effect actions")
        });
        menu
    }
    fn effect_present(&self, layer: &str, mask: bool, effect: &str) -> bool {
        self.shared
            .lock()
            .unwrap()
            .doc
            .layers
            .iter()
            .find(|entry| entry.id == layer)
            .is_some_and(|entry| {
                if mask {
                    entry
                        .mask
                        .as_ref()
                        .is_some_and(|m| m.steps.iter().any(|step| step.id == effect))
                } else {
                    entry.effects.iter().any(|entry| entry.id == effect)
                }
            })
    }
    fn effect_options(
        &mut self,
        ui: &mut egui::Ui,
        l: &Layer,
        mask: bool,
        effect: &str,
        index: usize,
        revision: u64,
    ) {
        let count = if mask {
            l.mask.as_ref().map_or(0, |m| m.steps.len())
        } else {
            l.effects.len()
        };
        ui.label(RichText::new("Top effects run last").size(11.).color(MUTED));
        for (label, available, destination) in [
            ("Move up · run later", index + 1 < count, index + 1),
            (
                "Move down · run earlier",
                index > 0,
                index.saturating_sub(1),
            ),
        ] {
            if ui
                .add_enabled(available, egui::Button::new(label))
                .clicked()
            {
                self.layer_cmd(
                    if mask {
                        "mask.step.reorder"
                    } else {
                        "effect.reorder"
                    },
                    if mask {
                        json!({"step":effect,"index":destination})
                    } else {
                        json!({"effect":effect,"index":destination})
                    },
                    "Reorder effect",
                );
                ui.close_menu();
            }
        }
        if ui
            .button(if mask {
                "Remove mask effect"
            } else {
                "Remove effect"
            })
            .clicked()
        {
            self.layer_cmd(
                if mask {
                    "mask.step.delete"
                } else {
                    "effect.delete"
                },
                if mask {
                    json!({"step":effect})
                } else {
                    json!({"effect":effect})
                },
                "Remove effect",
            );
            let removed = !self.effect_present(&l.id, mask, effect);
            if removed {
                if self.effect_is_selected(&l.id, mask, effect) {
                    self.effect_selected = None;
                }
                if mask && self.mask_step.as_deref() == Some(effect) {
                    self.mask_step = None;
                }
            }
            ui.close_menu();
        }
        ui.separator();
        let scope = if mask {
            "mask pixels"
        } else if l.kind == "group" {
            "folder pixels (flattens children)"
        } else if l.source.is_some() {
            "pixels (rasterizes text/vector)"
        } else {
            "layer pixels"
        };
        let label = format!("Bake into {scope}");
        ui.label(format!(
            "Includes this effect and {index} preceding {}",
            if index == 1 { "effect" } else { "effects" }
        ));
        let enabled = l.kind != "adjustment" || mask;
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            let command = if mask {
                json!({"op":"mask.step.bake","layer":l.id,"step":effect,"source_revision":revision})
            } else {
                json!({"op":"effect.bake","layer":l.id,"effect":effect,"source_revision":revision})
            };
            self.edit(vec![command], "Bake effect and preceding effects");
            if !self.effect_present(&l.id, mask, effect) {
                self.effect_selected = None;
                self.mask_step = None;
            }
            ui.close_menu();
        }
        if !enabled {
            ui.label(
                "Adjustment effects depend on the backdrop. Merge affected layers explicitly.",
            );
        }
    }
    pub(super) fn effect_heading(ui: &mut egui::Ui, mask: bool) {
        ui.label(RichText::new(if mask { "MASK EFFECTS" } else { "COLOR EFFECTS" }).size(10.).color(MUTED))
            .on_hover_text("Bottom runs first · Top runs last · Select a name for settings · Row menu for reorder, remove or bake");
    }
    pub(super) fn mask_stack(&mut self, ui: &mut egui::Ui, doc: &Document, l: &Layer) {
        let Some(m) = &l.mask else {
            return;
        };
        ui.horizontal(|ui| {
            Self::effect_heading(ui, true);
            let add = ui.menu_button("+ Add effect", |ui| {
                self.effect_picker(ui, true, l.kind == "adjustment")
            });
            self.effect_add_rect = Some(add.response.rect);
            let mut enabled = m.enabled;
            if ui
                .checkbox(&mut enabled, "On")
                .on_hover_text("Enable or bypass the whole mask")
                .changed()
            {
                self.layer_cmd("mask.toggle", json!({"enabled":enabled}), "Toggle mask");
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("mask effects")
            .max_height((ui.available_height() - 8.0).max(24.0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                for (index, step) in m.steps.iter().enumerate().rev() {
                    ui.push_id(&step.id, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let mut enabled = step.enabled;
                            if ui
                                .checkbox(&mut enabled, "")
                                .on_hover_text("Enable mask effect")
                                .changed()
                            {
                                self.layer_cmd(
                                    "mask.step.update",
                                    json!({"step":step.id,"enabled":enabled}),
                                    "Toggle mask step",
                                );
                            }
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
                            icons::paint(ui.painter(), rect, effect_icon(&step.kind), step.enabled);
                            let name = if step.kind == "blur" {
                                "Feather"
                            } else {
                                effect_name(&step.kind)
                            };
                            let title = Self::effect_title(
                                ui,
                                name,
                                self.effect_is_selected(&l.id, true, &step.id),
                            );
                            self.effect_bake_menu(&title, l, true, &step.id, index, doc.revision);
                            if title.clicked() {
                                self.select_effect(&l.id, true, &step.id);
                                self.mask_step = if step.kind == "paint" {
                                    Some(step.id.clone())
                                } else {
                                    None
                                };
                                self.last_preview = None;
                            }
                            self.effect_actions(ui, l, true, &step.id, index, doc.revision);
                            self.effect_weight(ui, &step.id, step.weight, true);
                        });
                        if self.effect_is_selected(&l.id, true, &step.id) {
                            if let Some(preview) = self.thumbnail_for(doc, &l.id, true, Some(index))
                            {
                                ui.add(egui::Image::new((preview.id(), Vec2::splat(40.0))))
                                    .on_hover_text("Result through this effect");
                            }
                            if ["fill", "levels", "blur"].contains(&step.kind.as_str()) {
                                let mut value = step.value;
                                let range = match step.kind.as_str() {
                                    "fill" => 0.0..=255.0,
                                    "blur" => 0.0..=64.0,
                                    _ => 0.1..=5.0,
                                };
                                ui.horizontal(|ui| {
                                    controls::label(
                                        ui,
                                        if step.kind == "fill" {
                                            "Value"
                                        } else if step.kind == "blur" {
                                            "Radius"
                                        } else {
                                            "Gamma"
                                        },
                                    );
                                    let response = controls::range(
                                        ui,
                                        (&step.id, "value"),
                                        &mut value,
                                        range,
                                        180.0,
                                        if step.kind == "blur" { " px" } else { "" },
                                        if step.kind == "levels" { 1 } else { 0 },
                                        false,
                                    );
                                    if response.changed() || response.double_clicked() {
                                        self.layer_parameter(
                                            json!({"step":step.id,"value":value}),
                                            "Mask parameter",
                                            &response,
                                        );
                                    }
                                });
                            }
                            if ["curves", "gaussian", "adjust"].contains(&step.kind.as_str()) {
                                let mut values = step.settings.clone();
                                if let Some(response) = settings(ui, &step.kind, &mut values) {
                                    self.layer_parameter(
                                        json!({"step":step.id,"settings":values}),
                                        "Mask parameter",
                                        &response,
                                    );
                                }
                            }
                        }
                    });
                }
            });
    }
    pub(super) fn color_stack(&mut self, ui: &mut egui::Ui, l: &Layer, revision: u64) {
        ui.horizontal(|ui| {
            Self::effect_heading(ui, false);
            let add = ui.menu_button("+ Add effect", |ui| {
                self.effect_picker(ui, false, l.kind == "adjustment");
            });
            self.effect_add_rect = Some(add.response.rect);
        });
        egui::ScrollArea::vertical()
            .id_salt("color effects")
            .max_height(
                (ui.available_height() - if l.mask.is_none() { 38.0 } else { 8.0 }).max(24.0),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                if l.effects.is_empty() {
                    ui.label(RichText::new("No color effects").size(11.).color(MUTED));
                }
                for (index, effect) in l.effects.iter().enumerate().rev() {
                    ui.push_id(&effect.id, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let mut enabled = effect.enabled;
                            if ui
                                .checkbox(&mut enabled, "")
                                .on_hover_text("Enable effect")
                                .changed()
                            {
                                self.layer_cmd(
                                    "effect.update",
                                    json!({"effect":effect.id,"enabled":enabled}),
                                    "Toggle color effect",
                                );
                            }
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
                            icons::paint(
                                ui.painter(),
                                rect,
                                effect_icon(&effect.kind),
                                effect.enabled,
                            );
                            let title = Self::effect_title(
                                ui,
                                effect_name(&effect.kind),
                                self.effect_is_selected(&l.id, false, &effect.id),
                            );
                            self.effect_bake_menu(&title, l, false, &effect.id, index, revision);
                            if title.clicked() {
                                self.select_effect(&l.id, false, &effect.id);
                            }
                            self.effect_actions(ui, l, false, &effect.id, index, revision);
                            self.effect_weight(ui, &effect.id, effect.weight, false);
                        });
                        if self.effect_is_selected(&l.id, false, &effect.id) {
                            if matches!(effect.kind.as_str(), "pattern_overlay" | "texture") {
                                pattern_source::control(self, ui, l, effect, revision);
                            }
                            if effect.kind == "liquify"
                                && ui
                                    .button("Edit on canvas")
                                    .on_hover_text("Paint an editable distortion")
                                    .clicked()
                            {
                                self.activate_liquify(Some(effect.id.clone()));
                            }
                            let mut values = effect.settings.clone();
                            if let Some(response) = settings(ui, &effect.kind, &mut values) {
                                self.layer_parameter(
                                    json!({"effect":effect.id,"settings":values}),
                                    "Effect parameter",
                                    &response,
                                );
                            }
                        }
                    });
                }
            });
    }
}
pub(super) fn effect_name(kind: &str) -> &str {
    effects::catalog::get(kind).map_or(kind, |entry| entry.name)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn menu_fixture(depth: u16, mask: bool) -> (PeerBrush, egui::Context, String, Document) {
        let ctx = egui::Context::default();
        let mut e = crate::engine::Engine::new();
        e.doc = Document::new_depth(32, 24, depth).unwrap();
        e.doc.layers[0]
            .pixels
            .set16(10, 12, [12345, 23457, 34569, 65535]);
        let layer = e.doc.layers[0].id.clone();
        let commands = if mask {
            vec![
                json!({"op":"mask.add","layer":layer,"value":128}),
                json!({"op":"mask.step.add","layer":layer,"kind":"invert"}),
                json!({"op":"mask.step.add","layer":layer,"kind":"levels","value":2.}),
            ]
        } else {
            vec![
                json!({"op":"effect.add","layer":layer,"kind":"invert"}),
                json!({"op":"effect.add","layer":layer,"kind":"adjust","settings":{"brightness":0.2}}),
            ]
        };
        e.edit("human", &commands, None, None, "Fixture").unwrap();
        e.undo.clear();
        let effect = if mask {
            e.doc.layers[0]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .iter()
                .find(|step| step.kind == "invert")
                .unwrap()
                .id
                .clone()
        } else {
            e.doc.layers[0].effects[0].id.clone()
        };
        let before = e.doc.clone();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
        let mut app = PeerBrush::init(
            &ctx,
            shared,
            crate::server::Connection {
                port: 0,
                token: String::new(),
                state_dir: Default::default(),
                instance_lock: None,
                availability: Default::default(),
            },
        );
        app.selected = layer.clone();
        app.selection_layers = [layer.clone()].into_iter().collect();
        app.mask = mask;
        app.effect_selected = Some((layer, mask, effect.clone()));
        (app, ctx, effect, before)
    }
    fn menu_frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        let doc = app.shared.lock().unwrap().doc.clone();
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(270., 800.))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    if app.mask {
                        app.mask_stack(ui, &doc, &doc.layers[0]);
                    } else {
                        app.color_stack(ui, &doc.layers[0], doc.revision);
                    }
                });
            },
        )
    }
    fn menu_label(output: &egui::FullOutput, label: &str) -> Pos2 {
        output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(text) if text.galley.job.text == label => {
                    Some(text.pos + Vec2::new(4., 4.))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("Missing {label}"))
    }
    fn menu_click(app: &mut PeerBrush, ctx: &egui::Context, at: Pos2) {
        for pressed in [true, false] {
            menu_frame(
                app,
                ctx,
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
    fn open_invert_menu(app: &mut PeerBrush, ctx: &egui::Context) -> egui::FullOutput {
        let output = menu_frame(app, ctx, vec![]);
        let title = menu_label(&output, "Invert");
        let at = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(text) if text.galley.job.text == "⋯" => {
                    Some(text.pos + Vec2::new(4., 4.))
                }
                _ => None,
            })
            .min_by(|a, b| (a.y - title.y).abs().total_cmp(&(b.y - title.y).abs()))
            .unwrap();
        assert!((at.y - title.y).abs() < 8., "Actions belong to their row");
        assert!(at.x < 252., "Actions must remain inside a compact stack");
        menu_click(app, ctx, at);
        menu_frame(app, ctx, vec![])
    }
    #[test]
    fn compact_color_and_mask_menus_reorder_real_native_results_once_and_keep_sources() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx, effect, before) = menu_fixture(depth, mask);
                let menu = open_invert_menu(&mut app, &ctx);
                menu_label(&menu, "Top effects run last");
                assert!(app.shared.lock().unwrap().undo.is_empty());
                let at = menu_label(&menu, "Move up · run later");
                menu_click(&mut app, &ctx, at);
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1, "{}", app.message);
                assert_ne!(
                    e.doc.export_png().unwrap(),
                    before.export_png().unwrap(),
                    "Order must change actual noncommuting pixels"
                );
                assert_eq!(
                    serde_json::to_value(&e.doc.layers[0].pixels).unwrap(),
                    serde_json::to_value(&before.layers[0].pixels).unwrap()
                );
                if mask {
                    let steps = &e.doc.layers[0].mask.as_ref().unwrap().steps;
                    assert_eq!(steps.last().unwrap().id, effect);
                    for original in &before.layers[0].mask.as_ref().unwrap().steps {
                        let current = steps.iter().find(|step| step.id == original.id).unwrap();
                        assert_eq!(
                            serde_json::to_value(&current.pixels).unwrap(),
                            serde_json::to_value(&original.pixels).unwrap()
                        );
                    }
                } else {
                    assert_eq!(e.doc.layers[0].effects.last().unwrap().id, effect);
                }
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
    #[test]
    fn guarded_effect_removal_and_bake_keep_the_active_inspector_and_native_work() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx, _effect, before) = menu_fixture(depth, mask);
                let layer = app.selected.clone();
                app.shared
                    .lock()
                    .unwrap()
                    .reserve(
                        "agent",
                        "Effect work",
                        vec![crate::engine::Scope::layer(&layer)],
                    )
                    .unwrap();
                let paint = before.layers[0]
                    .mask
                    .as_ref()
                    .and_then(|m| m.steps.iter().find(|step| step.kind == "paint"))
                    .map(|step| step.id.clone());
                if mask {
                    app.mask_step = paint.clone();
                    app.effect_selected = Some((layer.clone(), true, paint.clone().unwrap()));
                }
                let inspector = app.effect_selected.clone();
                for label in [
                    if mask {
                        "Remove mask effect"
                    } else {
                        "Remove effect"
                    },
                    if mask {
                        "Bake into mask pixels"
                    } else {
                        "Bake into layer pixels"
                    },
                ] {
                    let menu = open_invert_menu(&mut app, &ctx);
                    let at = menu_label(&menu, label);
                    menu_click(&mut app, &ctx, at);
                    assert_eq!(app.effect_selected, inspector);
                    if mask {
                        assert_eq!(app.mask_step, paint);
                    }
                }
                let e = app.shared.lock().unwrap();
                assert!(e.undo.is_empty());
                assert_eq!(
                    serde_json::to_value(&e.doc).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                assert_eq!(app.effect_selected, inspector);
                if mask {
                    assert_eq!(app.mask_step, paint);
                }
                assert!(
                    app.message.to_lowercase().contains("reserved"),
                    "{}",
                    app.message
                );
            }
        }
    }
    #[test]
    fn removing_an_unselected_effect_keeps_the_other_native_inspector() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx, _effect, before) = menu_fixture(depth, mask);
                let inspector = if mask {
                    before.layers[0].mask.as_ref().unwrap().steps[1].id.clone()
                } else {
                    before.layers[0].effects[1].id.clone()
                };
                app.effect_selected = Some((app.selected.clone(), mask, inspector.clone()));
                if mask {
                    app.mask_step = Some(inspector.clone());
                }
                let selected = app.effect_selected.clone();
                let menu = open_invert_menu(&mut app, &ctx);
                menu_click(
                    &mut app,
                    &ctx,
                    menu_label(
                        &menu,
                        if mask {
                            "Remove mask effect"
                        } else {
                            "Remove effect"
                        },
                    ),
                );
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1, "{}", app.message);
                assert_eq!(app.effect_selected, selected);
                if mask {
                    assert_eq!(app.mask_step.as_deref(), Some(inspector.as_str()));
                }
                assert_eq!(
                    serde_json::to_value(&e.doc.layers[0].pixels).unwrap(),
                    serde_json::to_value(&before.layers[0].pixels).unwrap()
                );
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
    #[test]
    fn right_click_native_color_and_mask_effects_bake_once_with_explicit_scope() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let ctx = egui::Context::default();
                let mut e = crate::engine::Engine::new();
                e.doc = Document::new_depth(32, 24, depth).unwrap();
                e.doc.layers[0]
                    .pixels
                    .set16(10, 12, [12345, 23457, 34569, 65535]);
                let id = e.doc.layers[0].id.clone();
                if mask {
                    e.edit(
                        "human",
                        &[json!({"op":"mask.add","layer":id,"value":255})],
                        None,
                        None,
                        "Mask",
                    )
                    .unwrap();
                }
                let c = if mask {
                    json!({"op":"mask.step.add","layer":id,"kind":"invert"})
                } else {
                    json!({"op":"effect.add","layer":id,"kind":"invert"})
                };
                e.edit("human", &[c], None, None, "Effect").unwrap();
                e.undo.clear();
                let before = e.doc.clone();
                let shared = std::sync::Arc::new(std::sync::Mutex::new(e));
                let connection = crate::server::Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: Default::default(),
                    instance_lock: None,
                    availability: Default::default(),
                };
                let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
                app.selected = id.clone();
                app.selection_layers = [id].into_iter().collect();
                app.mask = mask;
                let mut time = 0.;
                let mut draw = |app: &mut PeerBrush, events| {
                    time += 0.05;
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1200., 900.),
                            )),
                            events,
                            time: Some(time),
                            ..Default::default()
                        },
                        |ctx| app.draw(ctx),
                    )
                };
                draw(&mut app, vec![]);
                let output = draw(&mut app, vec![]);
                let position = |output: &egui::FullOutput, label: &str| {
                    output
                        .shapes
                        .iter()
                        .find_map(|s| match &s.shape {
                            egui::Shape::Text(t) if t.galley.job.text == label => {
                                Some(t.pos + Vec2::new(4., 4.))
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("Missing {label}"))
                };
                let at = position(&output, "Invert");
                let button = |pos, button, pressed| egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers: Default::default(),
                };
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(at),
                        button(at, egui::PointerButton::Secondary, true),
                    ],
                );
                draw(
                    &mut app,
                    vec![button(at, egui::PointerButton::Secondary, false)],
                );
                let menu = draw(&mut app, vec![]);
                assert!(menu.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text.starts_with("Includes this effect"))));
                assert!(shared.lock().unwrap().undo.is_empty());
                let at = position(
                    &menu,
                    if mask {
                        "Bake into mask pixels"
                    } else {
                        "Bake into layer pixels"
                    },
                );
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(at),
                        button(at, egui::PointerButton::Primary, true),
                    ],
                );
                draw(
                    &mut app,
                    vec![button(at, egui::PointerButton::Primary, false)],
                );
                let mut e = shared.lock().unwrap();
                assert_eq!(e.undo.len(), 1, "{}", app.message);
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
                if mask {
                    assert_eq!(e.doc.layers[0].mask.as_ref().unwrap().steps.len(), 1);
                } else {
                    assert!(e.doc.layers[0].effects.is_empty());
                }
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
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
            availability: Default::default(),
        };
        let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 600.))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.color_stack(ui, &layer, 1));
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
        for label in ["Color adjustment", "Invert"] {
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape,
                egui::Shape::Text(text) if text.galley.job.text == "100%"
                    && (text.pos.y-y(label)).abs() < 5.0)));
        }
        assert_eq!(
            output
                .shapes
                .iter()
                .filter(|s| matches!(&s.shape, egui::Shape::Text(t) if t.galley.job.text=="Weight"))
                .count(),
            0
        );
        assert!(!output
            .shapes
            .iter()
            .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Brightness")));
        app.select_effect(&id, false, &layer.effects[1].id);
        let selected = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 600.))),
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| app.color_stack(ui, &layer, 1));
            },
        );
        assert!(selected
            .shapes
            .iter()
            .any(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Brightness")));
        assert_eq!(
            selected
                .shapes
                .iter()
                .filter(|s| matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text=="Weight"))
                .count(),
            0
        );
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

fn channel_clamp_settings(ui: &mut egui::Ui, settings: &mut Value) -> Option<egui::Response> {
    let (channel, minimum, maximum) = effects::channel_clamp::parameters(settings).ok()?;
    let mut changed = None;
    ui.horizontal(|ui| {
        controls::label(ui, "Channel");
        for (index, (key, label, hint)) in [
            ("r", "R", "Red"),
            ("g", "G", "Green"),
            ("b", "B", "Blue"),
            ("o", "O", "Opacity / alpha"),
        ]
        .into_iter()
        .enumerate()
        {
            let mut response = ui
                .selectable_label(channel == index, label)
                .on_hover_text(hint);
            if response.clicked() && channel != index {
                settings["channel"] = json!(key);
                response.mark_changed();
                changed = Some(response);
            }
        }
    });
    egui::Grid::new((ui.id(), "clamp bounds"))
        .spacing(Vec2::new(8.0, 6.0))
        .show(ui, |ui| {
            for (key, label, original, limit) in [
                ("minimum", "Minimum", minimum, 0.0..=maximum as f32 * 100.0),
                (
                    "maximum",
                    "Maximum",
                    maximum,
                    minimum as f32 * 100.0..=100.0,
                ),
            ] {
                controls::label(ui, label);
                let mut value = original as f32 * 100.0;
                let response = controls::range(ui, key, &mut value, limit, 145.0, "%", 2, false);
                if response.changed() || response.double_clicked() {
                    let value = f64::from(value) / 100.0;
                    settings[key] = json!(if key == "minimum" {
                        value.min(maximum)
                    } else {
                        value.max(minimum)
                    });
                    changed = Some(response);
                }
                ui.end_row();
            }
        });
    changed
}

fn levels_slider(ui: &mut egui::Ui, settings: &mut Value) -> Option<egui::Response> {
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(240.0, 38.0), egui::Sense::click_and_drag());
    let bar = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 18.0));
    for i in 0..128 {
        let x = bar.left() + bar.width() * i as f32 / 128.0;
        ui.painter().rect_filled(
            Rect::from_min_max(
                egui::pos2(x, bar.top()),
                egui::pos2(x + bar.width() / 128.0 + 0.5, bar.bottom()),
            ),
            0,
            Color32::from_gray((i * 255 / 127) as u8),
        );
    }
    let mut black = effects::number(settings, "black", 0.0);
    let mut white = effects::number(settings, "white", 1.0);
    let mut gamma = effects::number(settings, "gamma", 1.0);
    let positions = [black, black + (white - black) * 0.5_f32.powf(gamma), white];
    let id = response.id.with("levels handle");
    if response.drag_started() || response.clicked() {
        if let Some(p) = ui
            .input(|i| i.pointer.press_origin())
            .or(response.interact_pointer_pos())
        {
            let nearest = (0..3)
                .min_by(|a, b| {
                    (bar.left() + positions[*a] * bar.width() - p.x)
                        .abs()
                        .total_cmp(&(bar.left() + positions[*b] * bar.width() - p.x).abs())
                })
                .unwrap();
            ui.ctx().data_mut(|d| d.insert_temp(id, nearest));
        }
    }
    if response.dragged() || response.clicked() {
        if let (Some(p), Some(index)) = (
            response.interact_pointer_pos(),
            ui.ctx().data(|d| d.get_temp::<usize>(id)),
        ) {
            let x = ((p.x - bar.left()) / bar.width()).clamp(0.0, 1.0);
            match index {
                0 => black = x.min(white - 0.01).clamp(0.0, 0.99),
                2 => white = x.max(black + 0.01).clamp(0.01, 1.0),
                _ => {
                    gamma = (((x - black) / (white - black)).clamp(0.001, 0.999).ln()
                        / 0.5_f32.ln())
                    .clamp(0.1, 5.0)
                }
            }
            settings["black"] = json!(black);
            settings["white"] = json!(white);
            settings["gamma"] = json!(gamma);
            response.mark_changed();
        }
    }
    for (i, v) in [black, black + (white - black) * 0.5_f32.powf(gamma), white]
        .into_iter()
        .enumerate()
    {
        let x = bar.left() + v * bar.width();
        ui.painter().add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x, bar.bottom() + 2.0),
                egui::pos2(x - 5.0, bar.bottom() + 11.0),
                egui::pos2(x + 5.0, bar.bottom() + 11.0),
            ],
            [Color32::BLACK, Color32::GRAY, Color32::WHITE][i],
            Stroke::new(1.0_f32, MUTED),
        ));
    }
    if response.drag_stopped() {
        ui.ctx().data_mut(|d| d.remove::<usize>(id));
    }
    let changed = response.changed();
    let response = response.on_hover_text("Drag the black, midtone and white input handles");
    changed.then_some(response)
}
