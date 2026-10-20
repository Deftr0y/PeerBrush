//! Compact range controls: values sit in the filled bar, without a number box.
use eframe::egui::{self, Color32, Rect, RichText, Stroke, Vec2};
use std::{hash::Hash, ops::RangeInclusive};

pub fn range(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f32,
    limits: RangeInclusive<f32>,
    width: f32,
    suffix: &str,
    decimals: usize,
    logarithmic: bool,
) -> egui::Response {
    let color = ui.visuals().selection.stroke.color;
    let color = if color == Color32::from_rgb(233, 84, 32) {
        Color32::from_rgb(190, 70, 29)
    } else {
        color.gamma_multiply(0.85)
    };
    range_with_color(
        ui,
        id,
        value,
        limits,
        width,
        suffix,
        decimals,
        logarithmic,
        color,
    )
}

pub fn range_with_color(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f32,
    limits: RangeInclusive<f32>,
    width: f32,
    suffix: &str,
    decimals: usize,
    logarithmic: bool,
    color: Color32,
) -> egui::Response {
    ui.push_id(id, |ui| {
        let before = *value;
        let mut response = ui
            .scope(|ui| {
                ui.spacing_mut().slider_width = width.max(50.0);
                ui.spacing_mut().interact_size.y = 22.0;
                let visuals = ui.visuals_mut();
                for widget in [
                    &mut visuals.widgets.inactive,
                    &mut visuals.widgets.hovered,
                    &mut visuals.widgets.active,
                    &mut visuals.widgets.noninteractive,
                ] {
                    widget.bg_fill = Color32::TRANSPARENT;
                    widget.weak_bg_fill = Color32::TRANSPARENT;
                    widget.bg_stroke = Stroke::NONE;
                    widget.fg_stroke.color = Color32::TRANSPARENT;
                }
                let mut slider = egui::Slider::new(value, limits.clone())
                    // Preserve display-only fractional animation; clamp/round on an actual edit.
                    .clamping(egui::SliderClamping::Edits)
                    .show_value(false)
                    // The custom bar has no handle: use its full width for native input mapping.
                    .handle_shape(egui::style::HandleShape::Rect { aspect_ratio: 0.0 })
                    .logarithmic(logarithmic)
                    .max_decimals(decimals);
                if decimals == 0 && limits.start().fract() == 0. && limits.end().fract() == 0. {
                    slider = slider.step_by(1.);
                }
                ui.add(slider)
            })
            .inner
            .interact(egui::Sense::click_and_drag());
        // A click focuses the control; actual pointer movement edits it. This
        // avoids changing the value/history while opening the numeric editor.
        if (response.is_pointer_button_down_on() && !response.dragged())
            || (response.clicked() && !response.drag_stopped())
        {
            *value = before;
        }
        let rect = Rect::from_center_size(
            response.rect.center(),
            Vec2::new(response.rect.width(), 22.0),
        );
        let editor_id = response.id.with("number");
        if response.double_clicked() && ui.is_enabled() {
            *value = before;
            ui.ctx()
                .data_mut(|d| d.insert_temp(editor_id, value.to_string()));
        }
        let enabled = ui.is_enabled();
        let muted = if enabled { 1.0 } else { 0.4 };
        let painter = ui.painter();
        painter.rect_filled(
            rect,
            11,
            Color32::from_rgb(54, 51, 58).gamma_multiply(muted),
        );
        let (low, high) = (*limits.start(), *limits.end());
        let fraction = if high <= low {
            0.0
        } else if logarithmic && low > 0.0 {
            ((*value).clamp(low, high).ln() - low.ln()) / (high.ln() - low.ln())
        } else {
            (*value - low) / (high - low)
        }
        .clamp(0.0, 1.0);
        let fill = fraction * rect.width();
        if fill > 0.0 {
            painter.rect_filled(
                Rect::from_min_size(rect.min, Vec2::new(fill, rect.height())),
                11,
                color.gamma_multiply(muted),
            );
        }
        let editing = ui.ctx().data(|d| d.get_temp::<String>(editor_id));
        if let Some(mut text) = editing {
            let edit = ui.put(
                rect.shrink2(Vec2::new(7.0, 1.0)),
                egui::TextEdit::singleline(&mut text)
                    .id(editor_id)
                    .frame(false)
                    .font(egui::TextStyle::Body)
                    .horizontal_align(egui::Align::RIGHT),
            );
            if response.double_clicked() {
                edit.request_focus();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), editor_id) {
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(text.chars().count()),
                        )));
                    state.store(ui.ctx(), editor_id);
                }
            }
            let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let commit = edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
            if cancel || commit {
                if !cancel {
                    if let Ok(parsed) = text.trim().parse::<f32>() {
                        if parsed.is_finite() {
                            *value = parsed.clamp(low, high);
                        }
                    }
                }
                ui.ctx().data_mut(|d| d.remove::<String>(editor_id));
                edit.surrender_focus();
            } else {
                ui.ctx().data_mut(|d| d.insert_temp(editor_id, text));
            }
        } else {
            let text = format!("{value:.decimals$}{suffix}");
            let galley = painter.layout_no_wrap(
                text,
                egui::FontId::proportional(12.5),
                Color32::from_rgb(245, 242, 240).gamma_multiply(muted),
            );
            let x = (rect.left() + fill - galley.size().x - 8.0).clamp(
                rect.left() + 7.0,
                (rect.right() - galley.size().x - 7.0).max(rect.left() + 7.0),
            );
            painter.galley(
                egui::pos2(x, rect.center().y - galley.size().y * 0.5),
                galley,
                Color32::WHITE,
            );
        }
        if *value != before {
            response.mark_changed();
        }
        response.on_hover_text("Drag to change · Double-click to type · Arrow keys to adjust")
    })
    .inner
}

pub fn numeric<N: egui::emath::Numeric>(
    ui: &mut egui::Ui,
    value: &mut N,
    limits: RangeInclusive<N>,
) -> egui::Response {
    ui.scope(|ui| {
        let visuals = ui.visuals_mut();
        for widget in [
            &mut visuals.widgets.inactive,
            &mut visuals.widgets.hovered,
            &mut visuals.widgets.active,
        ] {
            widget.bg_fill = Color32::TRANSPARENT;
            widget.weak_bg_fill = Color32::TRANSPARENT;
            widget.bg_stroke = Stroke::NONE;
        }
        ui.add(egui::DragValue::new(value).range(limits))
    })
    .inner
}

// Preserve in-range endpoints and exact fractional values. Wrap only actual edits.
fn wrapped_angle(value: f64) -> f64 {
    if (-180. ..=180.).contains(&value) {
        value
    } else {
        (value + 180.).rem_euclid(360.) - 180.
    }
}

/// Direction dial with unboxed, exact degree entry. Clockwise tile angles keep
/// their renderer convention; lighting/gradient angles increase anticlockwise.
pub fn angle(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f64,
    clockwise: bool,
    name: &str,
    help: &str,
) -> egui::Response {
    ui.push_id(id, |ui| {
        let before = *value;
        let width = ui.available_width().clamp(96., 160.);
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, 36.), egui::Sense::click_and_drag());
        let dial = Rect::from_min_size(rect.min, Vec2::splat(36.));
        let center = dial.center();
        let number = Rect::from_min_max(rect.min + Vec2::new(43., 6.), rect.max - Vec2::new(0., 6.));
        let editor_id = response.id.with("number");
        let origin_id = response.id.with("angle origin");
        let editing = ui.ctx().data(|data| data.get_temp::<String>(editor_id));
        let interactive = ui.is_enabled() && ui.input(|i| i.focused && !i.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false))) && !i.key_pressed(egui::Key::Escape));
        if interactive && editing.is_none() {
            if response.clicked() || response.is_pointer_button_down_on() {
                response.request_focus();
            }
            if response.double_clicked() {
                ui.ctx().data_mut(|data| data.insert_temp(editor_id, value.to_string()));
            } else if response.is_pointer_button_down_on() || response.dragged() {
                if ui.input(|i| i.pointer.any_pressed()) {
                    ui.ctx().data_mut(|data| data.insert_temp(origin_id, before));
                }
                if let Some(pointer) = response.interact_pointer_pos() {
                    let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pointer);
                    if dial.contains(origin) {
                        let delta = pointer - center;
                        if delta.length() >= 3. {
                            let direction = if clockwise { 1. } else { -1. };
                            let mut degrees = (f64::from(delta.y) * direction).atan2(f64::from(delta.x)).to_degrees();
                            if ui.input(|i| i.modifiers.shift) {
                                degrees = (degrees / 15.).round() * 15.;
                            }
                            *value = wrapped_angle(degrees);
                        }
                    } else if response.dragged() {
                        let origin = ui.ctx().data(|data| data.get_temp::<f64>(origin_id)).unwrap_or(before);
                        *value = wrapped_angle(origin + f64::from(response.drag_delta().x));
                    }
                }
            } else if response.has_focus() {
                let delta = ui.input(|i| {
                    let increment = if i.modifiers.shift { 15. } else { 1. };
                    let forward = i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowUp);
                    let backward = i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowDown);
                    (i32::from(forward) - i32::from(backward)) as f64 * increment
                });
                if delta != 0. {
                    *value = wrapped_angle(before + delta);
                }
                if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    ui.ctx().data_mut(|data| data.insert_temp(editor_id, value.to_string()));
                }
            }
        }
        let color = if ui.is_enabled() { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
        let accent = ui.visuals().selection.stroke.color;
        let radius = 14.;
        ui.painter().circle_stroke(center, radius, Stroke::new(if response.has_focus() { 1.8 } else { 1. }, if response.has_focus() { accent } else { ui.visuals().weak_text_color() }));
        for direction in [Vec2::X, -Vec2::X, Vec2::Y, -Vec2::Y] {
            ui.painter().line_segment([center + direction * 11., center + direction * radius], Stroke::new(1., color));
        }
        let radians = value.to_radians();
        let direction = Vec2::new(radians.cos() as f32, radians.sin() as f32 * if clockwise { 1. } else { -1. });
        ui.painter().line_segment([center, center + direction * 11.], Stroke::new(1.8, accent));
        ui.painter().circle_filled(center + direction * 11., 2., accent);
        if let Some(mut text) = ui.ctx().data(|data| data.get_temp::<String>(editor_id)) {
            let edit = ui.put(number, egui::TextEdit::singleline(&mut text).id(editor_id).frame(false).desired_width(number.width()));
            if response.double_clicked() || (editing.is_none() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                edit.request_focus();
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), editor_id) {
                    state.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()))));
                    state.store(ui.ctx(), editor_id);
                }
            }
            let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape)) || !interactive;
            let commit = edit.lost_focus() || (editing.is_some() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if cancel || commit {
                if !cancel {
                    if let Ok(parsed) = text.trim().trim_end_matches('°').trim().parse::<f64>() {
                        if parsed.is_finite() { *value = wrapped_angle(parsed); }
                    }
                }
                ui.ctx().data_mut(|data| data.remove::<String>(editor_id));
                edit.surrender_focus();
            } else {
                ui.ctx().data_mut(|data| data.insert_temp(editor_id, text));
            }
        } else {
            let degrees = format!("{value:.2}").trim_end_matches('0').trim_end_matches('.').to_string();
            ui.painter().text(number.left_center(), egui::Align2::LEFT_CENTER, format!("{degrees}°"), egui::FontId::proportional(12.5), color);
        }
        if *value != before { response.mark_changed(); }
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, ui.is_enabled(), format!("{name}: {value} degrees")));
        response.on_hover_text(format!("{help}\nDrag the radius · Shift snaps to 15° · Drag degrees to adjust · Double-click degrees to type · Focus and press Enter to type · Arrow keys adjust 1° (Shift: 15°) · Full turns wrap"))
    }).inner
}

#[cfg(test)]
mod range_tests {
    use super::*;

    fn range_frame(
        ctx: &egui::Context,
        value: &mut f32,
        limits: RangeInclusive<f32>,
        logarithmic: bool,
        events: Vec<egui::Event>,
    ) -> egui::Response {
        let mut response = None;
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(400., 180.))),
                focused: true,
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    response = Some(range(
                        ui,
                        "integer input",
                        value,
                        limits.clone(),
                        95.,
                        "",
                        0,
                        logarithmic,
                    ));
                });
            },
        );
        response.unwrap()
    }
    fn range_pointer(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    fn range_key(key: egui::Key, pressed: bool, repeat: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: Some(key),
            pressed,
            repeat,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn integer_ranges_focus_by_pointer_then_accumulate_keys_and_clamp() {
        for (limits, initial, logarithmic) in [
            (1.0..=9., 3., false),
            (0.0..=4096., 37., false),
            (-180.0..=180., -30., false),
            (1.0..=1024., 64., true),
        ] {
            let ctx = egui::Context::default();
            let mut value = initial;
            let response = range_frame(&ctx, &mut value, limits.clone(), logarithmic, vec![]);
            let pos = response.rect.center();
            for pressed in [true, false] {
                range_frame(
                    &ctx,
                    &mut value,
                    limits.clone(),
                    logarithmic,
                    vec![egui::Event::PointerMoved(pos), range_pointer(pos, pressed)],
                );
                assert_eq!(value, initial, "Focusing an integer range must not edit it");
            }
            assert!(
                response.has_focus(),
                "The actual pointer click must focus the range"
            );
            for repeat in [false, true] {
                let response = range_frame(
                    &ctx,
                    &mut value,
                    limits.clone(),
                    logarithmic,
                    vec![range_key(egui::Key::ArrowRight, true, repeat)],
                );
                assert!(response.has_focus() && response.changed());
                assert_eq!(value, initial + if repeat { 2. } else { 1. });
            }
            range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                logarithmic,
                vec![
                    range_key(egui::Key::ArrowRight, false, false),
                    range_key(egui::Key::ArrowLeft, true, false),
                ],
            );
            assert_eq!(value, initial + 1.);
            range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                logarithmic,
                vec![range_key(egui::Key::ArrowLeft, false, false)],
            );
            value = *limits.end();
            let response = range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                logarithmic,
                vec![range_key(egui::Key::ArrowRight, true, false)],
            );
            assert_eq!(value, *limits.end());
            assert!(!response.changed());
            value = *limits.start();
            range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                logarithmic,
                vec![
                    range_key(egui::Key::ArrowRight, false, false),
                    range_key(egui::Key::ArrowLeft, true, false),
                ],
            );
            assert_eq!(value, *limits.start());
            range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                logarithmic,
                vec![range_key(egui::Key::ArrowLeft, false, false)],
            );
            ctx.memory_mut(|memory| memory.surrender_focus(response.id));
            range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                logarithmic,
                vec![range_key(egui::Key::ArrowRight, true, false)],
            );
            assert_eq!(
                value,
                *limits.start(),
                "Unfocused ranges must not consume keyboard edits"
            );
        }
    }
    #[test]
    fn integer_pointer_mapping_and_fractional_bounds_and_idle_values_are_preserved() {
        for (limits, initial, fraction, expected) in [
            (1.0..=9., 3., 0.75, 7.),
            (-8.0..=0., -6., 0.5, -4.),
            (0.5..=2.5, 2., 0.25, 1.),
        ] {
            let ctx = egui::Context::default();
            let mut value = initial;
            let response = range_frame(&ctx, &mut value, limits.clone(), false, vec![]);
            let start = egui::pos2(
                response.rect.left() + response.rect.width() * 0.1,
                response.rect.center().y,
            );
            let target = egui::pos2(
                response.rect.left() + response.rect.width() * fraction,
                start.y,
            );
            range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                false,
                vec![egui::Event::PointerMoved(start), range_pointer(start, true)],
            );
            assert_eq!(value, initial);
            let response = range_frame(
                &ctx,
                &mut value,
                limits.clone(),
                false,
                vec![egui::Event::PointerMoved(target)],
            );
            assert!(response.dragged() && response.changed());
            assert_eq!(value, expected);
            range_frame(
                &ctx,
                &mut value,
                limits,
                false,
                vec![range_pointer(target, false)],
            );
            assert_eq!(value, expected);
        }
        for logarithmic in [false, true] {
            let ctx = egui::Context::default();
            let mut value = 3.125_f32;
            let before = value.to_bits();
            for _ in 0..3 {
                let response = range_frame(&ctx, &mut value, 1.0..=9., logarithmic, vec![]);
                assert_eq!(
                    value.to_bits(),
                    before,
                    "A presentation-only fractional value must stay exact"
                );
                assert!(!response.changed());
            }
        }
    }

    #[test]
    fn range_fill_tracks_the_current_value_in_the_input_frame() {
        for logarithmic in [false, true] {
            let ctx = egui::Context::default();
            let color = Color32::from_rgb(197, 63, 19);
            let mut time = 0.;
            let mut draw = |value: &mut f32, events| {
                time += 1. / 240.;
                let mut response = None;
                let output = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::splat(400.))),
                        time: Some(time),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            response = Some(range_with_color(
                                ui,
                                "range",
                                value,
                                1.0..=100.,
                                180.,
                                "%",
                                2,
                                logarithmic,
                                color,
                            ));
                        });
                    },
                );
                let response = response.unwrap();
                let fill = output
                    .shapes
                    .iter()
                    .find_map(|shape| match &shape.shape {
                        egui::Shape::Rect(rect) if rect.fill == color => Some(rect.rect),
                        _ => None,
                    })
                    .unwrap();
                let fraction = if logarithmic {
                    value.ln() / 100_f32.ln()
                } else {
                    (*value - 1.) / 99.
                };
                assert!((fill.width() - response.rect.width() * fraction).abs() < 0.001);
                response
            };
            let mut value = 20.;
            let response = draw(&mut value, vec![]);
            // AI presentation supplies its own interpolated value; do not ease it again.
            value = 65.125;
            draw(&mut value, vec![]);
            assert_eq!(value, 65.125);
            let start = response.rect.center();
            draw(
                &mut value,
                vec![
                    egui::Event::PointerMoved(start),
                    egui::Event::PointerButton {
                        pos: start,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            let end = start + Vec2::new(50., 0.);
            let response = draw(&mut value, vec![egui::Event::PointerMoved(end)]);
            assert!(response.changed());
        }
    }
}

#[cfg(test)]
mod angle_tests {
    use super::*;
    fn key(key: egui::Key, pressed: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn angle_dial_preserves_precision_and_accepts_pointer_keyboard_and_wrapped_entry() {
        for clockwise in [false, true] {
            let ctx = egui::Context::default();
            let mut time = 0.;
            let mut value = 17.250000000000007;
            let mut draw = |value: &mut f64, events, modifiers| {
                time += 0.1;
                let mut response = None;
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(180., 700.),
                        )),
                        time: Some(time),
                        events,
                        modifiers,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default().show(ctx, |ui| {
                            response = Some(angle(
                                ui,
                                "direction",
                                value,
                                clockwise,
                                "Direction",
                                "Direction",
                            ));
                        });
                    },
                );
                response.unwrap()
            };
            let response = draw(&mut value, vec![], Default::default());
            assert_eq!(value, 17.250000000000007);
            assert!(response.rect.width() <= 164.);
            let center = response.rect.min + Vec2::splat(18.);
            let right = center + Vec2::new(11., 0.);
            let up = center - Vec2::new(0., 11.);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            draw(
                &mut value,
                vec![egui::Event::PointerMoved(right), button(right, true)],
                Default::default(),
            );
            assert_eq!(value, 0.);
            draw(
                &mut value,
                vec![egui::Event::PointerMoved(up)],
                Default::default(),
            );
            assert_eq!(value, if clockwise { -90. } else { 90. });
            draw(&mut value, vec![button(up, false)], Default::default());
            draw(
                &mut value,
                vec![key(egui::Key::ArrowRight, true)],
                Default::default(),
            );
            assert_eq!(value, if clockwise { -89. } else { 91. });
            draw(
                &mut value,
                vec![
                    key(egui::Key::ArrowRight, false),
                    key(egui::Key::Enter, true),
                ],
                Default::default(),
            );
            assert!(number_editing(&ctx, response.id));
            let before = value;
            draw(
                &mut value,
                vec![
                    key(egui::Key::Enter, false),
                    egui::Event::Text("270.1234567890123".into()),
                ],
                Default::default(),
            );
            assert_eq!(value, before, "Text is provisional until committed");
            draw(
                &mut value,
                vec![key(egui::Key::Enter, true)],
                Default::default(),
            );
            assert_eq!(value, wrapped_angle(270.1234567890123));
            assert!(!number_editing(&ctx, response.id));
            // Finite exact input and complete turns retain the engine's bounded convention.
            assert_eq!(wrapped_angle(180.), 180.);
            assert_eq!(wrapped_angle(-180.), -180.);
            assert_eq!(wrapped_angle(540.), -180.);
            assert_eq!(wrapped_angle(-450.), -90.);
            // Explicit snapping is presentation input, never serialized source state.
            // Register the settled non-text widget before the next pointer gesture.
            draw(
                &mut value,
                vec![key(egui::Key::Enter, false)],
                Default::default(),
            );
            let diagonal = center + Vec2::new(16., -5.);
            let retry = draw(
                &mut value,
                vec![
                    key(egui::Key::Enter, false),
                    egui::Event::PointerMoved(right),
                    button(right, true),
                ],
                Default::default(),
            );
            assert_eq!(retry.id, response.id, "Stable control identity");
            assert_eq!(
                value,
                0.,
                "Fresh dial press: down={} dragged={} rect={:?}",
                retry.is_pointer_button_down_on(),
                retry.dragged(),
                retry.rect
            );
            draw(
                &mut value,
                vec![egui::Event::PointerMoved(diagonal)],
                egui::Modifiers {
                    shift: true,
                    ..Default::default()
                },
            );
            assert_eq!(value, if clockwise { -15. } else { 15. });
            draw(
                &mut value,
                vec![button(diagonal, false)],
                Default::default(),
            );
        }
    }
}

pub fn number_editing(ctx: &egui::Context, control: egui::Id) -> bool {
    ctx.data(|data| data.get_temp::<String>(control.with("number")).is_some())
}
pub fn cancel_number_edit(ctx: &egui::Context, control: egui::Id) {
    let editor = control.with("number");
    ctx.data_mut(|data| data.remove::<String>(editor));
    ctx.memory_mut(|memory| memory.surrender_focus(editor));
}

pub fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(12.0)
            .color(Color32::from_rgb(188, 183, 189)),
    );
}
