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
    hex_control: Option<egui::Id>,
    last_control: Option<egui::Id>,
}
fn pixel(rgba: [f64; 4]) -> Pixel {
    rgba.map(|n| (n.clamp(0., 1.) * 255.).round() as u8)
}
pub(super) fn format_hex(rgba: [f64; 4], native_hex: bool) -> String {
    if !native_hex {
        return color::format_hex(pixel(rgba));
    }
    let words = rgba.map(|n| (n.clamp(0., 1.) * 65535.).round() as u16);
    if words.into_iter().all(|word| word % 257 == 0) {
        return color::format_hex(pixel(rgba));
    }
    let [r, g, b, a] = words;
    if a == u16::MAX {
        format!("{r:04X}{g:04X}{b:04X}")
    } else {
        format!("{r:04X}{g:04X}{b:04X}{a:04X}")
    }
}

pub(super) fn parse_hex(text: &str, native_hex: bool, with_alpha: bool) -> Option<([f64; 3], Option<f64>)> {
    let digits = text.trim().trim_start_matches('#');
    if native_hex && (digits.len() == 12 || with_alpha && digits.len() == 16) {
        if !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return None;
        }
        let word = |channel: usize| {
            let start = channel * 4;
            u16::from_str_radix(&digits[start..start + 4], 16)
                .ok().map(|value| f64::from(value) / 65535.)
        };
        let alpha = if digits.len() == 16 { Some(word(3)?) } else { None };
        return Some(([word(0)?, word(1)?, word(2)?], alpha));
    }
    if !with_alpha && digits.len() == 8 {
        return None;
    }
    let [r, g, b, a] = color::parse_hex(text)?.map(|value| f64::from(value) / 255.);
    Some(([r, g, b], (digits.len() == 8).then_some(a)))
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
    alpha_label: Option<&str>,
    native_hex: bool,
) -> Option<egui::Response> {
    let parse = |text: &str| parse_hex(text, native_hex, alpha_label.is_some());
    let formats = match (native_hex, alpha_label.is_some()) {
        (true, true) => "Use 3, 6, 8, 12 or 16 HEX digits",
        (true, false) => "Use 3, 6 or 12 HEX digits",
        (false, true) => "Use 3, 6 or 8 HEX digits",
        (false, false) => "Use 3 or 6 HEX digits",
    };
    ui.push_id(id, |ui| {
        let state_id = ui.id().with("color picker");
        let input = [rgb[0], rgb[1], rgb[2], *alpha];
        let channels = if alpha_label.is_some() { "RGB, alpha and HEX" } else { "RGB and HEX" };
        let button = swatch(ui, input, Vec2::new(48., 22.))
            .on_hover_text(format!("Edit {label} color · {channels}"));
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
                    hex: format_hex(input, native_hex),
                    field: None,
                    hex_start: input,
                    hex_control: None,
                    last_control: None,
                })
            };
        }
        let Some(mut picker) = state else {
            ui.ctx().data_mut(|data| data.remove::<Picker>(state_id));
            return None;
        };
        // A late shared preview must not replace text before HEX validation.
        let editing_hex = picker
            .hex_control
            .is_some_and(|control| controls::number_editing(ui.ctx(), control));
        if picker.rgba != input {
            picker.rgba = input;
            picker.hsv = hsv(input);
            if !editing_hex {
                picker.hex = format_hex(input, native_hex);
            }
        }
        let before = picker.rgba;
        let mut open = true;
        let mut changed = None;
        let mut cancelled_hex = None;
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
                let before_rgb = picker.hsv.to_rgb();
                if let Some(response) = color::picker(ui, &mut picker.hsv, &mut picker.field, 160., |hsv| {
                    let [r, g, b] = hsv.to_rgb().map(|value| (value * 255.).round() as u8);
                    [r, g, b, 255]
                }) {
                    let rgb = picker.hsv.to_rgb();
                    if rgb != before_rgb {
                        picker.rgba[..3].copy_from_slice(&rgb.map(f64::from));
                    }
                    changed = Some(response);
                }
                if let Some(alpha_label) = alpha_label {
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
                }
                if picker.rgba != before && !editing_hex {
                    picker.hex = format_hex(picker.rgba, native_hex);
                }
                ui.horizontal(|ui| {
                    let label = ui.label("HEX");
                    let control = ui.id().with("hex");
                    picker.hex_control = Some(control);
                    let editor = control.with("number");
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut picker.hex)
                            .id(editor)
                            .frame(false)
                            .desired_width(140.),
                    ).labelled_by(label.id).on_hover_text(formats);
                    let mut response = ui.interact(edit.rect, control, egui::Sense::hover());
                    if edit.has_focus() {
                        if !controls::number_editing(ui.ctx(), control) {
                            picker.hex_start = picker.rgba;
                        }
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(editor, picker.hex.clone()));
                    }
                    if edit.changed() {
                        if let Some((rgb, alpha)) = parse(&picker.hex) {
                            picker.rgba[..3].copy_from_slice(&rgb);
                            if let Some(alpha) = alpha {
                                picker.rgba[3] = alpha;
                            }
                            picker.hsv = hsv(picker.rgba);
                            response.mark_changed();
                        }
                    }
                    if edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ui.ctx().data_mut(|data| data.remove::<String>(editor));
                        edit.surrender_focus();
                        if parse(&picker.hex).is_none() {
                            ui.ctx().data_mut(|data| {
                                data.insert_temp(control.with("cancel parameter"), true)
                            });
                            cancelled_hex = Some(control);
                            changed = Some(response.clone());
                        }
                    }
                    if response.changed() || controls::number_editing(ui.ctx(), control) {
                        changed = Some(response);
                    }
                });
                if parse(&picker.hex).is_none() {
                    ui.label(
                        RichText::new(formats)
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
        if let Some(control) = cancelled_hex {
            // Later precise controls cannot replace this gesture's cancellation.
            picker.rgba = picker.hex_start;
            picker.hsv = hsv(picker.rgba);
            changed = Some(cancel_response(&button, control));
        }
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
            for action in [
                "enter",
                "escape",
                "invalid",
                "invalid_blur",
                "invalid_channels",
                "source",
            ] {
                let ctx = egui::Context::default();
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(8, 8, depth).unwrap();
                engine.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(2, 3, [12347, 23459, 34571, 32769]);
                let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let rgba = [
                    0.171234567890123,
                    0.371234567890123,
                    0.731234567890123,
                    0.61234567890123,
                ];
                engine.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"color_overlay","settings":{"color":[rgba[0],rgba[1],rgba[2]],"opacity":rgba[3]}})],None,None,"Fixture").unwrap();
                engine.resident_state_mut().expect("resident fixture").undo.clear();
                let before = engine.resident_state().expect("resident fixture").document.clone();
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
                let mut output = draw(&mut app, vec![]);
                if action == "invalid_channels" {
                    let at = output
                        .shapes
                        .iter()
                        .find_map(|shape| match &shape.shape {
                            egui::Shape::Text(text)
                                if text.galley.job.text == "Precise channels" =>
                            {
                                Some(Rect::from_min_size(text.pos, text.galley.size()).center())
                            }
                            _ => None,
                        })
                        .expect("Actual precise-channel expander");
                    draw(
                        &mut app,
                        vec![egui::Event::PointerMoved(at), button(at, true)],
                    );
                    draw(&mut app, vec![button(at, false)]);
                    draw(&mut app, vec![]);
                    draw(&mut app, vec![]);
                    output = draw(&mut app, vec![]);
                }
                let hex = format_hex(rgba, true);
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
                let entered = if depth == 16 && action == "enter" { "F001400380058007" } else { "F0408080" };
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
                        egui::Event::Text(entered.into()),
                    ],
                );
                let commands = app
                    .parameter_gesture
                    .as_ref()
                    .expect("Source-bound HEX draft")
                    .commands
                    .clone();
                let control = app.parameter_gesture.as_ref().unwrap().control;
                let editor = control.with("number");
                assert_eq!(ctx.memory(|memory| memory.focused()), Some(editor));
                assert_eq!(
                    ctx.data(|data| data.get_temp::<String>(editor)).as_deref(),
                    Some(entered),
                    "{depth}/{action}: actual HEX text must own the draft"
                );
                let draft =
                    crate::engine::Engine::preview_edits(before.clone(), &commands).unwrap();
                if depth == 16 && action == "enter" {
                    let settings = &draft.layers[0].effects[0].settings;
                    for (value, word) in settings["color"].as_array().unwrap().iter().zip([0xF001u16, 0x4003, 0x8005]) {
                        assert_eq!(value.as_f64(), Some(f64::from(word) / 65535.));
                    }
                    assert_eq!(settings["opacity"].as_f64(), Some(f64::from(0x8007u16) / 65535.));
                }
                assert_ne!(draft.export_png().unwrap(), before.export_png().unwrap());
                assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                assert_eq!(
                    serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
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
                    expected = shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    1
                } else {
                    0
                };
                let mut blur_at = None;
                if action.starts_with("invalid") {
                    // Hold committed source in the settings while the actual
                    // shared-engine result is pending, then deliver it only
                    // after invalid text exists. No worker timing is assumed.
                    app.parameter_gesture.as_mut().unwrap().preview = None;
                    draw(&mut app, vec![key(egui::Key::A, false, Default::default())]);
                    app.parameter_gesture.as_mut().unwrap().preview = None;
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
                    assert_eq!(ctx.memory(|memory| memory.focused()), Some(editor));
                    assert_eq!(
                        ctx.data(|data| data.get_temp::<String>(editor)).as_deref(),
                        Some("invalid"),
                        "{depth}: invalid text must reach the actual focused editor"
                    );
                    app.parameter_gesture.as_mut().unwrap().preview = Some(Ok(draft.clone()));
                    let output = draw(&mut app, vec![]);
                    assert!(
                        output.shapes.iter().any(|shape| matches!(
                            &shape.shape,
                            egui::Shape::Text(text) if text.galley.job.text == "invalid"
                        )),
                        "{depth}: late preview must not replace the rendered HEX draft"
                    );
                    assert_eq!(
                        ctx.data(|data| data.get_temp::<String>(editor)).as_deref(),
                        Some("invalid")
                    );
                    assert_eq!(ctx.memory(|memory| memory.focused()), Some(editor));
                    assert!(controls::number_editing(&ctx, control));
                    assert_eq!(app.parameter_gesture.as_ref().unwrap().commands, commands);
                    assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                    assert_eq!(
                        serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                        serde_json::to_value(&before).unwrap()
                    );
                    if action == "invalid_blur" {
                        blur_at = Some(
                            output
                                .shapes
                                .iter()
                                .find_map(|shape| match &shape.shape {
                                    egui::Shape::Mesh(mesh) => {
                                        let rect = mesh.calc_bounds();
                                        ((rect.width() - 160.).abs() < 0.01
                                            && (rect.height() - 160.).abs() < 0.01)
                                            .then_some(
                                                rect.min + rect.size() * Vec2::new(0.13, 0.29),
                                            )
                                    }
                                    _ => None,
                                })
                                .expect("Actual color-field control for invalid HEX blur"),
                        );
                    } else if action == "invalid_channels" {
                        // Font centers need not coincide with the painted bar.
                        // F0 from the valid HEX draft displays as 94.1176% red.
                        let bars = output
                            .shapes
                            .iter()
                            .filter_map(|bar| {
                                let egui::Shape::Rect(shape) = &bar.shape else {
                                    return None;
                                };
                                let rect = shape.rect;
                                if shape.fill != Color32::from_rgb(54, 51, 58)
                                    || (rect.height() - 22.).abs() >= 0.01
                                {
                                    return None;
                                }
                                let red_label = output.shapes.iter().any(|label| {
                                    let egui::Shape::Text(text) = &label.shape else {
                                        return false;
                                    };
                                    let label_rect =
                                        Rect::from_min_size(text.pos, text.galley.size());
                                    label.clip_rect == bar.clip_rect
                                        && text.galley.job.text == "R"
                                        && label_rect.right() < rect.left()
                                        && rect.top() <= label_rect.center().y
                                        && label_rect.center().y <= rect.bottom()
                                });
                                let red_value = output.shapes.iter().any(|value| {
                                    let egui::Shape::Text(text) = &value.shape else {
                                        return false;
                                    };
                                    value.clip_rect == bar.clip_rect
                                        && text.galley.job.text == "94.1176%"
                                        && rect.contains(
                                            Rect::from_min_size(text.pos, text.galley.size())
                                                .center(),
                                        )
                                });
                                (red_label && red_value).then_some(rect)
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            bars.len(),
                            1,
                            "Unique actual precise red-channel bar for invalid HEX blur"
                        );
                        blur_at = Some(bars[0].center());
                    }
                }
                if let Some(at) = blur_at {
                    draw(
                        &mut app,
                        vec![egui::Event::PointerMoved(at), button(at, true)],
                    );
                    if action == "invalid_channels" {
                        let response = ctx
                            .memory(|memory| memory.focused())
                            .and_then(|id| ctx.read_response(id))
                            .expect("Precise red-channel control received actual focus");
                        assert_ne!(response.id, editor);
                        assert!(response.sense.senses_drag() && response.rect.contains(at));
                        assert_eq!(
                            response.layer_id,
                            ctx.read_response(editor)
                                .expect("HEX editor in the same picker")
                                .layer_id
                        );
                    }
                    draw(&mut app, vec![button(at, false)]);
                } else {
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
                }
                draw(&mut app, vec![]);
                assert!(!controls::number_editing(&ctx, control), "{depth}/{action}");
                assert!(app.parameter_gesture.is_none(), "{depth}/{action}");
                let mut engine = shared.lock().unwrap();
                if action == "enter" {
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
                } else {
                    if engine.resident_state().expect("resident fixture").undo.len() != history {
                        eprintln!(
                            "\nPB_EFFECT_COLOR_HISTORY depth={depth} action={action} before={history} after={}",
                            engine.resident_state().expect("resident fixture").undo.len(),
                            action = if action.starts_with("invalid") { "invalid" } else { action }
                        );
                    }
                    assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history, "{depth}/{action}");
                    assert_eq!(
                        serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
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
            engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(32, 32, depth).unwrap();
            for y in 8..24 {
                for x in 7..23 {
                    engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(
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
            let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
            let rgb = [0.171234567890123, 0.371234567890123, 0.731234567890123];
            let alpha = 0.61234567890123;
            engine.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"color_overlay","settings":{"color":rgb,"opacity":alpha}})],None,None,"Fixture").unwrap();
            engine.resident_state_mut().expect("resident fixture").undo.clear();
            let before = engine.resident_state().expect("resident fixture").document.clone();
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
            assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
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
            assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
                serde_json::to_value(&before).unwrap()
            );
            draw(&mut app, vec![button(end, false)]);
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
            let restored = crate::psd::decode(&crate::psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap()).unwrap();
            assert_eq!(
                restored.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(
                restored.layers[0].effects[0].settings,
                engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings
            );
            assert_eq!(
                restored.export_png().unwrap(),
                engine.resident_state().expect("resident fixture").document.export_png().unwrap()
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
    }
}
