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
            .inner;
        let rect = Rect::from_center_size(
            response.rect.center(),
            Vec2::new(response.rect.width(), 22.0),
        );
        let editor_id = response.id.with("number");
        if response.double_clicked() && ui.is_enabled() {
            *value = before;
            ui.ctx()
                .data_mut(|d| d.insert_temp(editor_id, format!("{value:.decimals$}")));
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
        let fraction = if logarithmic && low > 0.0 {
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
                    .frame(false)
                    .font(egui::TextStyle::Body)
                    .horizontal_align(egui::Align::RIGHT),
            );
            if response.double_clicked() {
                edit.request_focus();
            }
            let cancel = ui.input(|i| i.key_pressed(egui::Key::Escape));
            let commit = edit.lost_focus() || ui.input(|i| i.key_pressed(egui::Key::Enter));
            if cancel || commit {
                if !cancel {
                    if let Ok(parsed) = text.parse::<f32>() {
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

pub fn numeric(ui: &mut egui::Ui, value: &mut u32, limits: RangeInclusive<u32>) -> egui::Response {
    ui.scope(|ui| {
        for widget in [&mut ui.style_mut().visuals.widgets.inactive] {
            widget.bg_fill = Color32::TRANSPARENT;
            widget.bg_stroke = Stroke::NONE;
        }
        ui.add(egui::DragValue::new(value).range(limits))
    })
    .inner
}

pub fn label(ui: &mut egui::Ui, text: &str) {
    ui.label(
        RichText::new(text)
            .size(12.0)
            .color(Color32::from_rgb(188, 183, 189)),
    );
}
