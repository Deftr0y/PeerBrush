use super::*;

const SWATCH_SIZE: usize = 32;

fn sample_swatch(ctx: &egui::Context, kind: &str, key: &str, choice: &str) -> Option<TextureHandle> {
    if choice == "bitmap" {
        return None;
    }
    let id = egui::Id::new(("layer style sample", kind, key, choice));
    if let Some(texture) = ctx.data(|data| data.get_temp::<TextureHandle>(id)) {
        return Some(texture);
    }

    let mut settings = json!({"opacity":1.});
    settings[key] = match key {
        "down" => json!(choice == "recessed"),
        "invert" | "pixel_snap" | "source_knockout" => json!(choice == "on"),
        _ => json!(choice),
    };
    let shadow_sample = matches!(key, "pixel_snap" | "source_knockout");
    let fills_frame = key == "pattern"
        || kind == "gradient_overlay"
        || key == "invert" && matches!(kind, "pattern_overlay" | "texture");
    match kind {
        "gradient_overlay" => {
            settings["angle"] = json!(0.);
            settings["color"] = json!([0.08, 0.08, 0.08]);
            settings["color2"] = json!([0.94, 0.94, 0.94]);
        }
        "texture" => settings["depth"] = json!(1.),
        "stroke" => {
            settings["color"] = json!([0.96, 0.58, 0.32]);
            settings["size"] = json!(5.);
        }
        _ => {}
    }
    if shadow_sample {
        settings["angle"] = json!(45.);
        settings["distance"] = json!(6.5);
        settings["size"] = json!(0.);
        let color = if kind == "inner_shadow" { [1., 0.75, 0.40] } else { [0.12, 0.06, 0.04] };
        settings["color"] = json!(color);
        if kind == "inner_shadow" {
            settings["blend"] = json!("normal");
        }
    }
    let mut words = vec![0; SWATCH_SIZE * SWATCH_SIZE * 4];
    for (index, pixel) in words.chunks_exact_mut(4).enumerate() {
        let (x, y) = (index % SWATCH_SIZE, index / SWATCH_SIZE);
        if fills_frame || ((7..25).contains(&x) && (7..25).contains(&y)) {
            pixel[3] = if key == "source_knockout" { 32768 } else { u16::MAX };
            if kind == "contour" && key == "invert" {
                pixel[..3].fill((x as u16 - 7) * 3855);
            } else if shadow_sample {
                pixel[..3].copy_from_slice(&[65535, 29000, 13000]);
            } else {
                pixel[..3].fill(27000);
            }
        }
    }
    crate::effects::styles::apply(&mut words, SWATCH_SIZE as u32, SWATCH_SIZE as u32, kind, &settings).ok()?;
    if shadow_sample {
        for pixel in words.chunks_exact_mut(4) {
            let source = [pixel[0], pixel[1], pixel[2], pixel[3]];
            let background = [50000, 50000, 50000, u16::MAX];
            pixel.copy_from_slice(&crate::raster::blend16(background, source, 1., "normal"));
        }
    }
    let rgba: Vec<_> = words.into_iter().map(crate::raster::project16).collect();
    let texture = ctx.load_texture(
        format!("{kind} {choice} sample"),
        egui::ColorImage::from_rgba_unmultiplied([SWATCH_SIZE; 2], &rgba),
        egui::TextureOptions::LINEAR,
    );
    ctx.data_mut(|data| data.insert_temp(id, texture.clone()));
    Some(texture)
}

pub(super) fn settings(ui: &mut egui::Ui, kind: &str, value: &mut Value) -> Option<egui::Response> {
    let bitmap = crate::effects::patterns::active(kind, value);
    let mut defaults = crate::effects::styles::defaults(kind);
    if kind == "gradient_overlay" {
        defaults["offset_x"] = json!(0.);
        defaults["offset_y"] = json!(0.);
        defaults["smoothness"] = json!(0.);
    }
    if bitmap {
        for (key, value) in crate::effects::styles::bitmap_defaults()
            .as_object()
            .unwrap()
        {
            defaults[key] = value.clone();
        }
    }
    let mut changed = None;
    if kind == "bevel_emboss" {
        ui.horizontal_wrapped(|ui| {
            controls::label(ui, "Relief");
            let current = value["down"].as_bool().unwrap_or(false);
            for (down, label, help) in [
                (false, "Raised", "Light the bevel as if it rises from the surface."),
                (true, "Recessed", "Reverse the bevel lighting so it appears recessed."),
            ] {
                let selected = current == down;
                let choice = if down { "recessed" } else { "raised" };
                let button = match sample_swatch(ui.ctx(), kind, "down", choice) {
                    Some(texture) => {
                        egui::Button::image_and_text((texture.id(), Vec2::splat(26.)), label)
                    }
                    None => egui::Button::new(label),
                };
                let mut response = ui.add(button.selected(selected)).on_hover_text(help);
                response.widget_info(|| {
                    egui::WidgetInfo::selected(egui::WidgetType::RadioButton, response.enabled(), selected, label)
                });
                crate::icons::paint_focus(ui, &response);
                if response.clicked() && !selected {
                    value["down"] = json!(down);
                    response.mark_changed();
                    changed = Some(response);
                }
            }
        });
    }
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
                ("custom", "Custom"),
            ],
        ),
        (
            "highlight_blend",
            std::iter::once(("inherit", "Inherit effect blend"))
                .chain(crate::raster::BLENDS.iter().copied())
                .collect::<Vec<_>>(),
        ),
        (
            "shadow_blend",
            std::iter::once(("inherit", "Inherit effect blend"))
                .chain(crate::raster::BLENDS.iter().copied())
                .collect::<Vec<_>>(),
        ),
        (
            "pattern",
            if bitmap {
                vec![
                    ("bitmap", "Bitmap PNG"),
                    ("checker", "Checker"),
                    ("stripes", "Stripes"),
                    ("dots", "Dots"),
                    ("noise", "Noise"),
                ]
            } else {
                vec![
                    ("checker", "Checker"),
                    ("stripes", "Stripes"),
                    ("dots", "Dots"),
                    ("noise", "Noise"),
                ]
            },
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
                    "highlight_blend" => "Highlight blend",
                    "shadow_blend" => "Shadow blend",
                    "pattern" => "Pattern",
                    "position" => "Position",
                    "source" => "Source",
                    _ => "Style",
                },
            );
            let mut response = None;
            if matches!(key, "style" | "pattern" | "position" | "source" | "contour") {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 3.;
                    for (id, label) in &options {
                        let active = selected == *id;
                        let mut r = if key == "contour" {
                            let r = ui.add_sized([32., 24.], egui::Button::new("").selected(active));
                            contour_points::swatch(ui, r.rect.shrink(5.), id, &value["contour_points"]);
                            r
                        } else {
                            let button = match sample_swatch(ui.ctx(), kind, key, id) {
                                Some(texture) => egui::Button::image((texture.id(), Vec2::splat(26.))),
                                None => egui::Button::new(*label),
                            };
                            ui.add(button.selected(active))
                        }.on_hover_text(*label);
                        r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::RadioButton, r.enabled(), active, *label));
                        crate::icons::paint_focus(ui, &r);
                        if !active && r.clicked() {
                            selected = (*id).into();
                            r.mark_changed();
                            response = Some(r);
                        }
                    }
                    if let Some((_, label)) = options.iter().find(|(id, _)| *id == selected && *id != "bitmap") {
                        ui.label(*label);
                    }
                });
            } else {
                let label = options.iter().find(|(id, _)| *id == selected)
                    .map_or(selected.as_str(), |(_, label)| *label);
                let effect_blend = value["blend"].as_str().unwrap_or("normal");
                egui::ComboBox::from_id_salt((ui.id(), key)).selected_text(label).show_ui(ui, |ui| {
                    for (id, label) in options {
                        let mode = if id == "inherit" { effect_blend } else { id };
                        let mut r = controls::blend_choice(ui, mode, label, selected == id, true);
                        if r.clicked() {
                            ui.close_menu();
                            if selected != id {
                                selected = id.into();
                                r.mark_changed();
                                response = Some(r);
                            }
                        }
                    }
                });
            }
            if let Some(response) = response {
                value[key] = json!(selected);
                if key == "contour" {
                    if selected == "custom" {
                        value["contour_points"] = json!([{"position":0.,"value":0.},{"position":1.,"value":1.}]);
                    } else {
                        value.as_object_mut().unwrap().remove("contour_points");
                    }
                }
                if key == "pattern" && selected != "bitmap" {
                    let fields = value.as_object_mut().unwrap();
                    fields.remove("use_pattern_alpha");
                    for key in crate::effects::styles::bitmap_defaults().as_object().unwrap().keys() {
                        fields.remove(key);
                    }
                }
                changed = Some(response);
            }
        });
    }
    if crate::effects::patterns::active(kind, value) {
        use crate::production_export::Resampling;
        ui.horizontal_wrapped(|ui| {
            controls::label(ui, "Sampling");
            let current = if value["sampling"] == "nearest" { Resampling::Nearest } else { Resampling::Bilinear };
            if let Some((method, response)) = controls::sampling_choices(ui, current) {
                value["sampling"] = json!(method);
                changed = Some(response);
            }
        });
    }
    egui::Grid::new((ui.id(), "style controls"))
        .spacing(Vec2::new(6., 3.))
        .show(ui, |ui| {
            for (key, label, min, max, suffix) in [
                ("opacity", "Opacity", 0., 1., "%"),
                (
                    "size",
                    "Size",
                    0.,
                    if kind == "stroke" { crate::effects::styles::MAX_STROKE_SIZE as f32 } else { 64. },
                    " px",
                ),
                ("soften", "Soften", 0., 64., " px"),
                ("distance", "Distance", 0., 128., " px"),
                (
                    "angle",
                    if kind == "texture" {
                        "Light angle"
                    } else {
                        "Angle"
                    },
                    -180.,
                    180.,
                    "°",
                ),
                ("altitude", "Light altitude", 0., 90., "°"),
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
                ("smoothness", "Color smoothness", 0., 1., "%"),
                ("offset_x", "Offset X", -1., 1., "%"),
                ("offset_y", "Offset Y", -1., 1., "%"),
                ("highlight_opacity", "Highlight opacity", 0., 1., "%"),
                ("shadow_opacity", "Shadow opacity", 0., 1., "%"),
                ("bitmap_scale", "Scale", 0.01, 8., "%"),
                (
                    "bitmap_angle",
                    if kind == "texture" {
                        "Tile angle"
                    } else {
                        "Angle"
                    },
                    -180.,
                    180.,
                    "°",
                ),
                ("phase_x", "Phase X", -32768., 32768., " px"),
                ("phase_y", "Phase Y", -32768., 32768., " px"),
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
                if defaults.get(key).is_none() || (bitmap && key == "scale") {
                    continue;
                }
                if key == "smoothness" && value["color_stops"].as_array().is_none_or(|stops| stops.len() < 3) {
                    continue;
                }
                let linked_shadow = key == "angle" && value.get("global_light").is_some();
                controls::label(ui, if linked_shadow { "Global light" } else { label });
                if matches!(key, "angle" | "bitmap_angle" | "altitude") {
                    let mut degrees = value[key].as_f64().or_else(|| defaults[key].as_f64()).unwrap_or(0.);
                    let (name, help) = match key {
                        "altitude" => ("Light altitude", "Set the light's elevation above the surface, from 0° along the surface to 90° overhead."),
                        "bitmap_angle" => ("Tile angle", "Tile rotation: 0° points right; +90° rotates clockwise."),
                        _ => match kind {
                            "drop_shadow" | "inner_shadow" if linked_shadow => ("Shadow direction", "Uses the project’s Global Light. Changing the direction switches this shadow to local light."),
                            "drop_shadow" | "inner_shadow" => ("Shadow direction", "Shadow displacement: 0° points right; +90° points up. The light is opposite the shadow direction. Distance is independent."),
                            "bevel_emboss" | "texture" => ("Light angle", "Light direction: 0° points right; +90° points up. Altitude and distance remain independent."),
                            "satin" => ("Satin direction", "Satin samples along this axis: 0° is horizontal; +90° is vertical. Distance is independent."),
                            _ => ("Gradient angle", "Gradient direction: 0° points right; +90° points up. Scale and offset remain independent."),
                        },
                    };
                    let limits = (key == "altitude").then_some(f64::from(min)..=f64::from(max));
                    let response = controls::angle_with_limits(ui, key, &mut degrees, key == "bitmap_angle", name, help, limits);
                    if response.changed() || response.double_clicked() || controls::number_editing(&response.ctx, response.id) {
                        value[key] = json!(degrees);
                        changed = Some(response);
                    }
                    ui.end_row();
                    continue;
                }
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
                    160_f32.min(ui.available_width()).max(70.),
                    suffix,
                    if suffix == " px" { 1 } else { 0 },
                    false,
                );
                if r.changed() || r.double_clicked() {
                    if r.changed() { value[key] = json!(n / multiplier); }
                    changed = Some(r);
                }
                ui.end_row();
            }
        });
    for (key, label) in [
        (
            "color",
            if kind == "texture" {
                "Shadow"
            } else if kind == "bevel_emboss" {
                "Highlight"
            } else {
                "Color"
            },
        ),
        (
            "color2",
            if kind == "texture" {
                "Highlight"
            } else if kind == "bevel_emboss" {
                "Shadow"
            } else {
                "Second color"
            },
        ),
    ] {
        if defaults.get(key).is_none()
            || (bitmap && kind != "texture")
            || (kind == "gradient_overlay"
                && value["color_stops"]
                    .as_array()
                    .is_some_and(|s| s.len() >= 2))
        {
            continue;
        }
        let mut rgb: [f64; 3] = std::array::from_fn(|i| {
            value[key][i]
                .as_f64()
                .or_else(|| defaults[key][i].as_f64())
                .unwrap_or(0.)
        });
        let alpha_key = if kind == "bevel_emboss" {
            if key == "color" {
                "highlight_opacity"
            } else {
                "shadow_opacity"
            }
        } else {
            "opacity"
        };
        let mut alpha = value[alpha_key]
            .as_f64()
            .or_else(|| defaults[alpha_key].as_f64())
            .unwrap_or(1.);
        ui.horizontal(|ui| {
            controls::label(ui, label);
            if let Some(response) = effect_color::control(
                ui,
                key,
                label,
                &mut rgb,
                &mut alpha,
                Some(if kind == "bevel_emboss" {
                    "Channel opacity"
                } else {
                    "Effect opacity"
                }),
                true,
            ) {
                value[key] = json!(rgb);
                value[alpha_key] = json!(alpha);
                changed = Some(response);
            }
        });
    }
    if kind == "gradient_overlay" {
        changed = gradient_stops::controls(ui, value).or(changed);
        ui.horizontal_wrapped(|ui| {
            controls::label(ui, "Placement");
            let current = (value["pixel_grid"] == true, value["align_layer"] == true);
            for (grid, layer, icon, label, help) in [
                (false, false, Icon::Gradient, "Fractional canvas", "Use the full canvas with fractional gradient placement."),
                (true, false, Icon::CanvasBounds, "Pixel-aligned canvas", "Align the gradient to the pixel grid using the canvas bounds."),
                (true, true, Icon::TrimAlpha, "Pixel-aligned layer", "Align the gradient to the pixel grid using the bounds of the incoming visible pixels."),
            ] {
                let selected = current == (grid, layer);
                let mut response = icons::labeled_choice(ui, icon, label, Some(selected), 0.)
                    .on_hover_text(help);
                if response.clicked() && !selected {
                    value["pixel_grid"] = json!(grid);
                    if layer || value.get("align_layer").is_some() {
                        value["align_layer"] = json!(layer);
                    }
                    response.mark_changed();
                    changed = Some(response);
                }
            }
        });
    }
    let coverage_option = match kind {
        "gradient_overlay" => Some(("use_gradient_alpha", "Use gradient opacity to vary the layer's transparency, independently of gradient color.")),
        "pattern_overlay" if value["pattern"] == "bitmap" => Some(("use_pattern_alpha", "Use bitmap alpha to vary the layer's transparency, independently of tile color.")),
        _ => None,
    };
    if let Some((key, help)) = coverage_option {
        ui.horizontal_wrapped(|ui| {
            controls::label(ui, "Coverage");
            let current = value[key] == true;
            for (coverage, icon, label, help) in [
                (false, Icon::FillLayer, "Keep layer coverage", "Blend the overlay into the layer's colors while preserving its transparency."),
                (true, Icon::Mask, "Apply overlay coverage", help),
            ] {
                let selected = current == coverage;
                let mut response = icons::labeled_choice(ui, icon, label, Some(selected), 0.)
                    .on_hover_text(help);
                if response.clicked() && !selected {
                    value[key] = json!(coverage);
                    response.mark_changed();
                    changed = Some(response);
                }
            }
        });
    }
    changed = contour_points::controls(ui, value).or(changed);
    for (key, label, choices) in [
        ("invert", "Effect output", [
            ("Normal", "Use the effect’s original pattern, tone, or coverage."),
            ("Inverted", "Reverse the effect’s pattern, tone, or coverage."),
        ]),
        ("pixel_snap", "Shadow offset", [
            ("Fractional", "Allow the shadow to land between pixels for finer placement."),
            ("Pixel aligned", "Round the shadow’s horizontal and vertical offsets to whole pixels."),
        ]),
        ("source_knockout", "Shadow under layer", [
            ("Show through", "Let the shadow show through translucent parts of the layer."),
            ("Knock out source", "Use the layer’s opacity to reduce the shadow underneath it."),
        ]),
    ] {
        if defaults.get(key).is_some() && !bitmap {
            let current = value[key].as_bool().or_else(|| defaults[key].as_bool()).unwrap_or(false);
            ui.horizontal_wrapped(|ui| {
                controls::label(ui, label);
                for (enabled, (label, help)) in [false, true].into_iter().zip(choices) {
                    let selected = current == enabled;
                    let sample = sample_swatch(ui.ctx(), kind, key, if enabled { "on" } else { "off" });
                    let button = match sample {
                        Some(texture) => egui::Button::image_and_text((texture.id(), Vec2::splat(26.)), label),
                        None => egui::Button::new(label),
                    };
                    let mut response = ui.add(button.selected(selected)).on_hover_text(help);
                    response.widget_info(|| egui::WidgetInfo::selected(
                        egui::WidgetType::RadioButton, response.enabled(), selected, label,
                    ));
                    icons::paint_focus(ui, &response);
                    if response.clicked() && !selected {
                        value[key] = json!(enabled);
                        response.mark_changed();
                        changed = Some(response);
                    }
                }
            });
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn style_angle_dials_preview_native_sources_commit_once_and_type_exact_degrees() {
        for depth in [8, 16] {
            for (kind, key) in [
                ("drop_shadow", "angle"),
                ("inner_shadow", "angle"),
                ("satin", "angle"),
                ("bevel_emboss", "angle"),
                ("gradient_overlay", "angle"),
                ("texture", "angle"),
                ("texture", "bitmap_angle"),
                ("pattern_overlay", "bitmap_angle"),
            ] {
                let ctx = egui::Context::default();
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                for y in 7..22 {
                    for x in 6..21 {
                        engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
                            x,
                            y,
                            [
                                12347,
                                23459 + x as u16 * 13,
                                34571,
                                if (x + y) % 3 == 0 { 32769 } else { 65535 },
                            ],
                        );
                    }
                }
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let mut settings = crate::effects::styles::defaults(kind);
                if settings.get("distance").is_some() {
                    settings["distance"] = json!(4.);
                }
                if settings.get("size").is_some() {
                    settings["size"] = json!(1.);
                }
                if settings.get("angle").is_some() {
                    settings["angle"] = json!(6.75);
                }
                if key == "bitmap_angle" {
                    use base64::{engine::general_purpose::STANDARD, Engine as _};
                    let words = [
                        65535, 0, 10001, 65535, 0, 65535, 30003, 65535, 50007, 10001, 0, 65535,
                        10001, 30003, 50007, 32769,
                    ];
                    let png = if depth == 16 {
                        crate::raster::png16(2, 2, &words)
                    } else {
                        crate::raster::png(
                            2,
                            2,
                            &words
                                .into_iter()
                                .map(crate::raster::project16)
                                .collect::<Vec<_>>(),
                        )
                    }
                    .unwrap();
                    settings = json!({"pattern":"bitmap","bitmap_png":STANDARD.encode(png),"sampling":"nearest","bitmap_scale":3.});
                    if kind == "texture" {
                        settings["angle"] = json!(6.75);
                    }
                }
                settings[key] = json!(17.25);
                engine
                    .edit(
                        "human",
                        &[json!({"op":"effect.add","layer":layer,"kind":kind,"settings":settings})],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                let before = engine.resident_state().expect("resident fixture").document.clone();
                assert_eq!(before.bit_depth, depth);
                let effect = before.layers[0].effects[0].id.clone();
                let shared = Arc::new(std::sync::Mutex::new(engine));
                let mut app = PeerBrush::init(
                    &ctx,
                    shared.clone(),
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
                app.effect_selected = Some((layer, false, effect));
                let mut time = 0.;
                let mut draw = |app: &mut PeerBrush, events| {
                    time += 0.1;
                    ctx.run(
                        egui::RawInput {
                            screen_rect: Some(Rect::from_min_size(
                                Pos2::ZERO,
                                Vec2::new(1100., 1800.),
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
                let number = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == "17.25°" => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()).left_center())
                        }
                        _ => None,
                    })
                    .expect("Dedicated exact-angle control");
                let center = number - Vec2::new(25., 0.);
                let start = center + Vec2::new(11., 0.);
                let end = center - Vec2::new(0., 11.);
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
                    .expect("Angle draft")
                    .commands
                    .clone();
                let draft =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                assert_eq!(
                    draft.layers[0].effects[0].settings[key],
                    json!(if key == "bitmap_angle" { -90. } else { 90. }),
                    "{depth}/{kind}/{key}"
                );
                assert_ne!(
                    draft.export_png().unwrap(),
                    before.export_png().unwrap(),
                    "{depth}/{kind}/{key}"
                );
                assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(
                    serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                draw(&mut app, vec![button(end, false)]);
                {
                    let mut engine = shared.lock().unwrap();
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        draft.export_png().unwrap()
                    );
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    engine.undo("human").unwrap();
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        before.export_png().unwrap()
                    );
                    engine.redo("human").unwrap();
                    assert_eq!(
                        engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                        draft.export_png().unwrap()
                    );
                }
                // Pointer focus can open the exact editor through Enter without a mouse double-click.
                draw(
                    &mut app,
                    vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                );
                assert!(app
                    .parameter_gesture
                    .as_ref()
                    .is_some_and(|edit| edit.typing.is_some()));
                draw(
                    &mut app,
                    vec![
                        egui::Event::Key {
                            key: egui::Key::Enter,
                            physical_key: None,
                            pressed: false,
                            repeat: false,
                            modifiers: Default::default(),
                        },
                        egui::Event::Text("13.1234567890123".into()),
                    ],
                );
                assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), 1);
                draw(
                    &mut app,
                    vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }],
                );
                let mut engine = shared.lock().unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings[key].as_f64(),
                    Some(13.1234567890123)
                );
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 2);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                engine.undo("human").unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    draft.export_png().unwrap()
                );
            }
        }
    }
    #[test]
    fn held_style_parameters_require_fresh_press_after_cancellation() {
        for depth in [8, 16] {
            for interruption in ["revision", "identity", "escape", "focus", "focus_return"] {
                let ctx = egui::Context::default();
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                engine.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(12, 16, [12347, 23459, 34571, 65535]);
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"effect.add","layer":layer,"kind":"color_overlay"})],
                        None,
                        None,
                        "Fixture",
                    )
                    .unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
                let shared = Arc::new(std::sync::Mutex::new(engine));
                let mut app = PeerBrush::init(
                    &ctx,
                    shared.clone(),
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
                app.effect_selected = Some((layer.clone(), false, effect));
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
                    .unwrap();
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
                let moved = start - Vec2::new(25., 0.);
                draw(&mut app, vec![egui::Event::PointerMoved(moved)]);
                draw(&mut app, vec![egui::Event::PointerMoved(moved)]);
                assert!(app.parameter_gesture.is_some());
                let mut events = vec![];
                match interruption {
                    "revision" => {
                        shared
                            .lock()
                            .unwrap()
                            .edit(
                                "human",
                                &[json!({"op":"layer.update","layer":layer,"name":"Later work"})],
                                None,
                                None,
                                "Later edit",
                            )
                            .unwrap();
                    }
                    "identity" => {
                        shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
                    }
                    "escape" => events.push(egui::Event::Key {
                        key: egui::Key::Escape,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default(),
                    }),
                    "focus" => events.push(egui::Event::WindowFocused(false)),
                    _ => events.extend([
                        egui::Event::WindowFocused(false),
                        egui::Event::WindowFocused(true),
                    ]),
                }
                let later = shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                let history = shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                draw(&mut app, events);
                let late = moved - Vec2::new(22., 0.);
                draw(
                    &mut app,
                    vec![
                        egui::Event::WindowFocused(true),
                        egui::Event::PointerMoved(late),
                    ],
                );
                draw(&mut app, vec![egui::Event::PointerMoved(late)]);
                assert!(
                    app.parameter_gesture.is_none(),
                    "{depth}/{interruption}: held gesture rebound"
                );
                draw(&mut app, vec![button(late, false)]);
                assert_eq!(
                    serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&later).unwrap()
                );
                assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
                // A new physical press may start another rendered preview.
                draw(&mut app, vec![]);
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(start), button(start, true)],
                );
                draw(&mut app, vec![egui::Event::PointerMoved(moved)]);
                draw(&mut app, vec![egui::Event::PointerMoved(moved)]);
                let commands = app
                    .parameter_gesture
                    .as_ref()
                    .expect("Fresh retry")
                    .commands
                    .clone();
                let draft = crate::engine::Engine::preview_edits(later.clone(), &commands).unwrap();
                assert_ne!(draft.export_png().unwrap(), later.export_png().unwrap());
                assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
                draw(&mut app, vec![button(moved, false)]);
                let mut engine = shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 1);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    draft.export_png().unwrap()
                );
                engine.undo("human").unwrap();
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                    later.export_png().unwrap()
                );
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    later.layers[0].pixels.rgba16()
                );
            }
        }
    }
    #[test]
    fn style_opacity_drag_previews_native_pixels_and_commits_one_undo() {
        for depth in [8, 16] {
            for kind in ["color_overlay", "drop_shadow", "pattern_overlay", "texture"] {
                let ctx = egui::Context::default();
                let mut e = crate::engine::Engine::new();
                e.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
                for y in 8..24 {
                    for x in 8..24 {
                        e.resident_state_mut().expect("resident fixture").document.layers[0]
                            .pixels
                            .set16(x, y, [10001, 30003, 50007, 65535]);
                    }
                }
                let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let mut command = json!({"op":"effect.add","layer":layer,"kind":kind});
                if matches!(kind, "pattern_overlay" | "texture") {
                    use base64::{engine::general_purpose::STANDARD, Engine as _};
                    let words: Vec<u16> = if kind == "texture" {
                        vec![
                            57313, 16317, 29119, 65535, 11317, 52319, 29123, 65535, 31721, 13723,
                            59127, 65535,
                        ]
                    } else {
                        vec![57313, 16317, 29119, 65535]
                    };
                    let width = (words.len() / 4) as u32;
                    let png = if depth == 16 {
                        crate::raster::png16(width, 1, &words).unwrap()
                    } else {
                        crate::raster::png(
                            width,
                            1,
                            &words
                                .into_iter()
                                .map(crate::raster::project16)
                                .collect::<Vec<_>>(),
                        )
                        .unwrap()
                    };
                    command["settings"] =
                        json!({"pattern":"bitmap","bitmap_png":STANDARD.encode(png)});
                }
                e.edit("human", &[command], None, None, "Style").unwrap();
                e.resident_state_mut().expect("resident fixture").undo.clear();
                let before = e.resident_state().expect("resident fixture").document.clone();
                let effect = e.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
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
                    assert!(e.resident_state().expect("resident fixture").undo.is_empty());
                    assert_eq!(e.resident_state().expect("resident fixture").document.revision, before.revision);
                    assert_eq!(
                        e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                }
                draw(&mut app, vec![button(end, false)]);
                let mut e = shared.lock().unwrap();
                assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
                assert_eq!(native(&e.resident_state().expect("resident fixture").document), native(&draft));
                assert_eq!(
                    e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16(),
                    before.layers[0].pixels.rgba16()
                );
                e.undo("human").unwrap();
                assert_eq!(native(&e.resident_state().expect("resident fixture").document), native(&before));
            }
        }
    }
}

#[cfg(test)]
mod bevel_tests {
    use super::*;
    #[test]
    fn independent_style_ranges_preview_native_pixels_and_commit_once() {
        for depth in [8, 16] {
            for (kind, key, settings, lower) in [
                (
                    "bevel_emboss",
                    "highlight_opacity",
                    json!({"highlight_opacity":0.37,"shadow_opacity":0.61,"size":0.,"soften":0.,"depth":1.,"angle":0.,"altitude":0.}),
                    0.,
                ),
                (
                    "gradient_overlay",
                    "offset_x",
                    json!({"offset_x":0.37,"angle":0.,"opacity":1.,"color":[0.,0.,0.],"color2":[1.,1.,1.]}),
                    -1.,
                ),
            ] {
                for (delta, endpoint) in [(-300., lower), (300., 1.)] {
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
                    engine.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":kind,"settings":settings})], None, None, "Bevel").unwrap();
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
                            egui::Shape::Text(t) if t.galley.job.text == "37%" => {
                                Some(Rect::from_min_size(t.pos, t.galley.size()).center())
                            }
                            _ => None,
                        })
                        .expect("Compact independent style range");
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
                        .expect("Valid independent style gesture")
                        .commands
                        .clone();
                    let draft =
                        crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                    assert_eq!(draft.layers[0].effects[0].settings[key], json!(endpoint));
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
}
