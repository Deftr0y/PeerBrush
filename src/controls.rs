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
                ui.add(
                    egui::Slider::new(value, limits.clone())
                        // Preserve display-only fractional animation; clamp/round on an actual edit.
                        .clamping(egui::SliderClamping::Edits)
                        .show_value(false)
                        .logarithmic(logarithmic)
                        .max_decimals(decimals),
                )
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
        let fraction = ui
            .ctx()
            .animate_value_with_time(response.id.with("fill"), fraction, 0.06);
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
