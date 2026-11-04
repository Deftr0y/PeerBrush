use super::*;

pub(super) fn body(ui: &mut egui::Ui, width: f32) {
    ui.set_max_width(width.min((ui.ctx().screen_rect().width() - 40.).max(200.)));
    ui.spacing_mut().item_spacing = Vec2::new(8., 8.);
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
}

pub(super) fn heading(ui: &mut egui::Ui, title: &str, warning: bool) {
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(24.), egui::Sense::hover());
        let color = if warning { ACCENT } else { MUTED };
        if warning {
            ui.painter().add(egui::Shape::closed_line(
                vec![
                    rect.center_top() + Vec2::new(0., 2.),
                    rect.right_bottom() - Vec2::splat(2.),
                    rect.left_bottom() + Vec2::new(2., -2.),
                ],
                Stroke::new(1.5, color),
            ));
        } else {
            ui.painter()
                .circle_stroke(rect.center(), 10., Stroke::new(1.5, color));
        }
        ui.painter().text(
            rect.center() + Vec2::new(0., 2.),
            egui::Align2::CENTER_CENTER,
            if warning { "!" } else { "i" },
            egui::FontId::proportional(14.),
            color,
        );
        ui.label(RichText::new(title).size(18.).strong());
    });
    ui.add_space(3.);
}

pub(super) fn primary(label: &str) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).color(Color32::WHITE))
        .fill(Color32::from_rgb(190, 70, 29))
        .min_size(Vec2::new(0., 28.))
}
