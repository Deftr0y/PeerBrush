use super::*;
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Push,
    Expand,
    Pinch,
    Restore,
}
impl Mode {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Push => "Push",
            Self::Expand => "Expand",
            Self::Pinch => "Pinch",
            Self::Restore => "Restore",
        }
    }
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::Push => "push",
            Self::Expand => "expand",
            Self::Pinch => "pinch",
            Self::Restore => "restore",
        }
    }
}
pub(super) struct Settings {
    pub strength: f32,
    pub mode: Mode,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            strength: 0.5,
            mode: Mode::Push,
        }
    }
}
pub(super) fn toolbar(
    ui: &mut egui::Ui,
    radius: &mut f32,
    settings: &mut Settings,
    display_radius: f32,
    display_strength: f32,
    ai: bool,
) {
    egui::ComboBox::from_id_salt("liquify mode")
        .selected_text(settings.mode.name())
        .width(76.)
        .show_ui(ui, |ui| {
            for mode in [Mode::Push, Mode::Expand, Mode::Pinch, Mode::Restore] {
                ui.selectable_value(&mut settings.mode, mode, mode.name());
            }
        });
    controls::label(ui, "Size");
    let mut diameter = *radius * 2.;
    if tool_settings::number(
        ui,
        "liquify radius",
        &mut diameter,
        display_radius * 2.,
        1.0..=1024.,
        112.,
        " px",
        0,
        true,
        ai,
    )
    .changed()
    {
        *radius = diameter * 0.5;
    }
    controls::label(ui, "Strength");
    let mut percent = settings.strength * 100.;
    if tool_settings::number(
        ui,
        "liquify strength",
        &mut percent,
        display_strength * 100.,
        0.0..=100.,
        112.,
        "%",
        0,
        false,
        ai,
    )
    .changed()
    {
        settings.strength = percent / 100.;
    }
}
pub(super) fn command(layer: &str, points: &[[f32; 2]], radius: f32, settings: &Settings) -> Value {
    json!({"op":"liquify.stroke","layer":layer,"mode":settings.mode.key(),"points":points,"radius":radius,"strength":settings.strength})
}
pub(super) fn cursor(painter: &egui::Painter, center: Pos2, radius: f32, scale: f32) {
    painter.circle_stroke(
        center,
        (radius * scale).max(2.0),
        Stroke::new(3.0_f32, Color32::BLACK),
    );
    painter.circle_stroke(
        center,
        (radius * scale).max(2.0),
        Stroke::new(1.0_f32, Color32::WHITE),
    );
    for direction in [Vec2::X, Vec2::Y] {
        let line = [center - direction * 3.0, center + direction * 3.0];
        painter.line_segment(line, Stroke::new(2.5_f32, Color32::BLACK));
        painter.line_segment(line, Stroke::new(1.0_f32, Color32::WHITE));
    }
}
