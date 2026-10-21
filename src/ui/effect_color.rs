//! Dedicated effect color controls. Float values are retained; pixels are display only.
use super::*;

#[derive(Clone)]
struct Picker {
    rgba: [f64; 4],
    original: [f64; 4],
    hsv: egui::ecolor::Hsva,
    hex: String,
    field: Option<(u16, TextureHandle)>,
    hex_start: [f64; 4],
    last_control: Option<egui::Id>,
}
fn pixel(rgba: [f64; 4]) -> Pixel {
    rgba.map(|n| (n.clamp(0., 1.) * 255.).round() as u8)
}
fn hsv(rgba: [f64; 4]) -> egui::ecolor::Hsva {
    // These values are encoded working RGB, matching effect settings. HSV is
    // presentation input; it does not reinterpret or convert retained sources.
    egui::ecolor::Hsva::from_rgb([rgba[0] as f32, rgba[1] as f32, rgba[2] as f32])
}
fn swatch(ui: &mut egui::Ui, rgba: [f64; 4], size: Vec2) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    color::checker(ui.painter(), rect);
    let p = pixel(rgba);
    ui.painter().rect_filled(
        rect,
        2,
        Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]),
    );
    if response.has_focus() {
        ui.painter().rect_stroke(
            rect,
            2,
            Stroke::new(1.5, ui.visuals().selection.stroke.color),
            egui::StrokeKind::Inside,
        );
    }
    response
}
fn cancel_response(button: &egui::Response, control: egui::Id) -> egui::Response {
    // Cancellation is a receipt, not another registered widget on a different layer.
    let mut response = button.clone();
    response.id = control;
    response
}
pub(super) fn control(
    ui: &mut egui::Ui,
    id: impl std::hash::Hash,
    label: &str,
    rgb: &mut [f64; 3],
    alpha: &mut f64,
    alpha_label: &str,
) -> Option<egui::Response> {
    ui.push_id(id, |ui| {
        let state_id = ui.id().with("color picker");
        let input = [rgb[0], rgb[1], rgb[2], *alpha];
        let button = swatch(ui, input, Vec2::new(48., 22.))
            .on_hover_text(format!("Edit {label} color · RGB, alpha and HEX"));
        button.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Button,
                ui.is_enabled(),
                format!("{label} color"),
            )
        });
        let mut state = ui.ctx().data(|data| data.get_temp::<Picker>(state_id));
        if button.clicked() && state.is_some() {
            let closed = state.take().unwrap();
            ui.ctx().data_mut(|data| data.remove::<Picker>(state_id));
            if let Some(control) = closed.last_control {
                controls::cancel_number_edit(ui.ctx(), control);
                ui.ctx()
                    .data_mut(|data| data.insert_temp(control.with("cancel parameter"), true));
                return Some(cancel_response(&button, control));
            }
            return None;
        }
        if button.clicked() {
            state = if state.is_some() {
                None
            } else {
                Some(Picker {
                    rgba: input,
                    original: input,
                    hsv: hsv(input),
                    hex: color::format_hex(pixel(input)),
                    field: None,
                    hex_start: input,
                    last_control: None,
                })
            };
        }
        let Some(mut picker) = state else {
            ui.ctx().data_mut(|data| data.remove::<Picker>(state_id));
            return None;
        };
        if picker.rgba != input {
            picker.rgba = input;
            picker.hsv = hsv(input);
            picker.hex = color::format_hex(pixel(input));
        }
        let before = picker.rgba;
        let mut open = true;
        let mut changed = None;
        let enabled = ui.is_enabled();
        egui::Window::new(format!("{label} color"))
            .id(state_id)
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(button.rect.right_top() + Vec2::new(8., 0.))
            .show(ui.ctx(), |ui| {
                if !enabled {
                    ui.disable();
                }
                ui.spacing_mut().item_spacing = Vec2::new(6., 3.);
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let previous = swatch(ui, picker.original, Vec2::new(90., 22.));
                        controls::label(ui, "Previous");
                        if previous.clicked() && picker.rgba != picker.original {
                            picker.rgba = picker.original;
                            picker.hsv = hsv(picker.rgba);
                            changed = Some(previous);
                        }
                    });
                    ui.vertical(|ui| {
                        swatch(ui, picker.rgba, Vec2::new(90., 22.));
                        controls::label(ui, "Current");
                    });
                });
                let hue = (picker.hsv.h * 65535.).round() as u16;
                if picker.field.as_ref().map(|(key, _)| *key) != Some(hue) {
                    let bytes = (0..96)
                        .flat_map(|y| {
                            (0..96).flat_map(move |x| {
                                let c = egui::ecolor::Hsva::new(
                                    f32::from(hue) / 65535.,
                                    x as f32 / 95.,
                                    1. - y as f32 / 95.,
                                    1.,
                                )
                                .to_rgb();
                                [
                                    (c[0] * 255.).round() as u8,
                                    (c[1] * 255.).round() as u8,
                                    (c[2] * 255.).round() as u8,
                                    255,
                                ]
                            })
                        })
                        .collect::<Vec<_>>();
                    let image = egui::ColorImage::from_rgba_unmultiplied([96, 96], &bytes);
                    if let Some((key, texture)) = &mut picker.field {
                        *key = hue;
                        texture.set(image, egui::TextureOptions::LINEAR);
                    } else {
                        picker.field = Some((
                            hue,
                            ui.ctx().load_texture(
                                "effect color field",
                                image,
                                egui::TextureOptions::LINEAR,
                            ),
                        ));
                    }
                }
                ui.horizontal(|ui| {
                    let (rect, response) =
                        ui.allocate_exact_size(Vec2::splat(160.), egui::Sense::click_and_drag());
                    if response.clicked() || response.is_pointer_button_down_on() {
                        response.request_focus();
                    }
                    if let Some((_, texture)) = &picker.field {
                        ui.painter().image(
                            texture.id(),
                            rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                            Color32::WHITE,
                        );
                    }
                    if response.is_pointer_button_down_on() || response.dragged() {
                        if let Some(p) = response.interact_pointer_pos() {
                            picker.hsv.s = ((p.x - rect.left()) / rect.width()).clamp(0., 1.);
                            picker.hsv.v = (1. - (p.y - rect.top()) / rect.height()).clamp(0., 1.);
                            let color = picker.hsv.to_rgb();
                            for c in 0..3 {
                                picker.rgba[c] = f64::from(color[c]);
                            }
                            changed = Some(response.clone());
                        }
                    }
                    if response.has_focus()
                        && !response.is_pointer_button_down_on()
                        && !response.dragged()
                    {
                        let (ds, dv) = ui.input(|i| {
                            let step = if i.modifiers.shift { 0.1 } else { 0.01 };
                            (
                                step * (i32::from(i.key_pressed(egui::Key::ArrowRight))
                                    - i32::from(i.key_pressed(egui::Key::ArrowLeft)))
                                    as f32,
                                step * (i32::from(i.key_pressed(egui::Key::ArrowUp))
                                    - i32::from(i.key_pressed(egui::Key::ArrowDown)))
                                    as f32,
                            )
                        });
                        if ds != 0. || dv != 0. {
                            picker.hsv.s = (picker.hsv.s + ds).clamp(0., 1.);
                            picker.hsv.v = (picker.hsv.v + dv).clamp(0., 1.);
                            let color = picker.hsv.to_rgb();
                            for c in 0..3 {
                                picker.rgba[c] = f64::from(color[c]);
                            }
                            changed = Some(response.clone());
                        }
                    }
                    ui.painter().circle_stroke(
                        Pos2::new(
                            rect.left() + picker.hsv.s * rect.width(),
                            rect.top() + (1. - picker.hsv.v) * rect.height(),
                        ),
                        4.,
                        Stroke::new(1.5, Color32::WHITE),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Slider,
                            ui.is_enabled(),
                            "Color saturation and brightness",
                        )
                    });
                    let (rail, response) =
                        ui.allocate_exact_size(Vec2::new(20., 160.), egui::Sense::click_and_drag());
                    if response.clicked() || response.is_pointer_button_down_on() {
                        response.request_focus();
                    }
                    for i in 0..96 {
                        let rgb = egui::ecolor::Hsva::new(i as f32 / 95., 1., 1., 1.).to_rgb();
                        ui.painter().rect_filled(
                            Rect::from_min_size(
                                rail.min + Vec2::new(0., i as f32 * rail.height() / 96.),
                                Vec2::new(rail.width(), rail.height() / 96. + 0.5),
                            ),
                            0,
                            Color32::from_rgb(
                                (rgb[0] * 255.).round() as u8,
                                (rgb[1] * 255.).round() as u8,
                                (rgb[2] * 255.).round() as u8,
                            ),
                        );
                    }
                    if response.is_pointer_button_down_on() || response.dragged() {
                        if let Some(p) = response.interact_pointer_pos() {
                            picker.hsv.h = ((p.y - rail.top()) / rail.height()).clamp(0., 1.);
                            let color = picker.hsv.to_rgb();
                            for c in 0..3 {
                                picker.rgba[c] = f64::from(color[c]);
                            }
                            changed = Some(response.clone());
                        }
                    }
                    if response.has_focus()
                        && !response.is_pointer_button_down_on()
                        && !response.dragged()
                    {
                        let delta = ui.input(|i| {
                            let step = if i.modifiers.shift { 0.1 } else { 0.01 };
                            step * (i32::from(
                                i.key_pressed(egui::Key::ArrowRight)
                                    || i.key_pressed(egui::Key::ArrowDown),
                            ) - i32::from(
                                i.key_pressed(egui::Key::ArrowLeft)
                                    || i.key_pressed(egui::Key::ArrowUp),
                            )) as f32
                        });
                        if delta != 0. {
                            picker.hsv.h = (picker.hsv.h + delta).rem_euclid(1.);
                            let color = picker.hsv.to_rgb();
                            for c in 0..3 {
                                picker.rgba[c] = f64::from(color[c]);
                            }
                            changed = Some(response.clone());
                        }
                    }
                    let y = rail.top() + picker.hsv.h * rail.height();
                    ui.painter().line_segment(
                        [
                            Pos2::new(rail.left() - 2., y),
                            Pos2::new(rail.right() + 2., y),
                        ],
                        Stroke::new(2., Color32::WHITE),
                    );
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Slider,
                            ui.is_enabled(),
                            "Color hue",
                        )
                    });
                });
                ui.horizontal(|ui| {
                    controls::label(ui, alpha_label);
                    let mut opacity = picker.rgba[3] as f32 * 100.;
                    let response =
                        controls::range(ui, "alpha", &mut opacity, 0.0..=100., 160., "%", 2, false);
                    if response.changed() || response.double_clicked() {
                        if response.changed() {
                            picker.rgba[3] = f64::from(opacity) / 100.;
                        }
                        changed = Some(response);
                    }
                });
                if picker.rgba != before {
                    picker.hex = color::format_hex(pixel(picker.rgba));
                }
                ui.horizontal(|ui| {
                    controls::label(ui, "HEX");
                    let control = ui.id().with("hex");
                    let editor = control.with("number");
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut picker.hex)
                            .id(editor)
                            .frame(false)
                            .desired_width(140.),
                    );
                    let mut response = ui.interact(edit.rect, control, egui::Sense::hover());
                    if edit.has_focus() {
                        if !controls::number_editing(ui.ctx(), control) {
                            picker.hex_start = picker.rgba;
                        }
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(editor, picker.hex.clone()));
                    }
                    if edit.changed() {
                        if let Some(p) = color::parse_hex(&picker.hex) {
                            for c in 0..3 {
                                picker.rgba[c] = f64::from(p[c]) / 255.;
                            }
                            if picker.hex.trim().trim_start_matches('#').len() == 8 {
                                picker.rgba[3] = f64::from(p[3]) / 255.;
                            }
                            picker.hsv = hsv(picker.rgba);
                            response.mark_changed();
                        }
                    }
                    if edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ui.ctx().data_mut(|data| data.remove::<String>(editor));
                        edit.surrender_focus();
                        if color::parse_hex(&picker.hex).is_none() {
                            picker.rgba = picker.hex_start;
                            ui.ctx().data_mut(|data| {
                                data.insert_temp(control.with("cancel parameter"), true)
                            });
                            changed = Some(response.clone());
                        }
                    }
                    if response.changed() || controls::number_editing(ui.ctx(), control) {
                        changed = Some(response);
                    }
                });
                if color::parse_hex(&picker.hex).is_none() {
                    ui.label(
                        RichText::new("Use 3, 6 or 8 HEX digits")
                            .size(11.)
                            .color(ACCENT),
                    );
                }
                egui::CollapsingHeader::new(
                    RichText::new("Precise channels").size(11.).color(MUTED),
                )
                .id_salt("channels")
                .show(ui, |ui| {
                    for (c, name) in ["R", "G", "B"].into_iter().enumerate() {
                        ui.horizontal(|ui| {
                            controls::label(ui, name);
                            let mut channel = picker.rgba[c] as f32 * 100.;
                            let response = controls::range(
                                ui,
                                c,
                                &mut channel,
                                0.0..=100.,
                                160.,
                                "%",
                                4,
                                false,
                            );
                            if response.changed() || response.double_clicked() {
                                if response.changed() {
                                    picker.rgba[c] = f64::from(channel) / 100.;
                                    picker.hsv = hsv(picker.rgba);
                                }
                                changed = Some(response);
                            }
                        });
                    }
                });
            });
        if let Some(response) = &mut changed {
            if picker.rgba != before {
                response.mark_changed();
            }
            picker.last_control = Some(response.id);
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) || !enabled {
            open = false;
        }
        if !open {
            if let Some(control) = picker.last_control {
                controls::cancel_number_edit(ui.ctx(), control);
                ui.ctx()
                    .data_mut(|data| data.insert_temp(control.with("cancel parameter"), true));
                changed = Some(cancel_response(&button, control));
            }
        }
        if open {
            ui.ctx()
                .data_mut(|data| data.insert_temp(state_id, picker.clone()));
        } else {
            ui.ctx().data_mut(|data| data.remove::<Picker>(state_id));
        }
        if changed.is_some() {
            *rgb = picker.rgba[..3].try_into().unwrap();
            *alpha = picker.rgba[3];
        }
        changed
    })
    .inner
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn effect_color_hex_is_live_provisional_and_cancels_on_escape_invalid_input_and_source_changes()
    {
        for depth in [8, 16] {
            for action in ["enter", "escape", "invalid", "source"] {
                let ctx = egui::Context::default();
                let mut engine = crate::engine::Engine::new();
                engine.doc = Document::new_depth(8, 8, depth).unwrap();
                engine.doc.layers[0]
                    .pixels
                    .set16(2, 3, [12347, 23459, 34571, 32769]);
                let layer = engine.doc.layers[0].id.clone();
                let rgba = [
                    0.171234567890123,
                    0.371234567890123,
                    0.731234567890123,
                    0.61234567890123,
                ];
                engine.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"color_overlay","settings":{"color":[rgba[0],rgba[1],rgba[2]],"opacity":rgba[3]}})],None,None,"Fixture").unwrap();
                engine.undo.clear();
                let before = engine.doc.clone();
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
                let p = pixel(rgba);
                let at = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect)
                            if (rect.rect.width() - 48.).abs() < 0.01
                                && (rect.rect.height() - 22.).abs() < 0.01
                                && rect.fill
                                    == Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]) =>
                        {
                            Some(rect.rect.center())
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
                    vec![egui::Event::PointerMoved(at), button(at, true)],
                );
                draw(&mut app, vec![button(at, false)]);
                draw(&mut app, vec![]);
                let output = draw(&mut app, vec![]);
                let hex = color::format_hex(p);
                let at = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Text(text) if text.galley.job.text == hex => {
                            Some(Rect::from_min_size(text.pos, text.galley.size()).center())
                        }
                        _ => None,
                    })
                    .expect("Editable HEX");
                draw(
                    &mut app,
                    vec![egui::Event::PointerMoved(at), button(at, true)],
                );
                draw(&mut app, vec![button(at, false)]);
                let key = |key, pressed, modifiers| egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers,
                };
                draw(
                    &mut app,
                    vec![
                        key(
                            egui::Key::A,
                            true,
                            egui::Modifiers {
                                ctrl: true,
                                command: true,
                                ..Default::default()
                            },
                        ),
                        egui::Event::Text("F0408080".into()),
                    ],
                );
                let commands = app
                    .parameter_gesture
                    .as_ref()
                    .expect("Source-bound HEX draft")
                    .commands
                    .clone();
                let draft =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                assert_ne!(draft.export_png().unwrap(), before.export_png().unwrap());
                assert!(shared.lock().unwrap().undo.is_empty());
                assert_eq!(
                    serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                let mut expected = before.clone();
                let history = if action == "source" {
                    shared
                        .lock()
                        .unwrap()
                        .edit(
                            "human",
                            &[json!({"op":"layer.update","layer":layer,"name":"Later work"})],
                            None,
                            None,
                            "Later work",
                        )
                        .unwrap();
                    expected = shared.lock().unwrap().doc.clone();
                    1
                } else {
                    0
                };
                if action == "invalid" {
                    draw(&mut app, vec![key(egui::Key::A, false, Default::default())]);
                    draw(
                        &mut app,
                        vec![
                            key(
                                egui::Key::A,
                                true,
                                egui::Modifiers {
                                    ctrl: true,
                                    command: true,
                                    ..Default::default()
                                },
                            ),
                            egui::Event::Text("invalid".into()),
                        ],
                    );
                }
                draw(
                    &mut app,
                    vec![key(
                        if action == "escape" {
                            egui::Key::Escape
                        } else {
                            egui::Key::Enter
                        },
                        true,
                        Default::default(),
                    )],
                );
                draw(&mut app, vec![]);
                let mut engine = shared.lock().unwrap();
                if action == "enter" {
                    assert_eq!(engine.undo.len(), 1);
                    assert_eq!(
                        engine.doc.export_png().unwrap(),
                        draft.export_png().unwrap()
                    );
                    assert_eq!(
                        engine.doc.layers[0].pixels.rgba16(),
                        before.layers[0].pixels.rgba16()
                    );
                    engine.undo("human").unwrap();
                    assert_eq!(
                        engine.doc.export_png().unwrap(),
                        before.export_png().unwrap()
                    );
                } else {
                    if engine.undo.len() != history {
                        eprintln!(
                            "\nPB_EFFECT_COLOR_HISTORY depth={depth} action={action} before={history} after={}",
                            engine.undo.len()
                        );
                    }
                    assert_eq!(engine.undo.len(), history, "{depth}/{action}");
                    assert_eq!(
                        serde_json::to_value(&engine.doc).unwrap(),
                        serde_json::to_value(&expected).unwrap(),
                        "{depth}/{action}"
                    );
                }
            }
        }
    }
    #[test]
    fn effect_color_pointer_previews_retain_exact_sources_and_commit_once() {
        for depth in [8, 16] {
            let ctx = egui::Context::default();
            let mut engine = crate::engine::Engine::new();
            engine.doc = Document::new_depth(32, 32, depth).unwrap();
            for y in 8..24 {
                for x in 7..23 {
                    engine.doc.layers[0].pixels.set16(
                        x,
                        y,
                        [
                            10001,
                            30003,
                            50007,
                            if (x + y) % 3 == 0 { 32769 } else { 65535 },
                        ],
                    );
                }
            }
            let layer = engine.doc.layers[0].id.clone();
            let rgb = [0.171234567890123, 0.371234567890123, 0.731234567890123];
            let alpha = 0.61234567890123;
            engine.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"color_overlay","settings":{"color":rgb,"opacity":alpha}})],None,None,"Fixture").unwrap();
            engine.undo.clear();
            let before = engine.doc.clone();
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
                time += 0.05;
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100., 1000.))),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            draw(&mut app, vec![]);
            let output = draw(&mut app, vec![]);
            let p = pixel([rgb[0], rgb[1], rgb[2], alpha]);
            let swatch = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(rect)
                        if (rect.rect.width() - 48.).abs() < 0.01
                            && (rect.rect.height() - 22.).abs() < 0.01
                            && rect.fill
                                == Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3]) =>
                    {
                        Some(rect.rect.center())
                    }
                    _ => None,
                })
                .expect("Compact effect swatch");
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            draw(
                &mut app,
                vec![egui::Event::PointerMoved(swatch), button(swatch, true)],
            );
            draw(&mut app, vec![button(swatch, false)]);
            draw(&mut app, vec![]);
            let output = draw(&mut app, vec![]);
            assert!(shared.lock().unwrap().undo.is_empty());
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            let field = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Mesh(mesh) => {
                        let rect = mesh.calc_bounds();
                        ((rect.width() - 160.).abs() < 0.01 && (rect.height() - 160.).abs() < 0.01)
                            .then_some(rect)
                    }
                    _ => None,
                })
                .expect("Dedicated color field");
            let start = field.min + field.size() * Vec2::new(0.17, 0.37);
            let end = field.min + field.size() * Vec2::new(0.638, 0.231);
            draw(
                &mut app,
                vec![egui::Event::PointerMoved(start), button(start, true)],
            );
            draw(&mut app, vec![egui::Event::PointerMoved(end)]);
            draw(&mut app, vec![egui::Event::PointerMoved(end)]);
            let commands = app
                .parameter_gesture
                .as_ref()
                .expect("Color preview gesture")
                .commands
                .clone();
            let draft = crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
            assert_ne!(draft.export_png().unwrap(), before.export_png().unwrap());
            assert_eq!(
                draft.layers[0].effects[0].settings["opacity"].as_f64(),
                Some(alpha)
            );
            assert!(
                draft.layers[0].effects[0].settings["color"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|c| (c.as_f64().unwrap() * 255.).fract().abs() > 0.00001),
                "Color input retains float precision beyond display bytes"
            );
            assert!(shared.lock().unwrap().undo.is_empty());
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            draw(&mut app, vec![button(end, false)]);
            let mut engine = shared.lock().unwrap();
            assert_eq!(engine.undo.len(), 1);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                draft.export_png().unwrap()
            );
            assert_eq!(
                engine.doc.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            let restored = crate::psd::decode(&crate::psd::encode(&engine.doc).unwrap()).unwrap();
            assert_eq!(
                restored.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(
                restored.layers[0].effects[0].settings,
                engine.doc.layers[0].effects[0].settings
            );
            assert_eq!(
                restored.export_png().unwrap(),
                engine.doc.export_png().unwrap()
            );
            engine.undo("human").unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
            engine.redo("human").unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                draft.export_png().unwrap()
            );
        }
    }
}
