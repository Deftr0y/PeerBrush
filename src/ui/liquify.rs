use super::*;
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Mode {
    Push,
    Expand,
    Pinch,
    Restore,
}
impl Mode {
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
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.;
        for (mode, icon, label) in [
            (Mode::Push, Icon::LiquifyPush, "Push pixels with the stroke"),
            (Mode::Expand, Icon::LiquifyExpand, "Expand pixels away from the brush center"),
            (Mode::Pinch, Icon::LiquifyPinch, "Pinch pixels toward the brush center"),
            (Mode::Restore, Icon::Undo, "Restore the original shape in the brush area"),
        ] {
            if icons::selectable(ui, icon, settings.mode == mode, label).clicked() {
                settings.mode = mode;
            }
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
pub(super) fn command_fields(layer: &str, radius: f32, settings: &Settings) -> Value {
    json!({"op":"liquify.stroke","layer":layer,"mode":settings.mode.key(),"radius":radius,"strength":settings.strength})
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
