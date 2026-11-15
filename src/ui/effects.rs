use super::*;
use crate::{
    effects,
    engine::view::LayerView,
};
// Bitmap bytes are displayed/imported through the original Effect, never these controls.
struct InspectorSettings {
    value: Value,
    _charge: Option<Arc<crate::psd::memory36::MetadataCharge>>,
}
fn mask_inspector_settings(
    doc: DocumentView<'_>,
    step: crate::engine::view::MaskStepView<'_>,
) -> Result<InspectorSettings, String> {
    if let Some(native) = doc.native() {
        let admitted: crate::psd::memory36::Admitted<Value> =
            crate::engine::native::copy_admitted(step.settings(), native.budget(), true, &|| {
                Ok(())
            })?;
        let mut result = None;
        let moved = admitted.map_unique(|value, charge| {
            result = Some(InspectorSettings {
                value,
                _charge: Some(charge.clone()),
            });
            Ok(())
        })?;
        drop(moved);
        result.ok_or_else(|| "Mask inspector metadata could not be prepared".into())
    } else {
        Ok(InspectorSettings {
            value: step.settings().clone(),
            _charge: None,
        })
    }
}
impl InspectorSettings {
    fn restore_bitmap(
        &mut self,
        doc: DocumentView<'_>,
        effect: crate::engine::view::EffectView<'_>,
    ) -> Result<(), String> {
        if crate::effects::patterns::active(effect.kind(), &self.value) {
            if let Some(png) = effect.settings().get("bitmap_png") {
                if let Some(native) = doc.native() {
                    let bytes = png
                        .as_str()
                        .ok_or("Pattern bitmap is not encoded text")?
                        .len()
                        .checked_mul(2)
                        .and_then(|n| n.checked_add(1024))
                        .filter(|n| *n <= 64 * 1024 * 1024)
                        .ok_or("Pattern metadata exceeds admission")?;
                    let extra = crate::psd::memory36::MetadataCharge::reserve(
                        native.budget(),
                        bytes,
                        &|| Ok(()),
                    )?;
                    self._charge = Some(crate::psd::memory36::MetadataCharge::combine(
                        &[
                            self._charge
                                .as_ref()
                                .ok_or("Inspector admission is missing")?
                                .clone(),
                            extra,
                        ],
                        &|| Ok(()),
                    )?);
                }
                self.value["bitmap_png"] = png.clone();
            }
        }
        Ok(())
    }
}
fn inspector_settings(
    doc: DocumentView<'_>,
    effect: crate::engine::view::EffectView<'_>,
) -> Result<InspectorSettings, String> {
    // Admit the selected metadata before cloning; bitmap bytes stay in their source owner.
    struct Selected<'a> {
        kind: &'a str,
        value: &'a Value,
    }
    impl serde::Serialize for Selected<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            use serde::ser::SerializeMap;
            if let Some(fields) = self
                .value
                .as_object()
                .filter(|_| crate::effects::patterns::active(self.kind, self.value))
            {
                let mut map = serializer.serialize_map(None)?;
                for (key, value) in fields {
                    if key != "bitmap_png" {
                        map.serialize_entry(key, value)?;
                    }
                }
                map.end()
            } else {
                self.value.serialize(serializer)
            }
        }
    }
    if let Some(native) = doc.native() {
        let admitted: crate::psd::memory36::Admitted<Value> = crate::engine::native::copy_admitted(
            &Selected {
                kind: effect.kind(),
                value: effect.settings(),
            },
            native.budget(),
            true,
            &|| Ok(()),
        )?;
        let mut result = None;
        let moved = admitted.map_unique(|value, charge| {
            result = Some(InspectorSettings {
                value,
                _charge: Some(charge.clone()),
            });
            Ok(())
        })?;
        drop(moved);
        result.ok_or_else(|| "Inspector metadata could not be prepared".into())
    } else {
        Ok(InspectorSettings {
            value: inspector_values(effect.kind(), effect.settings()),
            _charge: None,
        })
    }
}
fn inspector_values(kind: &str, source: &Value) -> Value {
    match source
        .as_object()
        .filter(|_| crate::effects::patterns::active(kind, source))
    {
        Some(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != "bitmap_png")
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        ),
        None => source.clone(),
    }
}
pub(super) fn settings(
    ui: &mut egui::Ui,
    kind: &str,
    settings: &mut Value,
    depth: u16,
) -> Option<egui::Response> {
    if kind == effects::color_lookup::KIND {
        let (updated, response) = color_lookup_picker(ui, settings)?;
        *settings = updated;
        return Some(response);
    }
    if kind == effects::channel_mixer::KIND {
        return channel_mixer_settings(ui, settings, None);
    }
    if kind == effects::black_white::KIND {
        return black_white_settings(ui, settings, None);
    }
    if kind == effects::selective_color::KIND {
        return selective_color_settings(ui, settings, None);
    }
    if kind == effects::photo_filter::KIND {
        return photo_filter_settings(ui, settings, depth, None);
    }
    if kind == effects::gradient_map::KIND {
        return gradient_map_settings(ui, settings, None);
    }
    if kind == effects::photoshop_hue_saturation::KIND {
        return photoshop_hue_saturation_settings(ui, settings, None);
    }
    if !matches!(kind, "levels" | "curves") { return settings_values(ui, kind, settings, depth, false, None); }
    let byte_composition = effects::curves::byte_composition(kind, settings)
        || effects::levels::byte_composition(kind, settings);
    let selected_id = ui.id().with((kind, "channel"));
    let mut selected = ui.ctx().data(|data| data.get_temp::<usize>(selected_id)).unwrap_or(0).min(3);
    let _ = controls::swatch_choices(
        ui,
        "Channel",
        &mut selected,
        std::iter::once(("RGB", controls::RGB_COLORS.as_slice())).chain(controls::RGB_CHANNELS),
    );
    ui.ctx().data_mut(|data| data.insert_temp(selected_id, selected));
    ui.push_id((kind, "channel", selected), |ui| {
        if selected == 0 { return settings_values(ui, kind, settings, depth, byte_composition, None); }
        let identity = if kind == "curves" {
            json!({"points":[[0.,0.],[1.,1.]],"interpolation":"linear"})
        } else {
            effects::defaults(kind)
        };
        let mut channel = settings.get("channels").and_then(|values| values.get(selected - 1))
            .cloned().unwrap_or_else(|| identity.clone());
        if kind == "levels" { channel["input_mapping"] = json!("exact"); }
        let response = settings_values(ui, kind, &mut channel, depth, byte_composition, None);
        if response.is_some() {
            if settings.get("channels").is_none() {
                settings["channels"] = Value::Array(vec![identity; 3]);
            }
            settings["channels"][selected - 1] = channel;
        }
        response
    }).inner
}

fn posterize_choice(
    ui: &mut egui::Ui,
    mapping: &str,
    label: &str,
    help: &str,
    selected: bool,
    depth: u16,
) -> egui::Response {
    const WIDTH: usize = 64;
    let id = egui::Id::new(("Posterize ramp", mapping, depth));
    let texture = ui.ctx().data(|data| data.get_temp::<TextureHandle>(id))
        .unwrap_or_else(|| {
            // A four-level reference keeps the mapping comparison visible at any effect setting.
            let settings = if mapping.is_empty() { json!({}) } else { json!({"mapping": mapping}) };
            let quantizer = effects::posterize::Quantizer::new(&settings)
                .expect("Built-in Posterize mapping");
            let max = if depth == 16 { u16::MAX } else { 255 };
            let mut image = egui::ColorImage::new([WIDTH, 1], Color32::BLACK);
            for (index, pixel) in image.pixels.iter_mut().enumerate() {
                let input = (index as u32 * u32::from(max) / (WIDTH as u32 - 1)) as u16;
                let output = quantizer.sample(input, max);
                let shade = if depth == 16 { crate::raster::project16(output) } else { output as u8 };
                *pixel = Color32::from_gray(shade);
            }
            let texture = ui.ctx().load_texture(label, image, egui::TextureOptions::NEAREST);
            ui.ctx().data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });
    let response = ui.add(egui::Button::image_and_text((texture.id(), Vec2::new(48., 14.)), label)
        .selected(selected)).on_hover_text(help);
    response.widget_info(|| egui::WidgetInfo::selected(
        egui::WidgetType::RadioButton, response.enabled(), selected, label,
    ));
    icons::paint_focus(ui, &response);
    response
}

fn settings_values(
    ui: &mut egui::Ui,
    kind: &str,
    settings: &mut Value,
    depth: u16,
    byte_composition: bool,
    source: Option<&Value>,
) -> Option<egui::Response> {
    if crate::effects::styles::contains(kind) {
        return style_settings::settings(ui, kind, settings);
    }
    if kind == "channel_clamp" {
        return channel_clamp_settings(ui, settings);
    }
    let mut changed = None;
    let byte_levels = kind == "levels" && byte_composition;
    if kind == "levels" {
        changed = levels_slider(ui, settings, byte_levels);
    }
    let mut posterize_mapping = match effects::posterize::source_version(kind, settings) {
        Some(1) => "photoshop",
        Some(_) => effects::posterize::PHOTOSHOP_NATIVE,
        None => "",
    };
    if kind == "posterize" {
        let response = ui.horizontal_wrapped(|ui| {
            controls::label(ui, "Mapping");
            let show_legacy = posterize_mapping == "photoshop";
            let mut change = None;
            for (mapping, label, help) in [
                ("", "Nearest", "Round each channel to the nearest palette level. Swatch shows four levels."),
                (effects::posterize::PHOTOSHOP_NATIVE, "Photoshop bins", "Use Photoshop’s bucket and palette rules at the project’s depth. Swatch shows four levels."),
                ("photoshop", "Legacy bins", "Keep the mapping used by older PeerBrush projects. Swatch shows four levels."),
            ] {
                if mapping == "photoshop" && !show_legacy { continue; }
                let selected = mapping == posterize_mapping;
                let mut response = posterize_choice(ui, mapping, label, help, selected, depth);
                if response.clicked() && !selected {
                    posterize_mapping = mapping;
                    response.mark_changed();
                    change = Some(response);
                }
            }
            change
        }).inner;
        if let Some(response) = response {
            if !posterize_mapping.is_empty() {
                settings["mapping"] = json!(posterize_mapping);
                if settings["levels"].as_f64().is_some_and(|levels| levels > f64::from(effects::posterize::PHOTOSHOP_MAX_LEVELS)) {
                    settings["levels"] = json!(effects::posterize::PHOTOSHOP_MAX_LEVELS);
                }
            } else {
                settings.as_object_mut().unwrap().remove("mapping");
            }
            changed = Some(response);
        }
    }
    let integer_adjustment = matches!(kind, effects::legacy_brightness::KIND
        | effects::modern_brightness::KIND | effects::vibrance::KIND);
    let params: Vec<(&str, &str, f32, f32, &str)> = match kind {
        "posterize" => vec![("levels", "Levels", 2.0,
            if posterize_mapping.is_empty() { 256.0 } else { f32::from(effects::posterize::PHOTOSHOP_MAX_LEVELS) }, "")],
        "threshold" => vec![("level", "Level", 1.0, 255.0, "")],
        effects::modern_brightness::KIND => vec![
            ("brightness", "Brightness", -150.0, 150.0, ""),
            ("contrast", "Contrast", -50.0, 100.0, ""),
        ],
        effects::legacy_brightness::KIND => vec![
            ("brightness", "Brightness", -100.0, 100.0, ""),
            ("contrast", "Contrast", -100.0, 100.0, ""),
        ],
        effects::exposure::KIND => vec![
            ("exposure", "Exposure", -20.0, 20.0, " EV"),
            ("offset", "Offset", -0.5, 0.5, ""),
            ("gamma", "Gamma", 0.01, 9.99, ""),
        ],
        effects::vibrance::KIND => vec![
            ("vibrance", "Vibrance", -100.0, 100.0, "%"),
            ("saturation", "Saturation", -100.0, 100.0, "%"),
        ],
        "blur" | "gaussian" => vec![("radius", "Radius", 0.0, 64.0, " px")],
        "levels" => vec![
            ("black", "Black", 0.0, if byte_levels { 253./255. } else if effects::levels::exact(settings) { 1. - 1./65535. } else { 0.99 }, ""),
            ("white", "White", if byte_levels { 2./255. } else if effects::levels::exact(settings) { 1./65535. } else { 0.01 }, 1.0, ""),
            ("gamma", "Gamma", 0.1, 9.99, ""),
            ("output_black", "Output black", 0.0, 1.0, ""),
            ("output_white", "Output white", 0.0, 1.0, ""),
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
        .spacing(Vec2::new(6.0, 3.0))
        .show(ui, |ui| {
            for (key, label, min, max, suffix) in params {
                controls::label(ui, label);
                let defaults = effects::defaults(kind);
                let default = defaults[key]
                    .as_f64()
                    .map(|x| x as f32)
                    .unwrap_or(if kind == "gaussian" { 8.0 } else { 0.0 });
                let byte_endpoint = byte_levels && key != "gamma";
                let scale = if byte_endpoint { 255. } else if suffix == "%" && !integer_adjustment { 100.0 } else { 1.0 };
                let mut value = effects::number(settings, key, default) * scale;
                let width = 160.0_f32.min(ui.available_width()).max(70.);
                let response = if kind == "hsl" && key == "hue" {
                    hue_range(ui, key, &mut value, width, false)
                } else {
                    let decimals = if byte_endpoint || integer_adjustment {
                        0
                    } else if kind == effects::exposure::KIND && key == "offset" {
                        4
                    } else if kind == "levels" && effects::levels::exact(settings) && key != "gamma" {
                        5
                    } else if scale == 100.0 || ["radius", "spread", "levels", "level"].contains(&key) {
                        0
                    } else {
                        2
                    };
                    controls::range(ui, key, &mut value, min * scale..=max * scale, width, suffix, decimals, false)
                };
                if response.changed() || response.double_clicked() {
                    if let Some(source) = source {
                        settings.clone_from(source);
                    }
                    if source.is_none() || response.changed() {
                        settings[key] = if integer_adjustment {
                            json!(value.round() as i16)
                        } else if matches!(key, "level" | "levels") {
                            json!(value.round() as u16)
                        } else if byte_endpoint {
                            json!(f64::from(value.round()) / 255.)
                        } else {
                            json!(value / scale)
                        };
                    }
                    changed = Some(response);
                }
                ui.end_row();
            }
        });
    if matches!(kind, "color_balance" | effects::photoshop_color_balance::KIND) {
        let photoshop = kind == effects::photoshop_color_balance::KIND;
        let scale = if photoshop { 1. } else { 100. };
        let band_id = ui.id().with("color balance band");
        let mut selected = ui
            .ctx()
            .data(|d| d.get_temp::<usize>(band_id))
            .unwrap_or(1)
            .min(2);
        const TONES: [(&str, &[Color32]); 3] = [
            ("Shadows", &[Color32::from_gray(39)]),
            ("Midtones", &[Color32::GRAY]),
            ("Highlights", &[Color32::from_gray(230)]),
        ];
        let _ = controls::swatch_choices(ui, "Tone", &mut selected, TONES);
        ui.ctx().data_mut(|d| d.insert_temp(band_id, selected));
        let band = effects::photoshop_color_balance::BANDS[selected];
        let mut values: [f32; 3] =
            std::array::from_fn(|i| source.unwrap_or(settings)[band][i].as_f64().unwrap_or(0.0) as f32);
        egui::Grid::new((ui.id(), "balance axes"))
            .spacing(Vec2::new(6.0, 3.0))
            .show(ui, |ui| {
                const NEUTRAL: Color32 = Color32::from_gray(98);
                for (i, (label, colors)) in [
                    ("Cyan ↔ Red", [Color32::from_rgb(78, 213, 223), NEUTRAL, controls::RGB_COLORS[0]]),
                    ("Magenta ↔ Green", [Color32::from_rgb(222, 115, 203), NEUTRAL, controls::RGB_COLORS[1]]),
                    ("Yellow ↔ Blue", [Color32::from_rgb(246, 215, 106), NEUTRAL, controls::RGB_COLORS[2]]),
                ]
                    .into_iter()
                    .enumerate()
                {
                    controls::label(ui, label);
                    let mut value = settings[band][i].as_f64().map_or(values[i], |value| value as f32) * scale;
                    let response = controls::color_range(
                        ui,
                        (band, i),
                        &mut value,
                        -100.0..=100.0,
                        160.0,
                        "",
                        label,
                        &colors,
                    );
                    if response.changed() || response.double_clicked() {
                        if let Some(source) = source { settings.clone_from(source); }
                        if response.changed() || source.is_none() {
                            values[i] = value / scale;
                            settings[band] = if photoshop { json!(values.map(|value| value.round() as i16)) }
                                else { json!(values) };
                        }
                        changed = Some(response);
                    }
                    ui.end_row();
                }
            });
        let mut preserve = settings["preserve_luminosity"].as_bool().unwrap_or(true);
        let response = ui.checkbox(&mut preserve, "Preserve luminosity");
        if response.changed() || response.double_clicked() {
            if let Some(source) = source { settings.clone_from(source); }
            if response.changed() || source.is_none() {
                settings["preserve_luminosity"] = json!(preserve);
            }
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
                                .spacing(Vec2::new(6.0, 3.0))
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
        let photoshop = byte_composition || settings["interpolation"] == effects::curves::PHOTOSHOP;
        let spline = effects::curves::Spline::from_settings(settings);
        let line = (0..=256)
            .map(|i| {
                let x = i as f32 / 256.0;
                let y = spline.as_ref().map_or_else(|| effects::curve_value(settings, x), |curve| curve.value(f64::from(x)) as f32);
                to_screen(x, y)
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
                let mut x = f64::from(((p.x - rect.left()) / rect.width()).clamp(0.001, 0.999));
                let mut y = f64::from(((rect.bottom() - p.y) / rect.height()).clamp(0.0, 1.0));
                if photoshop {
                    x = (x * 255.).round() / 255.;
                    y = (y * 255.).round() / 255.;
                }
                if points.len() < effects::curves::MAX_POINTS
                    && points
                        .iter()
                        .all(|p| (p[0].as_f64().unwrap() - x).abs() > 0.001)
                {
                    let mut updated = points.clone();
                    updated.push(json!([x, y]));
                    updated
                        .sort_by(|a, b| a[0].as_f64().unwrap().total_cmp(&b[0].as_f64().unwrap()));
                    settings["interpolation"] = json!(if photoshop { effects::curves::PHOTOSHOP } else { "smooth" });
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
                    if photoshop && updated.len() == 2 {
                        settings["interpolation"] = json!("linear");
                    } else if !photoshop {
                        settings["interpolation"] = json!("smooth");
                    }
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
                    let linear_pair = points.len() == 2 && settings["interpolation"] == "linear";
                    let record_point = if linear_pair || photoshop {
                        drag_standard_curve_point(&points, index, x as f64, y as f64)
                    } else { None };
                    let selected_x = if index == 0 || index + 1 == points.len() {
                        points[index][0].as_f64().unwrap()
                    } else {
                        bounded_curve_x(
                            x as f64,
                            points[index - 1][0].as_f64().unwrap(),
                            points[index + 1][0].as_f64().unwrap(),
                            points[index][0].as_f64().unwrap(),
                        )
                    };
                    if !linear_pair && !photoshop {
                        settings["interpolation"] = json!("smooth");
                    }
                    settings["points"][index] = json!(record_point.unwrap_or([selected_x, y as f64]));
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

// Keep a standard curve on its coordinate grid while dragging.
fn drag_standard_curve_point(points: &[serde_json::Value], index: usize, x: f64, y: f64) -> Option<[f64; 2]> {
    let sample = effects::curves::grid_sample;
    let current = points.get(index)?;
    let current_x = sample(&current[0])?;
    sample(&current[1])?;
    let left = if index > 0 { sample(&points[index - 1][0])? + 1 } else { 0 };
    let right = if index + 1 < points.len() { sample(&points[index + 1][0])?.checked_sub(1)? } else { 255 };
    if left > current_x || current_x > right { return None; }
    if points.len() == 2 {
        sample(&points[1 - index][1])?;
    }
    let x = ((x * 255.).round() as u16).clamp(left, right);
    Some([f64::from(x) / 255., (y * 255.).round() / 255.])
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
        "levels" | "posterize" | "threshold" | "channel_clamp" | effects::legacy_brightness::KIND
        | effects::modern_brightness::KIND | effects::exposure::KIND => Icon::Levels,
        "curves" | effects::gradient_map::KIND => Icon::Curves,
        "blur" | "gaussian" => Icon::Blur,
        "adjust" => Icon::Adjust,
        "color_balance" | effects::channel_mixer::KIND | effects::black_white::KIND
        | effects::selective_color::KIND | effects::photoshop_color_balance::KIND => Icon::ColorBalance,
        "hsl" | effects::vibrance::KIND | effects::photo_filter::KIND
        | effects::photoshop_hue_saturation::KIND | effects::color_lookup::KIND => Icon::Hue,
        "bloom" => Icon::Bloom,
        "liquify" => Icon::Liquify,
        "invert" => Icon::Invert,
        "grayscale" => Icon::Grayscale,
        _ => Icon::Adjust,
    }
}
pub(super) fn menu_effect(ui: &mut egui::Ui, kind: &str, label: &str, selection: Option<bool>) -> egui::Response {
    icons::menu_choice(ui, effect_icon(kind), label, selection)
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
                        if menu_effect(ui, entry.kind, entry.name(mask), None).clicked() {
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
        self.finish_parameter(true);
        let key = (layer.to_string(), mask, effect.to_string());
        if self.effect_is_expanded(layer, mask, effect) {
            self.effect_expanded.remove(&key);
            if self.effect_is_selected(layer, mask, effect) {
                self.effect_selected = None;
            }
        } else {
            if let Some(selected) = &self.effect_selected {
                self.effect_expanded.insert(selected.clone());
            }
            self.effect_expanded.insert(key.clone());
            self.effect_selected = Some(key);
        }
    }
    pub(super) fn effect_is_expanded(&self, layer: &str, mask: bool, effect: &str) -> bool {
        self.effect_is_selected(layer, mask, effect)
            || self
                .effect_expanded
                .contains(&(layer.to_string(), mask, effect.to_string()))
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
            egui::Button::new(RichText::new(name).size(12.5).strong())
                .selected(selected)
                .frame(false)
                .truncate(),
        )
        .on_hover_text(format!(
            "{name} · Expand or collapse settings · Other effects can stay open · right-click for actions"
        ))
    }
    pub(super) fn effect_bake_menu<'a>(
        &mut self,
        response: &egui::Response,
        l: impl Into<LayerView<'a>>,
        mask: bool,
        effect: &str,
        index: usize,
        revision: u64,
    ) {
        let l = l.into();
        response.context_menu(|ui| self.effect_options(ui, l, mask, effect, index, revision));
    }
    pub(super) fn effect_actions<'a>(
        &mut self,
        ui: &mut egui::Ui,
        l: impl Into<LayerView<'a>>,
        mask: bool,
        effect: &str,
        index: usize,
        revision: u64,
    ) -> egui::Response {
        let l = l.into();
        let menu =
            egui::menu::menu_custom_button(ui, egui::Button::new("...").frame(false), |ui| {
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
        let engine = self.shared.lock().unwrap();
        crate::engine::view::current(&engine)
            .find_layer(layer)
            .is_some_and(|entry| {
                if mask {
                    entry.mask().is_some_and(|mask| {
                        (0..mask.step_count()).any(|i| mask.step(i).unwrap().id() == effect)
                    })
                } else {
                    (0..entry.effect_count()).any(|i| entry.effect(i).unwrap().id() == effect)
                }
            })
    }
    fn effect_options<'a>(
        &mut self,
        ui: &mut egui::Ui,
        l: impl Into<LayerView<'a>>,
        mask: bool,
        effect: &str,
        index: usize,
        revision: u64,
    ) {
        let l = l.into();
        let count = if mask {
            l.mask().map_or(0, |m| m.step_count())
        } else {
            l.effect_count()
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
            let removed = !self.effect_present(l.id(), mask, effect);
            if removed {
                self.effect_expanded
                    .remove(&(l.id().to_owned(), mask, effect.to_string()));
                if self.effect_is_selected(l.id(), mask, effect) {
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
        } else if l.kind() == "group" {
            "folder pixels (flattens children)"
        } else if l.source_kind().is_some() {
            "pixels (rasterizes text/vector)"
        } else {
            "layer pixels"
        };
        let label = format!("Bake into {scope}");
        ui.label(format!(
            "Includes this effect and {index} preceding {}",
            if index == 1 { "effect" } else { "effects" }
        ));
        let enabled = l.kind() != "adjustment" || mask;
        if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
            let command = if mask {
                json!({"op":"mask.step.bake","layer":l.id(),"step":effect,"source_revision":revision})
            } else {
                json!({"op":"effect.bake","layer":l.id(),"effect":effect,"source_revision":revision})
            };
            self.edit(vec![command], "Bake effect and preceding effects");
            if !self.effect_present(l.id(), mask, effect) {
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
    pub(super) fn mask_stack<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        l: impl Into<LayerView<'a>>,
    ) {
        let doc = doc.into();
        let l = l.into();
        let numbers = self.animation.controls(doc, ui.input(|i| i.time));
        let Some(m) = l.mask() else {
            return;
        };
        ui.horizontal(|ui| {
            Self::effect_heading(ui, true);
            let add = ui.menu_button("+ Add effect", |ui| {
                self.effect_picker(ui, true, l.kind() == "adjustment")
            });
            self.effect_add_rect = Some(add.response.rect);
            let mut enabled = m.enabled();
            if effect_visibility(ui, &mut enabled, "Layer mask").changed() {
                self.layer_cmd("mask.toggle", json!({"enabled":enabled}), "Toggle mask");
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("mask effects")
            .max_height((ui.available_height() - 8.0).max(24.0))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                for index in (0..m.step_count()).rev() {
                    let step = m.step(index).unwrap();
                    ui.push_id(&step.id(), |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let kind = step.kind();
                            let name = if kind == "blur" { "Feather" } else { effect_name(kind) };
                            let mut enabled = step.enabled();
                            if effect_visibility(ui, &mut enabled, name).changed() {
                                self.layer_cmd(
                                    "mask.step.update",
                                    json!({"step":step.id(),"enabled":enabled}),
                                    "Toggle mask step",
                                );
                            }
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
                            icons::paint(
                                ui.painter(),
                                rect,
                                effect_icon(kind),
                                step.enabled(),
                            );
                            let title = Self::effect_title(
                                ui,
                                name,
                                self.effect_is_expanded(&l.id(), true, &step.id()),
                            );
                            self.effect_bake_menu(
                                &title,
                                l,
                                true,
                                &step.id(),
                                index,
                                doc.revision(),
                            );
                            if title.clicked() {
                                self.select_effect(&l.id(), true, &step.id());
                                self.mask_step = if kind == "paint" {
                                    Some(step.id().to_owned())
                                } else {
                                    None
                                };
                                self.last_preview = None;
                            }
                            self.effect_actions(ui, l, true, &step.id(), index, doc.revision());
                            self.effect_weight(
                                ui,
                                &step.id(),
                                numbers.mask_weight(&l.id(), step),
                                true,
                            );
                        });
                        if self.effect_is_expanded(&l.id(), true, &step.id()) {
                            ui.spacing_mut().indent = 14.;
                            ui.visuals_mut().indent_has_left_vline = false;
                            ui.indent(("effect parameters", &step.id()), |ui| {
                                ui.spacing_mut().item_spacing.y = 3.;
                                let thumbnail = if let Some(native) = doc.native() {
                                    match self.mask_thumbnails.get(
                                        &self.shared,
                                        &self.project_id,
                                        native,
                                        l,
                                        index,
                                        ui.ctx(),
                                    ) {
                                        Ok(texture) => texture,
                                        Err(error) => {
                                            ui.label(RichText::new(error).color(MUTED));
                                            None
                                        }
                                    }
                                } else {
                                    self.thumbnail_for(
                                        ui.ctx(),
                                        doc.resident().unwrap(),
                                        l.id(),
                                        true,
                                        Some(index),
                                    )
                                };
                                if let Some(preview) = thumbnail {
                                    ui.add(egui::Image::new((preview.id(), Vec2::splat(40.0))))
                                        .on_hover_text("Result through this effect");
                                }
                                if ["fill", "levels", "blur", "density"].contains(&step.kind()) {
                                    let mut value = numbers.mask_value(&l.id(), step);
                                    if step.kind() == "density" {
                                        value *= 100. / 255.;
                                    }
                                    let range = match step.kind() {
                                        "fill" => 0.0..=255.0,
                                        "density" => 0.0..=100.0,
                                        "blur" => 0.0..=64.0,
                                        _ => 0.1..=5.0,
                                    };
                                    ui.horizontal(|ui| {
                                        controls::label(
                                            ui,
                                            match step.kind() {
                                                "fill" => "Value",
                                                "blur" => "Radius",
                                                "density" => "Density",
                                                _ => "Gamma",
                                            },
                                        );
                                        let response = controls::range(
                                            ui,
                                            (&step.id(), "value"),
                                            &mut value,
                                            range,
                                            180.0_f32.min(ui.available_width()).max(70.),
                                            match step.kind() { "blur" => " px", "density" => "%", _ => "" },
                                            usize::from(matches!(step.kind(), "levels" | "density")),
                                            false,
                                        );
                                        if response.changed() || response.double_clicked() {
                                            if step.kind() == "density" {
                                                value = (value * 255. / 100.).round();
                                            }
                                            self.layer_parameter(
                                                json!({"step":step.id(),"value":value}),
                                                "Mask parameter",
                                                &response,
                                            );
                                        }
                                    });
                                }
                                if ["curves", "gaussian", "adjust"].contains(&step.kind()) {
                                    let mut values = match mask_inspector_settings(doc, step) {
                                        Ok(values) => values,
                                        Err(error) => {
                                            ui.label(RichText::new(error).color(MUTED));
                                            return;
                                        }
                                    };
                                    numbers.mask_settings(&l.id(), step, &mut values.value);
                                    if let Some(response) =
                                        settings_values(ui, step.kind(), &mut values.value, doc.depth(), false, None)
                                    {
                                        self.layer_parameter(
                                            json!({"step":step.id(),"settings":values.value}),
                                            "Mask parameter",
                                            &response,
                                        );
                                    }
                                }
                            });
                            ui.add_space(5.);
                        }
                    });
                }
            });
    }
    pub(super) fn color_stack<'a>(
        &mut self,
        ui: &mut egui::Ui,
        l: impl Into<LayerView<'a>>,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let l = l.into();
        let doc = doc.into();
        let observed = (doc.id(), doc.revision());
        let revision = doc.revision();
        let numbers = self.animation.controls(doc, ui.input(|i| i.time));
        ui.horizontal(|ui| {
            Self::effect_heading(ui, false);
            let add = ui.menu_button("+ Add effect", |ui| {
                self.effect_picker(ui, false, l.kind() == "adjustment");
            });
            self.effect_add_rect = Some(add.response.rect);
        });
        egui::ScrollArea::vertical()
            .id_salt("color effects")
            .max_height(
                (ui.available_height() - if l.mask().is_none() { 38.0 } else { 8.0 }).max(24.0),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                if l.effect_count() == 0 {
                    ui.label(RichText::new("No color effects").size(11.).color(MUTED));
                }
                for index in (0..l.effect_count()).rev() {
                    let effect = l.effect(index).unwrap();
                    ui.push_id(&effect.id(), |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let kind = effect.kind();
                            let name = effect_name(kind);
                            let mut enabled = effect.enabled();
                            if effect_visibility(ui, &mut enabled, name).changed() {
                                self.layer_cmd(
                                    "effect.update",
                                    json!({"effect":effect.id(),"enabled":enabled}),
                                    "Toggle color effect",
                                );
                            }
                            let (rect, _) =
                                ui.allocate_exact_size(Vec2::splat(18.0), egui::Sense::hover());
                            icons::paint(
                                ui.painter(),
                                rect,
                                effect_icon(kind),
                                effect.enabled(),
                            );
                            let title = Self::effect_title(
                                ui,
                                name,
                                self.effect_is_expanded(&l.id(), false, &effect.id()),
                            );
                            self.effect_bake_menu(&title, l, false, &effect.id(), index, revision);
                            if title.clicked() {
                                self.select_effect(&l.id(), false, &effect.id());
                            }
                            self.effect_actions(ui, l, false, &effect.id(), index, revision);
                            self.effect_weight(
                                ui,
                                &effect.id(),
                                numbers.effect_weight(Some(&l.id()), effect),
                                false,
                            );
                        });
                        if self.effect_is_expanded(&l.id(), false, &effect.id()) {
                            ui.spacing_mut().indent = 14.;
                            ui.visuals_mut().indent_has_left_vline = false;
                            ui.indent(("effect parameters", &effect.id()), |ui| {
                                ui.spacing_mut().item_spacing.y = 3.;
                                if effects::styles::shadow_kind(effect.kind()) {
                                    let angle = doc.global_light_angle().map(u32::to_be_bytes);
                                    let current = effects::styles::GlobalLight::for_document(effect.settings(), angle).is_some();
                                    let choice = ui.horizontal_wrapped(|ui| {
                                        controls::label(ui, "Shadow light");
                                        let mut choice = None;
                                        for (linked, icon, label, help) in [
                                            (false, Icon::SingleLayer, "Local light", "Give this shadow its own direction. Switching to local light keeps the current direction."),
                                            (true, Icon::Connect, "Global Light", if angle.is_some() {
                                                "Follow the project’s Global Light."
                                            } else {
                                                "Create Global Light from this shadow’s current direction."
                                            }),
                                        ] {
                                            let selected = current == linked;
                                            if icons::labeled_choice(ui, icon, label, Some(selected), 0.)
                                                .on_hover_text(help).clicked() && !selected
                                            {
                                                choice = Some(linked);
                                            }
                                        }
                                        choice
                                    }).inner;
                                    if let Some(linked) = choice {
                                        self.finish_parameter(false);
                                        self.apply_action(
                                            vec![json!({"op":"effect.update","layer":l.id(),"effect":effect.id(),"use_global_light":linked})],
                                            "Change shadow light link",
                                            Some(observed),
                                            edit_job::Origin::Action,
                                        );
                                        return;
                                    }
                                }
                                if effect.kind() == effects::color_lookup::KIND {
                                    if let Some((settings, _)) = color_lookup_picker(ui, effect.settings()) {
                                        self.finish_parameter(false);
                                        self.apply_action(
                                            vec![json!({"op":"effect.update","layer":l.id(),"effect":effect.id(),"settings":settings})],
                                            "Edit Color Lookup",
                                            Some(observed),
                                            edit_job::Origin::Action,
                                        );
                                    }
                                    return;
                                }
                                if matches!(effect.kind(), "pattern_overlay" | "texture") {
                                    pattern_source::control(self, ui, l, effect, observed);
                                }
                                if effect.kind() == "liquify"
                                    && ui
                                        .button("Edit on canvas")
                                        .on_hover_text("Paint an editable distortion")
                                        .clicked()
                                {
                                    self.activate_liquify(Some(effect.id().to_owned()));
                                }
                                let mut values = match inspector_settings(doc, effect) {
                                    Ok(values) => values,
                                    Err(error) => {
                                        ui.label(RichText::new(error).color(MUTED));
                                        return;
                                    }
                                };
                                numbers.effect_settings(Some(&l.id()), effect, &mut values.value);
                                let document_id = doc.id().to_owned();
                                let response = ui
                                    .push_id(("settings source", document_id), |ui| {
                                        match effect.kind() {
                                            effects::channel_mixer::KIND =>
                                                channel_mixer_settings(ui, &mut values.value, Some(effect.settings())),
                                            effects::black_white::KIND =>
                                                black_white_settings(ui, &mut values.value, Some(effect.settings())),
                                            effects::selective_color::KIND =>
                                                selective_color_settings(ui, &mut values.value, Some(effect.settings())),
                                            effects::photo_filter::KIND =>
                                                photo_filter_settings(ui, &mut values.value, doc.depth(), Some(effect.settings())),
                                            effects::gradient_map::KIND =>
                                                gradient_map_settings(ui, &mut values.value, Some(effect.settings())),
                                            effects::photoshop_hue_saturation::KIND =>
                                                photoshop_hue_saturation_settings(ui, &mut values.value, Some(effect.settings())),
                                            kind @ (effects::legacy_brightness::KIND | effects::modern_brightness::KIND
                                                | effects::vibrance::KIND | effects::photoshop_color_balance::KIND) =>
                                                settings_values(ui, kind, &mut values.value, doc.depth(), false, Some(effect.settings())),
                                            kind => settings(ui, kind, &mut values.value, doc.depth()),
                                        }
                                    })
                                    .inner;
                                if let Some(response) = response {
                                    if let Err(error) = values.restore_bitmap(doc, effect) {
                                        self.message = error.into();
                                        return;
                                    }
                                    self.layer_parameter(
                                        json!({"effect":effect.id(),"settings":values.value}),
                                        "Effect parameter",
                                        &response,
                                    );
                                }
                            });
                            ui.add_space(5.);
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
    #[test]
    fn native_color_inspector_admits_selected_fields_and_preserves_bitmap_sources() {
        use crate::engine::{
            native::{worker, NativeDocument},
            scene::{CapturedScene, LegacyTileCache},
        };
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        for depth in [8, 16] {
            let fixture = worker::tests::Fixture::new();
            let (runtime, connection) = fixture.runtime();
            let pool = runtime.resident();
            let cache = LegacyTileCache::new(&pool, 32).unwrap();
            let mut doc = Document::new_depth(12, 8, depth).unwrap();
            let png = STANDARD.encode(crate::raster::png(1, 1, &[31, 73, 149, 255]).unwrap());
            doc.layers[0].effects.push(crate::effects::Effect {
                id: "pattern".into(),
                kind: "texture".into(),
                enabled: true,
                weight: 0.75,
                settings: Arc::new(json!({"pattern":"bitmap","bitmap_png":png,"bitmap_scale":2.})),
            });
            let staged =
                CapturedScene::from_parts(doc.clone(), "inspector".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(()))
                    .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let view = DocumentView::from(&native);
            let layer = view.layer(0).unwrap();
            let effect = layer.effect(0).unwrap();
            let original = serde_json::to_value(native.metadata()).unwrap();
            let used = pool.used();
            let mut settings = inspector_settings(view, effect).unwrap();
            assert!(settings.value.get("bitmap_png").is_none());
            assert!(pool.used() > used);
            settings.restore_bitmap(view, effect).unwrap();
            assert_eq!(settings.value["bitmap_png"], png);
            drop(settings);
            assert_eq!(pool.used(), used);
            let available = pool.limit() - pool.used();
            let held = pool.reserve_bytes(available, available).unwrap();
            assert!(inspector_settings(view, effect).is_err());
            drop(held);
            let (mut app, ctx) = super::super::workspace_tests::fixture(depth);
            app.select_effect(layer.id(), false, effect.id());
            let output = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500., 800.))),
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| app.color_stack(ui, layer, view));
                },
            );
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("Embedded bitmap PNG"))));
            assert_eq!(pool.used(), used);
            assert_eq!(serde_json::to_value(native.metadata()).unwrap(), original);
            drop(connection);
        }
    }
    fn menu_fixture(depth: u16, mask: bool) -> (PeerBrush, egui::Context, String, Document) {
        let ctx = egui::Context::default();
        let mut e = crate::engine::Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
        e.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(10, 12, [12345, 23457, 34569, 65535]);
        let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
        e.resident_state_mut().expect("resident fixture").undo.clear();
        let effect = if mask {
            e.resident_state().expect("resident fixture").document.layers[0]
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
            e.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone()
        };
        let before = e.resident_state().expect("resident fixture").document.clone();
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
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
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
                        app.color_stack(ui, &doc.layers[0], &doc);
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
    #[test]
    fn expanded_color_and_mask_settings_keep_independent_owners_without_document_edits() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let (mut app, ctx, _, _) = menu_fixture(depth, mask);
                let layer = app.selected.clone();
                app.shared.lock().unwrap().edit("human", &[if mask {
                    json!({"op":"mask.step.add","layer":layer,"kind":"blur","value":3.})
                } else {
                    json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":3.}})
                }], None, None, "Fixture").unwrap();
                app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").undo.clear();
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let (first, second) = if mask {
                    let steps = &before.layers[0].mask.as_ref().unwrap().steps;
                    (
                        steps
                            .iter()
                            .find(|step| step.kind == "levels")
                            .unwrap()
                            .id
                            .clone(),
                        steps
                            .iter()
                            .find(|step| step.kind == "blur")
                            .unwrap()
                            .id
                            .clone(),
                    )
                } else {
                    (
                        before.layers[0]
                            .effects
                            .iter()
                            .find(|effect| effect.kind == "adjust")
                            .unwrap()
                            .id
                            .clone(),
                        before.layers[0]
                            .effects
                            .iter()
                            .find(|effect| effect.kind == "blur")
                            .unwrap()
                            .id
                            .clone(),
                    )
                };
                menu_frame(&mut app, &ctx, vec![]);
                let output = menu_frame(&mut app, &ctx, vec![]);
                let at = menu_label(
                    &output,
                    if mask {
                        "Levels"
                    } else {
                        effect_name("adjust")
                    },
                );
                menu_click(&mut app, &ctx, at);
                let output = menu_frame(&mut app, &ctx, vec![]);
                let at = menu_label(&output, if mask { "Feather" } else { effect_name("blur") });
                menu_click(&mut app, &ctx, at);
                let output = menu_frame(&mut app, &ctx, vec![]);
                menu_label(&output, if mask { "Gamma" } else { "Brightness" });
                menu_label(&output, "Radius");
                assert!(app.effect_is_expanded(&layer, mask, &first));
                assert!(app.effect_is_expanded(&layer, mask, &second));
                assert!(app.effect_is_selected(&layer, mask, &second));
                // Closing another owner's settings keeps the active inspector and target.
                let at = menu_label(
                    &output,
                    if mask {
                        "Levels"
                    } else {
                        effect_name("adjust")
                    },
                );
                menu_click(&mut app, &ctx, at);
                menu_frame(&mut app, &ctx, vec![]);
                assert!(!app.effect_is_expanded(&layer, mask, &first));
                assert!(app.effect_is_expanded(&layer, mask, &second));
                assert!(app.effect_is_selected(&layer, mask, &second));
                let engine = app.shared.lock().unwrap();
                assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
            }
        }
    }
    fn open_invert_menu(app: &mut PeerBrush, ctx: &egui::Context) -> egui::FullOutput {
        let output = menu_frame(app, ctx, vec![]);
        let title = menu_label(&output, "Invert");
        let at = output
            .shapes
            .iter()
            .filter_map(|s| match &s.shape {
                egui::Shape::Text(text) if text.galley.job.text == "..." => {
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
                assert!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                let at = menu_label(&menu, "Move up · run later");
                menu_click(&mut app, &ctx, at);
                let mut e = app.shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                assert_ne!(
                    e.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap(),
                    "Order must change actual noncommuting pixels"
                );
                assert_eq!(
                    serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers[0].pixels).unwrap(),
                    serde_json::to_value(&before.layers[0].pixels).unwrap()
                );
                if mask {
                    let steps = &e.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap().steps;
                    assert_eq!(steps.last().unwrap().id, effect);
                    for original in &before.layers[0].mask.as_ref().unwrap().steps {
                        let current = steps.iter().find(|step| step.id == original.id).unwrap();
                        assert_eq!(
                            serde_json::to_value(&current.pixels).unwrap(),
                            serde_json::to_value(&original.pixels).unwrap()
                        );
                    }
                } else {
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].effects.last().unwrap().id, effect);
                }
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
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
                assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(
                    serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(),
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
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                assert_eq!(app.effect_selected, selected);
                if mask {
                    assert_eq!(app.mask_step.as_deref(), Some(inspector.as_str()));
                }
                assert_eq!(
                    serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers[0].pixels).unwrap(),
                    serde_json::to_value(&before.layers[0].pixels).unwrap()
                );
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
    #[test]
    fn right_click_native_color_and_mask_effects_bake_once_with_explicit_scope() {
        for depth in [8, 16] {
            for mask in [false, true] {
                let ctx = egui::Context::default();
                let mut e = crate::engine::Engine::new();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 24, depth).unwrap();
                e.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(10, 12, [12345, 23457, 34569, 65535]);
                let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
                e.resident_state_mut().expect("resident fixture").undo.clear();
                let before = e.resident_state().expect("resident fixture").document.clone();
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
                assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
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
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1, "{}", app.message);
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
                if mask {
                    assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap().steps.len(), 1);
                } else {
                    assert!(e.resident_state().expect("resident fixture").document.layers[0].effects.is_empty());
                }
                e.undo("human").unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), before.export_png().unwrap());
            }
        }
    }
    #[test]
    fn top_color_effect_is_last_without_reinterpreting_saved_sources() {
        let ctx = egui::Context::default();
        let mut e = crate::engine::Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new(4, 4).unwrap();
        e.resident_state_mut().expect("resident fixture").document.layers[0].kind = "fill".into();
        e.resident_state_mut().expect("resident fixture").document.layers[0].color = [64, 64, 64, 255];
        let id = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
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
        let source = serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers[0].effects).unwrap();
        let document = e.resident_state().expect("resident fixture").document.clone();
        let layer = e.resident_state().expect("resident fixture").document.layers[0].clone();
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
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.color_stack(ui, &layer, &document));
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
                egui::CentralPanel::default()
                    .show(ctx, |ui| app.color_stack(ui, &layer, &document));
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
            serde_json::to_value(&e.resident_state().expect("resident fixture").document.layers[0].effects).unwrap(),
            source
        );
        assert_eq!(
            &e.resident_state().expect("resident fixture").document.preview(None, 4, None, false).unwrap().2[..4],
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
                        settings(ui, "curves", &mut values, 8);
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
    fn standard_curve_drag_moves_endpoints_on_the_record_grid() {
        let points = json!([[51. / 255., 17. / 255.], [204. / 255., 239. / 255.]]);
        let points = points.as_array().unwrap();
        assert_eq!(drag_standard_curve_point(points, 0, 0.3, 0.1), Some([77. / 255., 26. / 255.]));
        assert_eq!(drag_standard_curve_point(points, 0, 1., 0.), Some([203. / 255., 0.]));
        assert_eq!(drag_standard_curve_point(points, 1, 0., 1.), Some([52. / 255., 1.]));
        let fine = json!([[0.123456, 0.], [1., 1.]]);
        assert!(drag_standard_curve_point(fine.as_array().unwrap(), 0, 0.3, 0.1).is_none());
        let spline = json!([[0.,0.],[60./255.,35./255.],[190./255.,225./255.],[1.,1.]]);
        assert_eq!(drag_standard_curve_point(spline.as_array().unwrap(), 1, 1., 0.3), Some([189./255.,77./255.]));
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

fn effect_visibility(ui: &mut egui::Ui, enabled: &mut bool, name: &str) -> egui::Response {
    let icon = if *enabled { Icon::Eye } else { Icon::EyeOff };
    icons::toggle(ui, icon, enabled, &format!("{name} visibility"))
}

fn channel_mixer_settings(ui: &mut egui::Ui, display: &mut Value, source: Option<&Value>) -> Option<egui::Response> {
    use effects::channel_mixer::{Mixer, ROWS};
    let mut mixer = Mixer::new(source.unwrap_or(display)).ok()?;
    let id = ui.id().with("mixer output");
    let mut selected = if mixer.monochrome { 3 } else {
        ui.ctx().data(|data| data.get_temp::<usize>(id)).unwrap_or(0).min(2)
    };
    let output = controls::swatch_choices(
        ui, "Output", &mut selected,
        controls::RGB_CHANNELS.into_iter().chain([("Monochrome", controls::GRAYSCALE)]),
    );
    if selected < 3 {
        ui.ctx().data_mut(|data| data.insert_temp(id, selected));
    }
    let monochrome = selected == 3;
    let mut changed = output.filter(|_| mixer.monochrome != monochrome);
    mixer.monochrome = monochrome;
    egui::Grid::new((ui.id(), "mixer", selected))
        .spacing(Vec2::new(6.0, 3.0))
        .show(ui, |ui| {
            for (index, label) in ["Red", "Green", "Blue", "Constant"].into_iter().enumerate() {
                controls::label(ui, label);
                let mut value = display[ROWS[selected]][index].as_f64()
                    .map_or(f32::from(mixer.rows[selected][index]), |value| value as f32);
                let response = controls::range(ui, (selected, index), &mut value, -200.0..=200.0,
                    160.0_f32.min(ui.available_width()).max(70.), "%", 0, false);
                if response.changed() || response.double_clicked() {
                    if response.changed() {
                        mixer.rows[selected][index] = value.round() as i16;
                    }
                    changed = Some(response);
                }
                ui.end_row();
            }
        });
    if changed.is_some() {
        let mut settings = mixer.settings();
        if let Some(preset) = source.unwrap_or(display).get("preset") {
            settings["preset"] = preset.clone();
        }
        *display = settings;
    }
    changed
}

fn black_white_settings(ui: &mut egui::Ui, display: &mut Value, source: Option<&Value>) -> Option<egui::Response> {
    use effects::black_white::{COLORS, Settings};
    let source = source.unwrap_or(display);
    let adjustment = Settings::new(source).ok()?;
    let mut edited = None;
    egui::Grid::new((ui.id(), "black and white"))
        .spacing(Vec2::new(6.0, 3.0))
        .show(ui, |ui| {
            let labels = ["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas"];
            for ((key, label), weight) in COLORS.into_iter().zip(labels).zip(adjustment.weights) {
                controls::label(ui, label);
                let mut value = effects::number(display, key, weight as f32);
                let response = controls::range(ui, key, &mut value, -200.0..=300.0,
                    160.0_f32.min(ui.available_width()).max(70.), "%", 0, false);
                if response.changed() || response.double_clicked() {
                    let value = if response.changed() { value.round() as i32 } else { weight };
                    edited = Some((response, key, json!(value)));
                }
                ui.end_row();
            }
        });
    ui.horizontal(|ui| {
        let mut use_tint = adjustment.use_tint;
        let response = ui.checkbox(&mut use_tint, "Tint");
        if response.changed() {
            edited = Some((response, "use_tint", json!(use_tint)));
        }
        if use_tint {
            let mut rgb = std::array::from_fn(|index| {
                display["tint_color"][index].as_f64().unwrap_or(adjustment.tint[index]) / 255.
            });
            let before = rgb;
            if let Some(response) = super::effect_color::control(ui, "tint", "Tint", &mut rgb, &mut 1., None, true) {
                let tint = if rgb == before { adjustment.tint } else { rgb.map(|value| value * 255.) };
                edited = Some((response, "tint_color", json!(tint)));
            }
        }
    });
    let (response, key, value) = edited?;
    let mut updated = source.clone();
    updated[key] = value;
    *display = updated;
    Some(response)
}

const HUE_COLORS: [Color32; 6] = [
    controls::RGB_COLORS[0],
    Color32::from_rgb(244, 215, 108),
    controls::RGB_COLORS[1],
    Color32::from_rgb(99, 211, 222),
    controls::RGB_COLORS[2],
    Color32::from_rgb(222, 125, 212),
];
const HUE_RANGES: [(&str, &[Color32]); 6] = [
    ("Reds", &[HUE_COLORS[0]]),
    ("Yellows", &[HUE_COLORS[1]]),
    ("Greens", &[HUE_COLORS[2]]),
    ("Cyans", &[HUE_COLORS[3]]),
    ("Blues", &[HUE_COLORS[4]]),
    ("Magentas", &[HUE_COLORS[5]]),
];

const HUE_WHEEL_RAMP: [Color32; 10] = [
    Color32::from_rgb(255, 0, 0), Color32::from_rgb(255, 255, 0), Color32::from_rgb(0, 255, 0),
    Color32::from_rgb(0, 255, 255), Color32::from_rgb(0, 0, 255), Color32::from_rgb(255, 0, 255),
    Color32::from_rgb(255, 0, 0), Color32::from_rgb(255, 255, 0), Color32::from_rgb(0, 255, 0),
    Color32::from_rgb(0, 255, 255),
];

fn hue_range(ui: &mut egui::Ui, id: impl std::hash::Hash, value: &mut f32, width: f32, colorize: bool) -> egui::Response {
    let (limits, colors, help) = if colorize {
        (0.0..=359.0, &HUE_WHEEL_RAMP[..7], "Choose the color applied to the image.")
    } else {
        (-180.0..=180.0, &HUE_WHEEL_RAMP[3..], "Rotate the hue. The ramp uses red as its reference color.")
    };
    controls::color_range(ui, id, value, limits, width, "°", "Hue", colors).on_hover_text(help)
}

fn hue_band_preview(ui: &mut egui::Ui, start: i16, widths: [i16; 3]) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width().max(1.), 25.), egui::Sense::hover());
    response.on_hover_text("The raised area shows which hues receive the change and where it fades in and out.");
    let painter = ui.painter().with_clip_rect(rect);
    let x = |angle: f32| rect.left() + rect.width() * angle / 360.;
    let bottom = rect.bottom() - 6.;
    let mut spectrum = egui::Mesh::default();
    for (sector, &color) in HUE_WHEEL_RAMP[..7].iter().enumerate() {
        let x = x(sector as f32 * 60.);
        spectrum.colored_vertex(egui::pos2(x, bottom), color);
        spectrum.colored_vertex(egui::pos2(x, rect.bottom()), color);
        if sector != 0 {
            let i = sector as u32 * 2;
            spectrum.add_triangle(i - 2, i, i + 1);
            spectrum.add_triangle(i - 2, i + 1, i - 1);
        }
    }
    painter.add(egui::Shape::mesh(spectrum));
    if widths == [0; 3] {
        return;
    }
    let [rise, core, fall] = widths.map(f32::from);
    let stroke = Stroke::new(1.5, ui.visuals().text_color());
    let top = rect.top() + 2.;
    // Draw both copies so ranges crossing red continue at the opposite edge.
    for start in [f32::from(start) - 360., f32::from(start)] {
        painter.add(egui::Shape::line(vec![
            egui::pos2(x(start), bottom),
            egui::pos2(x(start + rise), top),
            egui::pos2(x(start + rise + core), top),
            egui::pos2(x(start + rise + core + fall), bottom),
        ], stroke));
    }
}

fn selective_color_settings(ui: &mut egui::Ui, display: &mut Value, source: Option<&Value>) -> Option<egui::Response> {
    use effects::selective_color::{RANGES, Settings};
    let mut settings = Settings::new(source.unwrap_or(display)).ok()?;
    let mut changed = None;
    ui.horizontal_wrapped(|ui| {
        controls::label(ui, "Method");
        for (absolute, name, help) in [
            (false, "Relative", "Scale the correction by the remaining room in each channel."),
            (true, "Absolute", "Scale the correction by the full range of each channel."),
        ] {
            let selected = settings.absolute == absolute;
            let id = egui::Id::new(("Selective Color method swatch", absolute));
            let texture = ui.ctx().data(|data| data.get_temp::<TextureHandle>(id))
                .unwrap_or_else(|| {
                    let mut example = Settings { absolute, ranges: [[0; 4]; 9] };
                    example.ranges[7][0] = 45;
                    let example = example.prepare();
                    let mut image = egui::ColorImage::new([24, 2], Color32::BLACK);
                    for (index, pixel) in image.pixels.iter_mut().enumerate() {
                        let shade = 64 + (index % 24) as u8 * 5;
                        let [r, g, b] = if index < 24 { [u16::from(shade); 3] }
                            else { example.sample([u16::from(shade); 3], 255) };
                        *pixel = Color32::from_rgb(r as u8, g as u8, b as u8);
                    }
                    let texture = ui.ctx().load_texture(name, image, egui::TextureOptions::NEAREST);
                    ui.ctx().data_mut(|data| data.insert_temp(id, texture.clone()));
                    texture
                });
            let mut response = ui.add(egui::Button::image_and_text((texture.id(), Vec2::new(34., 16.)), name)
                .selected(selected)).on_hover_text(format!("{help} The sample shows neutrals above and a cyan correction below."));
            response.widget_info(|| egui::WidgetInfo::selected(
                egui::WidgetType::RadioButton, response.enabled(), selected, name,
            ));
            icons::paint_focus(ui, &response);
            if !selected && response.clicked() {
                settings.absolute = absolute;
                response.mark_changed();
                changed = Some(response);
            }
        }
    });
    let id = ui.id().with("selective color range");
    let mut selected = ui.ctx().data(|data| data.get_temp::<usize>(id)).unwrap_or(0).min(RANGES.len() - 1);
    let tones: [(&str, &[Color32]); 3] = [
        ("Whites", &[Color32::WHITE]),
        ("Neutrals", &[Color32::GRAY]),
        ("Blacks", &[Color32::BLACK]),
    ];
    let _ = controls::swatch_choices(ui, "Colors", &mut selected, HUE_RANGES.into_iter().chain(tones));
    ui.ctx().data_mut(|data| data.insert_temp(id, selected));
    egui::Grid::new((ui.id(), "selective color", selected))
        .spacing(Vec2::new(6.0, 3.0))
        .show(ui, |ui| {
            let row = &mut settings.ranges[selected];
            for (index, label) in ["Cyan", "Magenta", "Yellow", "Black"].into_iter().enumerate() {
                controls::label(ui, label);
                let mut value = display[RANGES[selected]][index].as_f64()
                    .map_or(f32::from(row[index]), |value| value as f32);
                let response = controls::range(ui, (selected, index), &mut value, -100.0..=100.0,
                    160.0_f32.min(ui.available_width()).max(70.), "%", 0, false);
                if response.changed() || response.double_clicked() {
                    if response.changed() {
                        row[index] = value.round() as i16;
                    }
                    changed = Some(response);
                }
                ui.end_row();
            }
        });
    if changed.is_some() {
        *display = settings.value();
    }
    changed
}

fn color_lookup_picker(ui: &mut egui::Ui, settings: &Value) -> Option<(Value, egui::Response)> {
    let table = effects::color_lookup::uses_table(settings);
    let profile = effects::color_lookup::source_size(settings, "profile") != 0;
    let format = match settings["lut_format"].as_str() {
        Some("3dl") => "3dl",
        Some("look") => "look",
        _ => "cube",
    };
    if table || profile {
        let name = ["name", "file_name"].into_iter().filter_map(|field| settings[field].as_str())
            .map(|name| name.trim_end_matches('\0').rsplit(['/', '\\']).next().unwrap_or(""))
            .find(|name| !name.is_empty())
            .unwrap_or(match (table, format) {
                (false, _) => "Embedded ICC DeviceLink",
                (true, "3dl") => "Embedded .3dl",
                (true, "look") => "Embedded .look",
                _ => "Embedded .cube",
            });
        controls::label(ui, name);
    }
    if table && !profile {
        let legacy = settings["lut_format"].as_str().unwrap_or("").is_empty();
        let table_default = if format == "3dl" { "rgb" } else { "bgr" };
        let order = |key: &str, default| settings[key].as_str().filter(|value| !value.is_empty()).unwrap_or(default);
        let mut data = order("data_order", "rgb");
        let mut table_order = if legacy { table_default } else { order("table_order", table_default) };
        const ORDERS: [(&str, &[Color32]); 2] = [
            ("RGB", &controls::RGB_COLORS),
            ("BGR", &[controls::RGB_COLORS[2], controls::RGB_COLORS[1], controls::RGB_COLORS[0]]),
        ];
        let mut changed = None;
        for (label, order) in [("Output channel order", &mut data), ("Table axis order", &mut table_order)] {
            let mut selected = usize::from(*order == "bgr");
            if let Some(response) = controls::swatch_choices(ui, label, &mut selected, ORDERS) {
                *order = ["rgb", "bgr"][selected];
                changed = Some(response);
            }
        }
        if let Some(response) = changed {
            let mut updated = settings.clone();
            updated["lut_format"] = json!(format);
            updated["embed_profile"] = json!(true);
            updated["data_order"] = json!(data);
            updated["table_order"] = json!(table_order);
            return Some((updated, response));
        }
    }
    let mut response = ui.button("Choose lookup source…");
    let id = response.id.with("profile error");
    if let Some(error) = ui.ctx().data(|data| data.get_temp::<String>(id)) {
        ui.label(RichText::new(error).color(MUTED));
    }
    if !response.clicked() { return None; }
    let path = rfd::FileDialog::new().add_filter("Color Lookup", &["cube", "3dl", "look", "icc", "icm"]).pick_file()?;
    match effects::color_lookup::read_file(&path) {
        Ok(settings) => {
            ui.ctx().data_mut(|data| { data.remove::<String>(id); });
            response.mark_changed();
            Some((settings, response))
        }
        Err(error) => {
            ui.ctx().data_mut(|data| data.insert_temp(id, error));
            ui.ctx().request_repaint();
            None
        }
    }
}

fn photoshop_hue_saturation_settings(
    ui: &mut egui::Ui,
    display: &mut Value,
    source: Option<&Value>,
) -> Option<egui::Response> {
    use effects::photoshop_hue_saturation::Settings;
    let mut settings = Settings::new(source.unwrap_or(display)).ok()?;
    let mut changed = None;
    let response = ui.checkbox(&mut settings.colorize, "Colorize");
    if response.changed() { changed = Some(response); }
    let id = ui.id().with("hue saturation range");
    let mut selected = ui.ctx().data(|data| data.get_temp::<usize>(id)).unwrap_or(0).min(HUE_RANGES.len());
    if !settings.colorize {
        let _ = controls::swatch_choices(
            ui,
            "Colors",
            &mut selected,
            std::iter::once(("Master", HUE_COLORS.as_slice())).chain(HUE_RANGES),
        );
        ui.ctx().data_mut(|data| data.insert_temp(id, selected));
    }
    let colorize = settings.colorize;
    let (row, shown) = if colorize {
        (&mut settings.colorization, &display["colorization"])
    } else if selected == 0 {
        (&mut settings.master, &display["master"])
    } else {
        (&mut settings.bands[selected - 1].change, &display["bands"][selected - 1]["change"])
    };
    egui::Grid::new((id, colorize, selected))
        .spacing(Vec2::new(6.0, 3.0))
        .show(ui, |ui| {
            for (axis, label) in ["Hue", "Saturation", "Lightness"].into_iter().enumerate() {
                controls::label(ui, label);
                let mut value = shown[axis].as_f64().map_or(f32::from(row[axis]), |value| value as f32);
                let (low, high, suffix) = match axis {
                    0 if colorize => { value = value.rem_euclid(360.); (0., 359., "°") }
                    0 => (-180., 180., "°"),
                    1 if colorize => (0., 100., "%"),
                    _ => (-100., 100., "%"),
                };
                let width = 160.0_f32.min(ui.available_width()).max(70.);
                let control = (id, colorize, selected, axis);
                let response = if axis == 0 {
                    hue_range(ui, control, &mut value, width, colorize)
                } else {
                    controls::range(ui, control, &mut value, low..=high, width, suffix, 0, false)
                };
                if response.changed() || response.double_clicked() {
                    if response.changed() {
                        let value = value.round() as i16;
                        row[axis] = if colorize && axis == 0 && value > 180 { value - 360 } else { value };
                    }
                    changed = Some(response);
                }
                ui.end_row();
            }
        });
    if !colorize && selected > 0 {
        let band = &mut settings.bands[selected - 1];
        let mut start = band.range[0].rem_euclid(360);
        let mut widths = band.widths();
        egui::Grid::new((id, selected, "range stops"))
            .spacing(Vec2::new(6.0, 3.0))
            .show(ui, |ui| {
                for (index, (name, help)) in [
                    ("Range start", "Starting hue in degrees, with red at zero."),
                    ("Fade in", "Degrees over which the adjustment goes from zero to full strength."),
                    ("Core width", "Degrees that receive the full adjustment."),
                    ("Fade out", "Degrees over which the adjustment returns to zero."),
                ].into_iter().enumerate() {
                    let label = ui.label(name).on_hover_text(help);
                    let (stop, max) = if index == 0 { (&mut start, 359) } else {
                        let max = (360 - widths.iter().sum::<i16>() + widths[index - 1]).min(359);
                        (&mut widths[index - 1], max)
                    };
                    let mut value = f32::from(*stop);
                    let response = controls::range(ui, (id, selected, "range", index), &mut value, 0.0..=f32::from(max),
                        160.0_f32.min(ui.available_width()).max(70.), "°", 0, false)
                        .labelled_by(label.id).on_hover_text(help);
                    if response.changed() || response.double_clicked() {
                        if response.changed() {
                            *stop = value.round() as i16;
                            let [rise, core, fall] = widths;
                            band.range = [start, start + rise, start + rise + core, start + rise + core + fall]
                                .map(|stop| stop.rem_euclid(360));
                        }
                        changed = Some(response);
                    }
                    ui.end_row();
                }
            });
        hue_band_preview(ui, start, widths);
    }
    if changed.is_some() { *display = settings.value(); }
    changed
}

fn photo_filter_settings(
    ui: &mut egui::Ui,
    display: &mut Value,
    depth: u16,
    source: Option<&Value>,
) -> Option<egui::Response> {
    use effects::photo_filter::{Color, Settings};
    let mut settings = Settings::new(source.unwrap_or(display)).ok()?;
    let mut changed = None;
    ui.horizontal(|ui| {
        controls::label(ui, "Filter color");
        let before = settings.color.rgb();
        let mut rgb = before;
        let native_rgb = depth == 16 || matches!(settings.color, Color::Rgb(_));
        if let Some(response) = super::effect_color::control(
            ui, "filter color", "Filter", &mut rgb, &mut 1., None, native_rgb,
        ) {
            if rgb != before {
                settings.color = if native_rgb {
                    Color::Rgb(rgb.map(|value| (value * f64::from(u16::MAX)).round() as u16))
                } else {
                    Color::from_rgb(rgb)
                };
            }
            changed = Some(response);
        }
    });
    ui.horizontal(|ui| {
        controls::label(ui, "Density");
        let mut value = effects::number(display, "density", f32::from(settings.density));
        let response = controls::range(ui, "density", &mut value, 0.0..=100.0,
            160.0_f32.min(ui.available_width()).max(70.), "%", 0, false);
        if response.changed() || response.double_clicked() {
            if response.changed() {
                settings.density = value.round() as u8;
            }
            changed = Some(response);
        }
    });
    let response = ui.checkbox(&mut settings.preserve_luminosity, "Preserve Luminosity");
    if response.changed() {
        changed = Some(response);
    }
    if changed.is_some() {
        *display = settings.value();
    }
    changed
}

fn gradient_map_settings(ui: &mut egui::Ui, display: &mut Value, source: Option<&Value>) -> Option<egui::Response> {
    use effects::gradient_map::{Method, Settings};

    let mut map = Settings::new(source.unwrap_or(display)).ok()?;
    let mut method = map.method;
    let mut changed = None;
    controls::label(ui, "Interpolation");
    ui.horizontal_wrapped(|ui| {
        for (choice, label, help) in [
            (Method::Classic, "Classic", "Interpolate the encoded RGB values."),
            (Method::Linear, "Linear", "Interpolate in linear light."),
            (Method::Perceptual, "Perceptual", "Interpolate in OKLab for more even visual transitions."),
        ] {
            let active = method == choice;
            let font = egui::TextStyle::Button.resolve(ui.style());
            let text_size = ui.painter().layout_no_wrap(label.into(), font.clone(), Color32::PLACEHOLDER).size();
            let mut response = ui
                .add_sized([70.0_f32.max(text_size.x + 8.), text_size.y + 24.], egui::Button::new("").selected(active))
                .on_hover_text(format!("{label} · {help}"));
            response.widget_info(|| {
                egui::WidgetInfo::selected(egui::WidgetType::RadioButton, response.enabled(), active, label)
            });
            let inner = response.rect.shrink(4.);
            let swatch = Rect::from_min_size(inner.min, Vec2::new(inner.width(), 12.));
            map.method = choice;
            let prepared = map.prepare();
            super::gradient_stops::paint_ramp(ui.painter(), swatch, |t| {
                let [r, g, b] = prepared.lookup(t, 255).map(|value| value as u8);
                Color32::from_rgb(r, g, b)
            });
            ui.painter().text(
                inner.center_bottom(),
                egui::Align2::CENTER_BOTTOM,
                label,
                font,
                ui.style().interact_selectable(&response, active).text_color(),
            );
            icons::paint_focus(ui, &response);
            if !active && response.clicked() {
                method = choice;
                response.mark_changed();
                changed = Some(response);
            }
        }
    });
    map.method = method;
    let mut settings = map.value();
    ui.horizontal(|ui| {
        controls::label(ui, "Smoothness");
        let mut value = effects::number(display, "smoothness", settings["smoothness"].as_f64().unwrap_or(1.) as f32) * 100.;
        let response = controls::range(ui, "smoothness", &mut value, 0.0..=100.0,
            160.0_f32.min(ui.available_width()).max(70.), "%", 1, false);
        if response.changed() || response.double_clicked() {
            if response.changed() {
                settings["smoothness"] = json!((f64::from(value) * 4096. / 100.).round() / 4096.);
            }
            changed = Some(response);
        }
    });
    changed = super::gradient_stops::map_controls(ui, &mut settings).or(changed);
    if changed.is_some() {
        *display = settings;
    }
    changed
}

fn channel_clamp_settings(ui: &mut egui::Ui, settings: &mut Value) -> Option<egui::Response> {
    const OPACITY: (&str, &[Color32]) = ("Opacity / alpha", &[Color32::BLACK, Color32::WHITE]);
    let (mut channel, minimum, maximum) = effects::channel_clamp::parameters(settings).ok()?;
    let mut changed = controls::swatch_choices(
        ui,
        "Channel",
        &mut channel,
        controls::RGB_CHANNELS.into_iter().chain([OPACITY]),
    );
    if changed.is_some() {
        settings["channel"] = json!(["r", "g", "b", "o"][channel]);
    }
    egui::Grid::new((ui.id(), "clamp bounds"))
        .spacing(Vec2::new(6.0, 3.0))
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

fn levels_slider(ui: &mut egui::Ui, settings: &mut Value, byte_levels: bool) -> Option<egui::Response> {
    let gap = if byte_levels { 1. / 255. } else if effects::levels::exact(settings) { 1. / 65535. } else { 0.01 };
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
                0 => {
                    let max = if byte_levels { 253. / 255. } else { 1. - gap };
                    black = x.min(white - gap).clamp(0.0, max);
                    settings["black"] = if byte_levels { json!(f64::from((black * 255.).round()) / 255.) } else { json!(black) };
                }
                2 => {
                    let min = if byte_levels { 2. / 255. } else { gap };
                    white = x.max(black + gap).clamp(min, 1.0);
                    settings["white"] = if byte_levels { json!(f64::from((white * 255.).round()) / 255.) } else { json!(white) };
                }
                _ => {
                    gamma = (((x - black) / (white - black)).clamp(0.5_f32.powf(9.99), 0.5_f32.powf(0.1)).ln()
                        / 0.5_f32.ln())
                    .clamp(0.1, 9.99);
                    settings["gamma"] = json!(gamma);
                }
            }
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

#[cfg(test)]
#[path = "levels_input_tests.rs"]
mod levels_input_tests;

#[cfg(test)]
mod bitmap_inspector_tests {
    use super::super::workspace_tests as workspace;
    use super::*;
    const SIZE: Vec2 = Vec2::new(1100., 1800.);
    fn bitmap_png(depth: u16, offset: u16) -> String {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let mut words = Vec::new();
        for y in 0..16u16 {
            for x in 0..32u16 {
                words.extend([
                    12347u16.wrapping_add(x * 1459).wrapping_add(offset),
                    23459u16.wrapping_add(y * 2311).wrapping_add(offset),
                    34571u16.wrapping_add((x + y) * 977),
                    if (x + y) % 5 == 0 { 32769 } else { 65535 },
                ]);
            }
        }
        let png = if depth == 16 {
            crate::raster::png16(32, 16, &words)
        } else {
            crate::raster::png(
                32,
                16,
                &words
                    .into_iter()
                    .map(crate::raster::project16)
                    .collect::<Vec<_>>(),
            )
        }
        .unwrap();
        STANDARD.encode(png)
    }
    fn fixture(depth: u16, kind: &str) -> (PeerBrush, egui::Context, String) {
        let (mut app, ctx) = workspace::fixture(depth);
        let png = bitmap_png(depth, 0);
        let effect = {
            let mut engine = app.shared.lock().unwrap();
            let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
            engine
                .edit(
                    "human",
                    &[json!({"op":"effect.add","layer":layer,"kind":kind,
                "settings":{"pattern":"bitmap","bitmap_png":png,"sampling":"nearest",
                    "bitmap_scale":2.,"bitmap_angle":17.25,"phase_x":3.,"phase_y":-2.}})],
                    None,
                    None,
                    "Embedded inspector source",
                )
                .unwrap();
            let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
            for opacity in [0.9, 0.8] {
                engine
                    .edit(
                        "human",
                        &[json!({"op":"layer.update","layer":layer,"opacity":opacity})],
                        None,
                        None,
                        "Human layer setting",
                    )
                    .unwrap();
            }
            engine.undo("human").unwrap();
            assert!(!engine.resident_state().expect("resident fixture").undo.is_empty() && !engine.resident_state().expect("resident fixture").redo.is_empty());
            effect
        };
        app.select_effect(&app.selected.clone(), false, &effect);
        (app, ctx, png)
    }
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        workspace::draw(app, ctx, SIZE, events)
    }
    fn settle_canvas(app: &mut PeerBrush, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            frame(app, ctx, vec![]);
            if !app.pending
                && app.canvas_preview.is_some()
                && app.canvas_preview == app.last_preview
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Actual bitmap preview did not finish"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    fn button(at: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    fn drag_opacity(app: &mut PeerBrush, ctx: &egui::Context) -> Pos2 {
        frame(app, ctx, vec![]);
        let output = frame(app, ctx, vec![]);
        let at = workspace::label(&output, "75%");
        frame(
            app,
            ctx,
            vec![egui::Event::PointerMoved(at), button(at, true)],
        );
        let moved = at - Vec2::new(28., 0.);
        frame(app, ctx, vec![egui::Event::PointerMoved(moved)]);
        frame(app, ctx, vec![egui::Event::PointerMoved(moved)]);
        assert!(app.parameter_gesture.is_some());
        moved
    }
    #[test]
    fn idle_bitmap_inspector_omits_only_png_from_controls_and_preserves_exact_native_work() {
        for depth in [8, 16] {
            for kind in ["pattern_overlay", "texture"] {
                let (mut app, ctx, png) = fixture(depth, kind);
                let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.layers[0].effects[0]
                    .settings
                    .clone();
                let before = workspace::source(&app);
                for _ in 0..8 {
                    let values = inspector_values(kind, &source);
                    assert!(values.get("bitmap_png").is_none());
                    assert_eq!(
                        values.as_object().unwrap().len() + 1,
                        source.as_object().unwrap().len()
                    );
                    for (key, value) in source.as_object().unwrap() {
                        if key != "bitmap_png" {
                            assert_eq!(values.get(key), Some(value));
                        }
                    }
                    frame(&mut app, &ctx, vec![]);
                    let engine = app.shared.lock().unwrap();
                    assert!(Arc::ptr_eq(
                        &engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings,
                        &source
                    ));
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["bitmap_png"],
                        json!(png)
                    );
                }
                assert_eq!(workspace::source(&app), before);
                let procedural = crate::effects::styles::defaults(kind);
                assert_eq!(inspector_values(kind, &procedural), procedural);
                let curves = json!({"points":[[0.,0.],[1.,1.]],"interpolation":"smooth"});
                assert_eq!(inspector_values("curves", &curves), curves);
            }
        }
    }
    #[test]
    fn bitmap_inspector_actual_drag_restores_png_for_shared_native_preview_and_one_history_commit()
    {
        for depth in [8, 16] {
            for kind in ["pattern_overlay", "texture"] {
                let (mut app, ctx, png) = fixture(depth, kind);
                let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let source = workspace::source(&app);
                let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                let moved = drag_opacity(&mut app, &ctx);
                let commands = app.parameter_gesture.as_ref().unwrap().commands.clone();
                assert_eq!(commands.len(), 1);
                assert_eq!(commands[0]["settings"]["bitmap_png"], json!(png));
                let original = &before.layers[0].effects[0].settings;
                for (key, value) in original.as_object().unwrap() {
                    if key != "opacity" {
                        assert_eq!(commands[0]["settings"].get(key), Some(value));
                    }
                }
                let preview =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                assert_ne!(preview.export_png().unwrap(), before.export_png().unwrap());
                settle_canvas(&mut app, &ctx);
                let (w, h, pixels) = app.canvas_pixels.as_ref().unwrap();
                assert_eq!(
                    *pixels,
                    preview.preview(None, (*w).max(*h), None, false).unwrap().2
                );
                assert_eq!(workspace::source(&app), source);
                frame(&mut app, &ctx, vec![button(moved, false)]);
                let mut engine = app.shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 1);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["bitmap_png"],
                    json!(png)
                );
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
                assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
                engine.undo("human").unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    before.export_png().unwrap()
                );
                engine.redo("human").unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    preview.export_png().unwrap()
                );
            }
        }
    }
    #[test]
    fn bitmap_inspector_cancel_newer_source_and_reservations_cannot_rebind_the_old_png() {
        for depth in [8, 16] {
            for interruption in ["escape", "newer", "reserved"] {
                let (mut app, ctx, _) = fixture(depth, "pattern_overlay");
                let moved = drag_opacity(&mut app, &ctx);
                let layer = app.selected.clone();
                if interruption == "newer" {
                    let mut engine = app.shared.lock().unwrap();
                    let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                    let mut settings = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings.as_ref().clone();
                    settings["bitmap_png"] = json!(bitmap_png(depth, 19001));
                    engine
                        .edit(
                            "human",
                            &[json!({"op":"effect.update","layer":layer,"effect":effect,
                        "settings":settings})],
                            None,
                            None,
                            "Newer human bitmap",
                        )
                        .unwrap();
                } else if interruption == "reserved" {
                    app.shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "other",
                            "Keep human source",
                            vec![crate::engine::Scope::layer(&layer)],
                        )
                        .unwrap();
                }
                let retained = workspace::source(&app);
                let reservations = json!(app.shared.lock().unwrap().leases);
                let events = if interruption == "escape" {
                    vec![egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }]
                } else {
                    vec![]
                };
                frame(&mut app, &ctx, events);
                let late = moved - Vec2::new(18., 0.);
                frame(&mut app, &ctx, vec![egui::Event::PointerMoved(late)]);
                frame(&mut app, &ctx, vec![button(late, false)]);
                frame(&mut app, &ctx, vec![]);
                assert!(app.parameter_gesture.is_none());
                assert_eq!(workspace::source(&app), retained, "{depth}/{interruption}");
                assert_eq!(json!(app.shared.lock().unwrap().leases), reservations);
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.bit_depth, depth);
            }
        }
    }
}

#[cfg(test)]
mod native_metadata_tests {
    use super::*;
    #[test]
    fn native_effect_action_menus_read_only_the_shared_metadata_snapshot() {
        for depth in [8, 16] {
            let ctx = egui::Context::default();
            let mut document = Document::new_depth(9, 7, depth).unwrap();
            let effect = crate::engine::id();
            let step = crate::engine::id();
            document.layers[0].effects.push(crate::effects::Effect {
                id: effect.clone(),
                kind: "invert".into(),
                enabled: true,
                weight: 0.5,
                settings: Arc::new(json!({})),
            });
            document.layers[0].mask = Some(crate::engine::Mask {
                enabled: true,
                linked: true,
                cache_key: crate::engine::id(),
                steps: vec![crate::engine::MaskStep {
                    id: step.clone(),
                    kind: "fill".into(),
                    enabled: true,
                    weight: 1.,
                    value: 183.,
                    pixels: crate::raster::Raster::new_depth(9, 7, depth),
                    settings: json!({}),
                }],
            });
            let mut engine = crate::engine::Engine::new();
            engine.resident_state_mut().expect("resident fixture").document = document.clone();
            let mut app = PeerBrush::init(
                &ctx,
                Arc::new(std::sync::Mutex::new(engine)),
                Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: Default::default(),
                    instance_lock: None,
                    availability: Default::default(),
                },
            );
            let pool = crate::large_document::ResidentBudget::new(8 * 1024 * 1024).unwrap();
            let owner = crate::engine::view::DocumentSnapshot::from(
                crate::tree::presentation_tests::native(&document, &pool),
            );
            let admitted = pool.used();
            for mask in [false, true] {
                let id = if mask { &step } else { &effect };
                assert!(app.effect_present(&document.layers[0].id, mask, id));
                let output = ctx.run(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        app.effect_options(
                            ui,
                            owner.view().layer(0).unwrap(),
                            mask,
                            id,
                            0,
                            owner.view().revision(),
                        );
                    });
                });
                let text = output
                    .shapes
                    .iter()
                    .filter_map(|shape| match &shape.shape {
                        egui::epaint::Shape::Text(text) => Some(text.galley.text()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(text.contains("Top effects run last"));
                assert!(text.contains(if mask {
                    "Bake into mask pixels"
                } else {
                    "Bake into layer pixels"
                }));
                assert_eq!(pool.used(), admitted);
                assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").document.revision, 0);
            }
        }
    }
}
