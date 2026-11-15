//! Compact range controls: values sit in the filled bar, without a number box.
use eframe::egui::{self, Color32, Rect, RichText, Stroke, Vec2};
use std::{hash::Hash, ops::RangeInclusive};

pub(crate) fn point_release_pending(ctx: &egui::Context, id: egui::Id) -> bool {
    ctx.drag_stopped_id() == Some(id)
        && ctx.input(|input| input.pointer.primary_released())
        && ctx.data(|data| {
            data.get_temp::<Vec2>(id.with("point grab offset")).is_some()
                || data.get_temp::<(egui::Pos2, f64)>(id.with("angle origin")).is_some()
        })
}

pub fn point_handle(
    ui: &egui::Ui,
    id: egui::Id,
    center: egui::Pos2,
    label: &str,
) -> (egui::Response, Option<egui::Pos2>) {
    let response = ui.interact(Rect::from_center_size(center, Vec2::splat(18.)), id, egui::Sense::drag())
        .on_hover_cursor(egui::CursorIcon::Grab)
        .on_hover_text(label);
    response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, ui.is_enabled(), label));
    let grab = id.with("point grab offset");
    let primary = egui::PointerButton::Primary;
    let started = response.drag_started_by(primary);
    let stopped = response.drag_stopped_by(primary);
    let focused = ui.is_enabled() && ui.input(|input| {
        input.focused && !input.key_pressed(egui::Key::Escape)
            && !input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
    });
    if started && !stopped {
        let from_global = ui.ctx().layer_transform_from_global(ui.layer_id()).unwrap_or_default();
        let offset = ui.input(|input| input.pointer.press_origin())
            .map_or(Vec2::ZERO, |press| center - from_global * press);
        ui.ctx().data_mut(|data| data.insert_temp(grab, offset));
    }
    let point = if focused && (started || response.dragged_by(primary) || stopped) {
        let offset = ui.ctx().data(|data| data.get_temp::<Vec2>(grab)).unwrap_or(Vec2::ZERO);
        let pointer = if stopped {
            let from_global = ui.ctx().layer_transform_from_global(ui.layer_id()).unwrap_or_default();
            ui.input(|input| input.events.iter().find_map(|event| match event {
                egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: false, .. } => {
                    Some(from_global * *pos)
                }
                _ => None,
            }))
        } else {
            response.interact_pointer_pos()
        };
        pointer.map(|pointer| pointer + offset)
    } else {
        None
    };
    if stopped || !focused {
        ui.ctx().data_mut(|data| data.remove::<Vec2>(grab));
    }
    (response, point)
}

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
    let color = range_color(ui);
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

fn range_color(ui: &egui::Ui) -> Color32 {
    let color = ui.visuals().selection.stroke.color;
    if color == Color32::from_rgb(233, 84, 32) {
        Color32::from_rgb(190, 70, 29)
    } else {
        color.gamma_multiply(0.85)
    }
}

pub(crate) fn centered_range(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f32,
    limit: f32,
    width: f32,
    suffix: &str,
) -> egui::Response {
    let color = range_color(ui);
    range_control(ui, id, value, -limit..=limit, width, suffix, 0, false, RangeTrack::Centered(color))
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
    range_control(ui, id, value, limits, width, suffix, decimals, logarithmic, RangeTrack::Fill(color))
}

#[derive(Clone, Copy)]
enum RangeTrack<'a> {
    Fill(Color32),
    Centered(Color32),
    Ramp(&'a [Color32]),
}

pub(crate) fn color_range(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f32,
    limits: RangeInclusive<f32>,
    width: f32,
    suffix: &str,
    label: &str,
    colors: &[Color32],
) -> egui::Response {
    debug_assert!(colors.len() >= 2);
    let response = range_control(ui, id, value, limits, width, suffix, 0, false, RangeTrack::Ramp(colors));
    response.widget_info(|| egui::WidgetInfo::slider(response.enabled(), f64::from(*value), label));
    response.on_hover_text(format!("{label} · Drag to change · Double-click to type · Arrow keys to adjust"))
}

fn range_control(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f32,
    limits: RangeInclusive<f32>,
    width: f32,
    suffix: &str,
    decimals: usize,
    logarithmic: bool,
    track: RangeTrack<'_>,
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
                let bounds = f64::from(*limits.start())..=f64::from(*limits.end());
                let mut slider = egui::Slider::from_get_set(bounds, number_input(value, ui.is_enabled()))
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
        if ui.is_enabled() && (response.clicked() || response.is_pointer_button_down_on()) {
            response.request_focus();
        }
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
        if matches!(track, RangeTrack::Fill(_) | RangeTrack::Centered(_)) {
            painter.rect_filled(rect, 11, Color32::from_rgb(54, 51, 58).gamma_multiply(muted));
        }
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
        match track {
            RangeTrack::Fill(color) if fill > 0. => {
                painter.rect_filled(
                    Rect::from_min_size(rect.min, Vec2::new(fill, rect.height())),
                    11,
                    color.gamma_multiply(muted),
                );
            }
            RangeTrack::Centered(color) => {
                let zero = rect.center().x;
                let position = rect.left() + fill;
                if position != zero {
                    painter.rect_filled(
                        Rect::from_min_max(
                            egui::pos2(zero.min(position), rect.top()),
                            egui::pos2(zero.max(position), rect.bottom()),
                        ),
                        11,
                        color.gamma_multiply(muted),
                    );
                }
                painter.vline(zero, (rect.bottom() - 5.)..=(rect.bottom() - 2.),
                    Stroke::new(1., ui.visuals().text_color().gamma_multiply(muted)));
            }
            RangeTrack::Ramp(colors) => {
                let ramp = Rect::from_min_max(
                    egui::pos2(rect.left(), rect.bottom() - 6.),
                    egui::pos2(rect.right(), rect.bottom() - 2.),
                );
                let mut mesh = egui::Mesh::default();
                for (index, color) in colors.iter().enumerate() {
                    let x = egui::lerp(ramp.x_range(), index as f32 / (colors.len() - 1) as f32);
                    let color = color.gamma_multiply(muted);
                    mesh.colored_vertex(egui::pos2(x, ramp.top()), color);
                    mesh.colored_vertex(egui::pos2(x, ramp.bottom()), color);
                    if index > 0 {
                        let i = index as u32 * 2;
                        mesh.add_triangle(i - 2, i, i + 1);
                        mesh.add_triangle(i - 2, i + 1, i - 1);
                    }
                }
                painter.add(egui::Shape::mesh(mesh));
                let x = egui::lerp(ramp.x_range(), fraction);
                painter.vline(x, ramp.y_range().expand(1.), Stroke::new(3., Color32::BLACK.gamma_multiply(muted)));
                painter.vline(x, ramp.y_range().expand(1.), Stroke::new(1.5, Color32::WHITE.gamma_multiply(muted)));
            }
            RangeTrack::Fill(_) => {}
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
            let (x, y) = match track {
                RangeTrack::Fill(_) => {
                    let x = (rect.left() + fill - galley.size().x - 8.0).clamp(
                        rect.left() + 7.0,
                        (rect.right() - galley.size().x - 7.0).max(rect.left() + 7.0),
                    );
                    (x, rect.center().y)
                }
                RangeTrack::Centered(_) => (rect.right() - galley.size().x - 7., rect.center().y),
                RangeTrack::Ramp(_) => (rect.center().x - galley.size().x * 0.5, rect.center().y - 3.),
            };
            painter.galley(
                egui::pos2(x, y - galley.size().y * 0.5),
                galley,
                Color32::WHITE,
            );
        }
        if matches!(track, RangeTrack::Ramp(_) | RangeTrack::Centered(_)) {
            crate::icons::paint_focus(ui, &response);
        }
        if *value != before {
            response.mark_changed();
        }
        match track {
            RangeTrack::Fill(_) | RangeTrack::Centered(_) => response.on_hover_text("Drag to change · Double-click to type · Arrow keys to adjust"),
            RangeTrack::Ramp(_) => response,
        }
    })
    .inner
}

// The stock widgets can receive value actions from a previously displayed tree.
fn number_input<N: egui::emath::Numeric>(value: &mut N, enabled: bool) -> impl FnMut(Option<f64>) -> f64 + '_ {
    move |next| {
        if let Some(next) = next.filter(|next| enabled && next.is_finite()) {
            let next = N::from_f64(next);
            if next.to_f64().is_finite() {
                *value = next;
            }
        }
        value.to_f64()
    }
}

pub(crate) fn drag_value<N: egui::emath::Numeric>(value: &mut N, enabled: bool) -> egui::DragValue<'_> {
    let widget = egui::DragValue::from_get_set(number_input(value, enabled));
    if N::INTEGRAL {
        widget.max_decimals(0).range(N::MIN..=N::MAX).speed(0.25)
    } else {
        widget
    }
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
        ui.add(drag_value(value, ui.is_enabled()).range(limits))
    })
    .inner
}

const ANCHOR_NAMES: [&str; 9] = [
    "Top left", "Top", "Top right", "Left", "Center", "Right", "Bottom left", "Bottom", "Bottom right",
];

pub(crate) fn anchor_grid(
    ui: &mut egui::Ui,
    id: impl Hash,
    selected: Option<usize>,
    prefix: &str,
) -> Option<usize> {
    let mut choice = None;
    egui::Grid::new(id).spacing(Vec2::new(5., 3.)).show(ui, |ui| {
        for (index, name) in ANCHOR_NAMES.into_iter().enumerate() {
            let active = selected == Some(index);
            let label = format!("{prefix}{name}");
            let response = ui.add_sized([24., 24.], egui::Button::new("").selected(active))
                .on_hover_text(&label);
            response.widget_info(|| egui::WidgetInfo::selected(
                egui::WidgetType::RadioButton, response.enabled(), active, &label,
            ));
            if response.clicked() {
                choice = Some(index);
            }
            let color = if active && response.enabled() { ui.visuals().selection.stroke.color } else {
                ui.style().interact(&response).fg_stroke.color
            };
            let center = response.rect.center();
            if index == 4 {
                ui.painter().circle_filled(center, 2.5, color);
            } else {
                let direction = Vec2::new((index % 3) as f32 - 1., (index / 3) as f32 - 1.).normalized();
                let tip = center + direction * 6.;
                let side = Vec2::new(-direction.y, direction.x) * 3.;
                let stroke = Stroke::new(1.5, color);
                for end in [center - direction * 5., tip - direction * 4. + side, tip - direction * 4. - side] {
                    ui.painter().line_segment([end, tip], stroke);
                }
            }
            crate::icons::paint_focus(ui, &response);
            if index % 3 == 2 {
                ui.end_row();
            }
        }
    });
    choice
}

/// Illustrate a pixel blend with a warm source overlapping a cool backdrop.
pub(crate) fn blend_choice(
    ui: &mut egui::Ui,
    mode: &str,
    label: &str,
    selected: bool,
    enabled: bool,
) -> egui::Response {
    let button = if mode == "pass_through" {
        egui::Button::new(label)
    } else {
        const WIDTH: usize = 26;
        const HEIGHT: usize = 18;
        let id = egui::Id::new(("blend mode sample", mode));
        let texture = ui
            .ctx()
            .data(|data| data.get_temp::<egui::TextureHandle>(id))
            .unwrap_or_else(|| {
                let mut image = egui::ColorImage::new([WIDTH, HEIGHT], Color32::TRANSPARENT);
                for (index, pixel) in image.pixels.iter_mut().enumerate() {
                    let (x, y) = (index % WIDTH, index / WIDTH);
                    let backdrop = if x < 18 && y < 14 {
                        [8000 + 1600 * x as u16, 15000 + 3100 * y as u16, 49000, u16::MAX]
                    } else {
                        [0; 4]
                    };
                    let result = if x >= 8 && y >= 4 {
                        let source = [
                            57000,
                            12000 + 1600 * x as u16,
                            7000 + 1400 * y as u16,
                            u16::MAX,
                        ];
                        crate::raster::blend16(backdrop, source, 1., mode)
                    } else {
                        backdrop
                    };
                    let [r, g, b, a] = result.map(crate::raster::project16);
                    *pixel = Color32::from_rgba_unmultiplied(r, g, b, a);
                }
                let texture = ui.ctx().load_texture(
                    format!("{mode} blend sample"),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                ui.ctx().data_mut(|data| data.insert_temp(id, texture.clone()));
                texture
            });
        egui::Button::image_and_text(
            (texture.id(), Vec2::new(WIDTH as f32, HEIGHT as f32)),
            label,
        )
    };
    let response = ui.add_enabled(enabled, button.selected(selected));
    response.widget_info(|| {
        egui::WidgetInfo::selected(egui::WidgetType::RadioButton, response.enabled(), selected, label)
    });
    crate::icons::paint_focus(ui, &response);
    response
}

pub(crate) fn layer_sampling(
    ui: &mut egui::Ui,
    merged: &mut bool,
    layer_label: &str,
    layer_help: &str,
    composite_help: &str,
) -> bool {
    use crate::icons::{self, Icon};
    ui.horizontal(|ui| {
        label(ui, "Sample");
        ui.spacing_mut().item_spacing.x = 2.;
        let mut changed = false;
        for (value, icon, label, help) in [
            (false, Icon::SingleLayer, layer_label, layer_help),
            (true, Icon::LayerStack, "Sample all layers", composite_help),
        ] {
            let selected = *merged == value;
            if icons::selectable(ui, icon, selected, label).on_hover_text(help).clicked() && !selected {
                *merged = value;
                changed = true;
            }
        }
        changed
    }).inner
}

pub(crate) fn sampling_choices(
    ui: &mut egui::Ui,
    current: crate::production_export::Resampling,
) -> Option<(crate::production_export::Resampling, egui::Response)> {
    use crate::production_export::{native_grid, Resampling};
    const EDGE: u32 = 24;
    let mut change = None;
    for (method, name, help) in [
        (Resampling::Nearest, "Nearest", "Use one source pixel and keep hard pixel edges."),
        (Resampling::Bilinear, "Bilinear", "Blend nearby source pixels for smoother edges."),
    ] {
        let id = egui::Id::new(("sampling sample", name));
        let texture = ui.ctx().data(|data| data.get_temp::<egui::TextureHandle>(id))
            .unwrap_or_else(|| {
                let coordinate = |v: u32| (f64::from(v) + 0.5) * 6. / f64::from(EDGE) - 0.5;
                let mut image = egui::ColorImage::new([EDGE as usize; 2], Color32::BLACK);
                for (index, display) in (0..EDGE * EDGE).zip(&mut image.pixels) {
                    let point = [coordinate(index % EDGE), coordinate(index / EDGE)];
                    let pixel = native_grid::sample(point, method, |x, y| {
                        let shade = if x > y { 57000 } else { 8500 };
                        Ok([shade, shade, shade, u16::MAX])
                    }).expect("Finite sampling illustration");
                    *display = Color32::from_gray(crate::raster::project16(pixel[0]));
                }
                let texture = ui.ctx().load_texture(
                    format!("{name} sample"),
                    image,
                    egui::TextureOptions::NEAREST,
                );
                ui.ctx().data_mut(|data| data.insert_temp(id, texture.clone()));
                texture
            });
        let mut response = ui.add(egui::Button::image_and_text((texture.id(), Vec2::splat(EDGE as f32)), name)
            .selected(current == method)).on_hover_text(help);
        response.widget_info(|| {
            egui::WidgetInfo::selected(egui::WidgetType::RadioButton, response.enabled(), current == method, name)
        });
        crate::icons::paint_focus(ui, &response);
        if current != method && response.clicked() {
            response.mark_changed();
            change = Some((method, response));
        }
    }
    change
}

pub(crate) const RGB_COLORS: [Color32; 3] = [
    Color32::from_rgb(234, 100, 103),
    Color32::from_rgb(113, 202, 124),
    Color32::from_rgb(106, 158, 245),
];
type SwatchChoice = (&'static str, &'static [Color32]);
pub(crate) const RGB_CHANNELS: [SwatchChoice; 3] = [
    ("Red", &[RGB_COLORS[0]]),
    ("Green", &[RGB_COLORS[1]]),
    ("Blue", &[RGB_COLORS[2]]),
];
pub(crate) const GRAYSCALE: &[Color32] = &[Color32::BLACK, Color32::GRAY, Color32::WHITE];

pub(crate) fn blend_channel(ui: &mut egui::Ui, name: &str, color: Color32, enabled: bool) -> egui::Response {
    let id = egui::Id::new(("blend channel swatch", color, enabled));
    let texture = ui.ctx().data(|data| data.get_temp::<egui::TextureHandle>(id))
        .unwrap_or_else(|| {
            let mut image = egui::ColorImage::new([12, 12], color);
            if !enabled {
                for (index, pixel) in image.pixels.iter_mut().enumerate() {
                    let (x, y) = (index % 12, index / 12);
                    if (x + y).abs_diff(11) <= 1 {
                        *pixel = Color32::from_gray(36);
                    }
                }
            }
            let texture = ui.ctx().load_texture("Blend channel swatch", image, egui::TextureOptions::LINEAR);
            ui.ctx().data_mut(|data| data.insert_temp(id, texture.clone()));
            texture
        });
    let response = ui.add(egui::Button::image_and_text((texture.id(), Vec2::splat(14.)), name)
        .selected(enabled)).on_hover_text(if enabled { "Exclude this channel" } else { "Include this channel" });
    response.widget_info(|| egui::WidgetInfo::selected(
        egui::WidgetType::Checkbox, response.enabled(), enabled, format!("Blend {name} channel"),
    ));
    crate::icons::paint_focus(ui, &response);
    response
}

pub(crate) fn swatch_choices(
    ui: &mut egui::Ui,
    title: &str,
    selected: &mut usize,
    choices: impl IntoIterator<Item = SwatchChoice>,
) -> Option<egui::Response> {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 3.;
        label(ui, title);
        let mut current = "";
        let mut changed = None;
        for (index, (name, colors)) in choices.into_iter().enumerate() {
            let active = *selected == index;
            let mut response = ui
                .add_sized([24., 24.], egui::Button::new("").selected(active))
                .on_hover_text(name);
            response.widget_info(|| {
                egui::WidgetInfo::selected(
                    egui::WidgetType::RadioButton,
                    response.enabled(),
                    active,
                    format!("{title} · {name}"),
                )
            });
            let swatch = response.rect.shrink(5.);
            let width = swatch.width() / colors.len() as f32;
            for (column, &color) in colors.iter().enumerate() {
                let start = swatch.min + Vec2::new(column as f32 * width, 0.);
                ui.painter().rect_filled(
                    Rect::from_min_size(start, Vec2::new(width, swatch.height())),
                    0,
                    color,
                );
            }
            ui.painter().rect_stroke(
                swatch,
                0,
                Stroke::new(1., ui.visuals().widgets.inactive.fg_stroke.color),
                egui::StrokeKind::Outside,
            );
            crate::icons::paint_focus(ui, &response);
            if response.clicked() && !active {
                *selected = index;
                response.mark_changed();
                changed = Some(response);
            }
            if *selected == index {
                current = name;
            }
        }
        ui.label(current);
        changed
    }).inner
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
    angle_control(ui, id, value, clockwise, name, help, AngleRange::Signed)
}

pub(crate) fn whole_turn_angle(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f64,
    name: &str,
    help: &str,
) -> egui::Response {
    angle_control(ui, id, value, false, name, help, AngleRange::WholeTurn)
}

enum AngleRange {
    Signed,
    WholeTurn,
    Bounded(RangeInclusive<f64>),
}

pub(crate) fn angle_with_limits(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f64,
    clockwise: bool,
    name: &str,
    help: &str,
    limits: Option<RangeInclusive<f64>>,
) -> egui::Response {
    let range = limits.map_or(AngleRange::Signed, AngleRange::Bounded);
    angle_control(ui, id, value, clockwise, name, help, range)
}

fn angle_control(
    ui: &mut egui::Ui,
    id: impl Hash,
    value: &mut f64,
    clockwise: bool,
    name: &str,
    help: &str,
    range: AngleRange,
) -> egui::Response {
    use egui::accesskit::{Action, ActionData};

    let limits = match &range {
        AngleRange::Signed => -180. ..=180.,
        AngleRange::WholeTurn => 0. ..=359.,
        AngleRange::Bounded(limits) => limits.clone(),
    };
    let partial_arc = matches!(range, AngleRange::Bounded(_))
        && limits.end() - limits.start() <= 180.;
    let constrain = |degrees: f64| match &range {
        AngleRange::Bounded(limits) => degrees.clamp(*limits.start(), *limits.end()),
        AngleRange::Signed => wrapped_angle(degrees),
        AngleRange::WholeTurn => degrees.round().rem_euclid(360.),
    };
    ui.push_id(id, |ui| {
        let before = *value;
        let width = ui.available_width().clamp(96., 160.);
        let (rect, mut response) = ui.allocate_exact_size(Vec2::new(width, 36.), egui::Sense::click_and_drag());
        let dial = Rect::from_min_size(rect.min, Vec2::splat(36.));
        let center = dial.center();
        let number = Rect::from_min_max(rect.min + Vec2::new(43., 6.), rect.max - Vec2::new(0., 6.));
        let editor_id = response.id.with("number");
        let origin_id = response.id.with("angle origin");
        let primary = egui::PointerButton::Primary;
        let released = response.drag_stopped_by(primary);
        let from_global = ui.ctx().layer_transform_from_global(ui.layer_id()).unwrap_or_default();
        let editing = ui.ctx().data(|data| data.get_temp::<String>(editor_id));
        let interactive = ui.is_enabled() && ui.input(|i| i.focused && !i.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false))) && !i.key_pressed(egui::Key::Escape));
        if interactive && editing.is_none() {
            if response.clicked() || response.is_pointer_button_down_on() {
                response.request_focus();
            }
            if response.double_clicked() {
                ui.ctx().data_mut(|data| data.insert_temp(editor_id, value.to_string()));
            } else if response.is_pointer_button_down_on() || response.dragged_by(primary) || released {
                if response.is_pointer_button_down_on() && ui.input(|i| i.pointer.button_pressed(primary)) {
                    if let Some(press) = ui.input(|i| i.pointer.press_origin()) {
                        ui.ctx().data_mut(|data| data.insert_temp(origin_id, (from_global * press, before)));
                    }
                }
                let pointer = if released {
                    ui.input(|i| i.events.iter().find_map(|event| match event {
                        egui::Event::PointerButton { pos, button, pressed: false, modifiers }
                            if *button == primary => Some((from_global * *pos, modifiers.shift)),
                        _ => None,
                    }))
                } else {
                    response.interact_pointer_pos().map(|pointer| (pointer, ui.input(|i| i.modifiers.shift)))
                };
                let origin = ui.ctx().data(|data| data.get_temp::<(egui::Pos2, f64)>(origin_id));
                if let (Some((pointer, snap)), Some((press, origin))) = (pointer, origin) {
                    if dial.contains(press) {
                        let delta = pointer - center;
                        if delta.length() >= 3. {
                            let direction = if clockwise { 1. } else { -1. };
                            let mut degrees = (f64::from(delta.y) * direction).atan2(f64::from(delta.x)).to_degrees();
                            if snap {
                                degrees = (degrees / 15.).round() * 15.;
                            }
                            if matches!(range, AngleRange::Bounded(_)) {
                                let reference = if partial_arc {
                                    (limits.start() + limits.end()) * 0.5
                                } else {
                                    before
                                };
                                degrees = reference + wrapped_angle(degrees - reference);
                            }
                            *value = constrain(degrees);
                        }
                    } else if response.dragged_by(primary) || released {
                        *value = constrain(origin + f64::from(pointer.x - press.x));
                    }
                }
            } else {
                ui.input(|i| {
                    let forward = i.num_accesskit_action_requests(response.id, Action::Increment);
                    let backward = i.num_accesskit_action_requests(response.id, Action::Decrement);
                    if forward != backward {
                        *value = constrain(before + forward as f64 - backward as f64);
                    }
                    for request in i.accesskit_action_requests(response.id, Action::SetValue) {
                        match request.data {
                            Some(ActionData::NumericValue(next)) if next.is_finite() => {
                                *value = constrain(next);
                            }
                            _ => {}
                        }
                    }
                });
                if response.has_focus() {
                    ui.memory_mut(|memory| {
                        memory.set_focus_lock_filter(response.id, egui::EventFilter {
                            horizontal_arrows: true,
                            vertical_arrows: true,
                            ..Default::default()
                        });
                    });
                    let delta = ui.input(|i| {
                        let increment = if i.modifiers.shift { 15. } else { 1. };
                        let forward = i.key_pressed(egui::Key::ArrowRight) || i.key_pressed(egui::Key::ArrowUp);
                        let backward = i.key_pressed(egui::Key::ArrowLeft) || i.key_pressed(egui::Key::ArrowDown);
                        (i32::from(forward) - i32::from(backward)) as f64 * increment
                    });
                    if delta != 0. {
                        *value = constrain(*value + delta);
                    }
                    if ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        ui.ctx().data_mut(|data| data.insert_temp(editor_id, value.to_string()));
                    }
                }
            }
        }
        if released || !interactive || !ui.input(|i| i.pointer.primary_down()) {
            ui.ctx().data_mut(|data| data.remove::<(egui::Pos2, f64)>(origin_id));
        }
        let color = if ui.is_enabled() { ui.visuals().text_color() } else { ui.visuals().weak_text_color() };
        let accent = ui.visuals().selection.stroke.color;
        let radius = 14.;
        let dial_stroke = if response.has_focus() {
            Stroke::new(1.8, accent)
        } else {
            Stroke::new(1., ui.visuals().weak_text_color())
        };
        let direction = |degrees: f64| {
            let (sin, cos) = degrees.to_radians().sin_cos();
            Vec2::new(cos as f32, sin as f32 * if clockwise { 1. } else { -1. })
        };
        if partial_arc {
            let low = *limits.start();
            let span = *limits.end() - low;
            let arc = (0..=16)
                .map(|step| center + direction(low + span * f64::from(step) / 16.) * radius)
                .collect();
            ui.painter().add(egui::Shape::line(arc, dial_stroke));
        } else {
            ui.painter().circle_stroke(center, radius, dial_stroke);
        }
        let endpoints = [direction(*limits.start()), direction(*limits.end())];
        let ticks = if partial_arc {
            endpoints.as_slice()
        } else {
            &[Vec2::X, -Vec2::X, Vec2::Y, -Vec2::Y]
        };
        for &direction in ticks {
            ui.painter().line_segment([center + direction * 11., center + direction * radius], Stroke::new(1., color));
        }
        let direction = direction(*value);
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
                        if parsed.is_finite() { *value = constrain(parsed); }
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
        response.widget_info(|| egui::WidgetInfo::slider(response.enabled(), *value, name));
        ui.ctx().accesskit_node_builder(response.id, |node| {
            node.set_min_numeric_value(*limits.start());
            node.set_max_numeric_value(*limits.end());
            if response.enabled() {
                node.add_action(Action::SetValue);
                if !matches!(range, AngleRange::Bounded(_)) || *value < *limits.end() {
                    node.add_action(Action::Increment);
                }
                if !matches!(range, AngleRange::Bounded(_)) || *value > *limits.start() {
                    node.add_action(Action::Decrement);
                }
            }
        });
        let wrap_hint = if matches!(range, AngleRange::Bounded(_)) { "" } else { " · Full turns wrap" };
        response.on_hover_text(format!("{help}\nDrag the dial · Shift snaps to 15° · Drag degrees to adjust · Double-click degrees to type · Focus and press Enter to type · Arrow keys adjust 1° (Shift: 15°){wrap_hint}"))
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
        range_frame_enabled(ctx, value, limits, logarithmic, true, events)
    }
    fn range_frame_enabled(
        ctx: &egui::Context,
        value: &mut f32,
        limits: RangeInclusive<f32>,
        logarithmic: bool,
        enabled: bool,
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
                    if !enabled {
                        ui.disable();
                    }
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
    #[test]
    fn accessible_range_actions_respect_enabled_state_and_finite_values() {
        use egui::accesskit::{Action, ActionData, ActionRequest};

        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut value = 40.;
        let id = range_frame(&ctx, &mut value, 0. ..=100., false, vec![]).id;
        let set = |value| egui::Event::AccessKitActionRequest(ActionRequest {
            action: Action::SetValue,
            target: id.value().into(),
            data: Some(ActionData::NumericValue(value)),
        });
        let disabled = range_frame_enabled(&ctx, &mut value, 0. ..=100., false, false, vec![set(72.)]);
        assert!(!disabled.changed());
        assert_eq!(value, 40.);

        let invalid = range_frame(&ctx, &mut value, 0. ..=100., false, vec![set(f64::NAN)]);
        assert!(!invalid.changed());
        assert_eq!(value, 40.);

        let accepted = range_frame(&ctx, &mut value, 0. ..=100., false, vec![set(72.)]);
        assert!(accepted.changed());
        assert_eq!(value, 72.);
    }

    fn range_pointer(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn point_handle_keeps_the_grab_offset_through_release() {
        let ctx = egui::Context::default();
        let center = egui::pos2(100., 60.);
        let frame = |events| {
            let mut point = None;
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(400., 180.))),
                focused: true,
                events,
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    (_, point) = point_handle(ui, egui::Id::new("point"), center, "Drag point");
                });
            });
            point
        };
        frame(vec![]);
        let press = center + Vec2::new(4., -2.);
        let held = egui::pos2(135., 87.);
        let release = egui::pos2(150., 93.);
        frame(vec![egui::Event::PointerMoved(press), range_pointer(press, true)]);
        assert_eq!(frame(vec![egui::Event::PointerMoved(held)]), Some(held + (center - press)));
        assert_eq!(
            frame(vec![
                egui::Event::PointerMoved(release),
                range_pointer(release, false),
                egui::Event::PointerMoved(release + Vec2::new(20., 3.)),
            ]),
            Some(release + (center - press))
        );
        assert_eq!(frame(vec![]), None);
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
    fn disabled_ranges_do_not_take_focus_or_change_values() {
        let ctx = egui::Context::default();
        let mut value = 3.;
        let response = range_frame_enabled(&ctx, &mut value, 1.0..=9., false, false, vec![]);
        let pos = response.rect.center();
        for events in [
            vec![egui::Event::PointerMoved(pos), range_pointer(pos, true)],
            vec![range_pointer(pos, false)],
            vec![range_key(egui::Key::ArrowRight, true, false)],
        ] {
            let response = range_frame_enabled(&ctx, &mut value, 1.0..=9., false, false, events);
            assert!(!response.enabled() && !response.has_focus() && !response.changed());
            assert_eq!(value, 3.);
        }
    }

    #[test]
    fn double_clicked_range_keeps_numeric_editor_focus_and_provisional_value() {
        let ctx = egui::Context::default();
        let mut value = 3.;
        let response = range_frame(&ctx, &mut value, 1.0..=9., false, vec![]);
        let pos = response.rect.center();
        for _ in 0..2 {
            for pressed in [true, false] {
                range_frame(
                    &ctx,
                    &mut value,
                    1.0..=9.,
                    false,
                    vec![egui::Event::PointerMoved(pos), range_pointer(pos, pressed)],
                );
                assert_eq!(
                    value, 3.,
                    "Focus and numeric entry must not change the value"
                );
            }
        }
        let editor = response.id.with("number");
        assert!(number_editing(&ctx, response.id));
        assert!(ctx.memory(|memory| memory.has_focus(editor)));
        range_frame(
            &ctx,
            &mut value,
            1.0..=9.,
            false,
            vec![egui::Event::Text("7".into())],
        );
        assert_eq!(
            ctx.data(|data| data.get_temp::<String>(editor)).as_deref(),
            Some("7")
        );
        assert_eq!(value, 3.);
        for pressed in [true, false] {
            range_frame(
                &ctx,
                &mut value,
                1.0..=9.,
                false,
                vec![egui::Event::PointerMoved(pos), range_pointer(pos, pressed)],
            );
            assert!(
                ctx.memory(|memory| memory.has_focus(editor)),
                "The range must not steal its text editor's focus"
            );
            assert_eq!(value, 3.);
        }
        range_frame(
            &ctx,
            &mut value,
            1.0..=9.,
            false,
            vec![range_key(egui::Key::ArrowRight, true, false)],
        );
        assert!(ctx.memory(|memory| memory.has_focus(editor)));
        assert_eq!(
            value, 3.,
            "Text cursor movement must not edit the underlying slider"
        );
        let accepted = range_frame(
            &ctx,
            &mut value,
            1.0..=9.,
            false,
            vec![range_key(egui::Key::Enter, true, false)],
        );
        assert!(accepted.changed());
        assert_eq!(value, 7.);
        assert!(!number_editing(&ctx, response.id));
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
