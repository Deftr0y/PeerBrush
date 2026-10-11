use super::*;
#[derive(Default)]
pub(super) struct Straight {
    last: Option<Last>,
    pub axis: Option<(usize, &'static str)>,
    pub line: bool,
    pub document: Option<String>,
    pub cancelled: bool,
    shift: bool,
    start: usize,
}
struct Last {
    document: String,
    revision: u64,
    layer: String,
    mask: bool,
    step: Option<String>,
    tool: Tool,
    point: [f32; 2],
    pressure: f32,
}
impl Straight {
    pub(super) fn cancel(&mut self) {
        self.axis = None;
        self.line = false;
        self.shift = false;
        self.start = 0;
        self.document = None;
        self.cancelled = false;
    }
    pub(super) fn reset(&mut self) {
        self.cancel();
        self.last = None;
    }
}
impl PeerBrush {
    pub(super) fn brush_tool(&self) -> bool {
        matches!(
            self.tool,
            Tool::Brush | Tool::Eraser | Tool::Smudge | Tool::Clone | Tool::Heal
        )
    }
    pub(super) fn straight_click(&mut self, doc: &Document, point: [f32; 2], shift: bool) {
        if !shift || !self.brush_tool() {
            return;
        }
        let Some(last) = self.straight.last.as_ref().filter(|v| {
            v.document == doc.id
                && v.revision == doc.revision
                && v.layer == self.selected
                && v.mask == self.mask
                && v.step == self.mask_step
                && v.tool == self.tool
        }) else {
            return;
        };
        let (start, pressure) = (last.point, last.pressure);
        if !self.begin_retouch(doc, start) {
            return;
        }
        self.straight.cancel();
        self.straight.line = true;
        self.straight.document = Some(doc.id.clone());
        self.drag_start = Some(point);
        self.drag_revision = Some(doc.revision);
        self.points = vec![start, point];
        self.stroke_pressures = vec![pressure, self.current_pressure.unwrap_or(1.)];
        self.stroke_has_pressure = self.current_pressure.is_some() || pressure != 1.;
        self.live_gesture = Some(crate::engine::id());
    }
    pub(super) fn straight_drag(&mut self, shift: bool) {
        self.straight.cancel();
        self.straight.shift = shift;
    }
    pub(super) fn straight_modifier(&mut self, shift: bool) {
        if !self.brush_tool() || self.straight.line {
            return;
        }
        if self.straight.shift && !shift {
            if self.straight.axis.is_some() && !self.points.is_empty() {
                if let Ok((points, _)) = crate::brush::points_from_command(
                    &self.stroke_command(self.points.clone(), self.color),
                ) {
                    self.points = points;
                }
            }
            self.straight.axis = None;
        }
        if shift && !self.straight.shift {
            self.straight.start = self.points.len().saturating_sub(1);
        }
        self.straight.shift = shift;
    }
    pub(super) fn straight_axis(&mut self) {
        if self.brush_tool()
            && self.straight.shift
            && !self.straight.line
            && self.straight.axis.is_none()
            && self.points.len() > self.straight.start + 1
        {
            let a = self.points[self.straight.start];
            let b = *self.points.last().unwrap();
            if (a[0] - b[0]).hypot(a[1] - b[1]) > 0. {
                self.straight.axis = Some((
                    self.straight.start,
                    if crate::brush::horizontal_axis(a, b) {
                        "horizontal"
                    } else {
                        "vertical"
                    },
                ));
            }
        }
    }
    pub(super) fn toolbar_brush_opacity(&mut self, ui: &mut egui::Ui) {
        controls::label(
            ui,
            if self.tool == Tool::Smudge {
                "Strength"
            } else {
                "Opacity"
            },
        );
        let ai = matches!(
            (self.animation.tool(ui.input(|i| i.time)), self.tool),
            (Some("brush"), Tool::Brush)
                | (Some("eraser"), Tool::Eraser)
                | (Some("smudge"), Tool::Smudge)
                | (Some("clone"), Tool::Clone)
                | (Some("heal"), Tool::Heal)
        );
        let ai = ai || self.tool_preferences_ai(ui.input(|i| i.time));
        let mut percent =
            self.tool_display_number("opacity", self.brush.opacity, ui.input(|i| i.time)) * 100.;
        let response = controls::range_with_color(
            ui,
            "toolbar brush opacity",
            &mut percent,
            0.0..=100.0,
            104.,
            "%",
            0,
            false,
            if ai {
                AI_BLUE
            } else {
                Color32::from_rgb(190, 70, 29)
            },
        );
        if response.changed() {
            self.brush.opacity = percent / 100.;
        }
        response.on_hover_text(if self.tool == Tool::Smudge {
            "Smudge strength"
        } else {
            "Brush opacity · double-click to enter a value"
        });
    }
    pub(super) fn pointer_pressure(ui: &egui::Ui) -> Option<f32> {
        ui.input(|input| {
            input.events.iter().rev().find_map(|event| {
                if let egui::Event::Touch {
                    force: Some(force),
                    phase,
                    ..
                } = event
                {
                    if matches!(phase, egui::TouchPhase::Start | egui::TouchPhase::Move)
                        && force.is_finite()
                        && (0.0..=1.0).contains(force)
                    {
                        return Some(*force);
                    }
                }
                None
            })
        })
    }
    pub(super) fn tip_outline(&self, painter: &egui::Painter, center: Pos2, scale: f32) {
        let (sin, cos) = self.brush.angle.to_radians().sin_cos();
        let points: Vec<Pos2> = (0..=48)
            .map(|i| {
                let a = i as f32 * std::f32::consts::TAU / 48.0;
                let x = a.cos() * self.radius * scale;
                let y = a.sin() * self.radius * scale * self.brush.roundness;
                center
                    + self
                        .view
                        .vector(Vec2::new(x * cos - y * sin, x * sin + y * cos))
            })
            .collect();
        painter.add(egui::Shape::line(
            points.clone(),
            Stroke::new(3.0_f32, Color32::BLACK),
        ));
        painter.add(egui::Shape::line(
            points,
            Stroke::new(1.2_f32, Color32::WHITE),
        ));
        // A small center mark remains legible when the real tip is subpixel at low zoom.
        for direction in [Vec2::X, Vec2::Y] {
            let line = [center - direction * 3.0, center + direction * 3.0];
            painter.line_segment(line, Stroke::new(2.5_f32, Color32::BLACK));
            painter.line_segment(line, Stroke::new(1.0_f32, Color32::WHITE));
        }
    }
    pub(super) fn swap_colors(&mut self) {
        if self.mask {
            std::mem::swap(&mut self.mask_value, &mut self.mask_background);
        } else {
            std::mem::swap(&mut self.color, &mut self.background);
        }
    }
    pub(super) fn palette(&mut self, ui: &mut egui::Ui) {
        let (rect, _) = ui.allocate_exact_size(Vec2::new(44.0, 60.0), egui::Sense::hover());
        let fore = if self.mask {
            [self.mask_value, self.mask_value, self.mask_value, 255]
        } else {
            self.color
        };
        let back = if self.mask {
            [
                self.mask_background,
                self.mask_background,
                self.mask_background,
                255,
            ]
        } else {
            self.background
        };
        for (foreground, pixel, at) in [
            (false, back, rect.min + Vec2::new(14.0, 15.0)),
            (true, fore, rect.min + Vec2::new(0.0, 2.0)),
        ] {
            let swatch = Rect::from_min_size(at, Vec2::splat(28.0));
            let response = ui.interact(
                swatch,
                ui.id().with(if foreground {
                    "foreground"
                } else {
                    "background"
                }),
                egui::Sense::click(),
            );
            super::color::checker(ui.painter(), swatch);
            ui.painter().rect_filled(
                swatch,
                2,
                Color32::from_rgba_unmultiplied(pixel[0], pixel[1], pixel[2], pixel[3]),
            );
            ui.painter().rect_stroke(
                swatch,
                2,
                Stroke::new(1.0_f32, Color32::from_gray(155)),
                egui::StrokeKind::Inside,
            );
            if response
                .on_hover_text(if foreground {
                    "Foreground color · X swaps colors"
                } else {
                    "Background color · X swaps colors"
                })
                .clicked()
            {
                let target = match (foreground, self.mask) {
                    (true, false) => super::color::Target::Foreground,
                    (false, false) => super::color::Target::Background,
                    (true, true) => super::color::Target::MaskForeground,
                    (false, true) => super::color::Target::MaskBackground,
                };
                self.open_color(target, pixel);
            }
        }
        let swap = ui
            .interact(
                Rect::from_min_size(rect.min + Vec2::new(29.0, -2.0), Vec2::splat(16.0)),
                ui.id().with("swap"),
                egui::Sense::click(),
            )
            .on_hover_text("Swap foreground / background · X");
        icons::paint(ui.painter(), swap.rect, Icon::Swap, true);
        if swap.clicked() {
            self.swap_colors();
        }
    }
    pub(super) fn paint_command(&mut self, points: Vec<[f32; 2]>, color: Pixel, label: &str) {
        let command = self.stroke_command(points, color);
        let last = crate::brush::points_from_command(&command)
            .ok()
            .and_then(|(p, pressure)| {
                p.last()
                    .map(|v| (*v, pressure.and_then(|p| p.last().copied()).unwrap_or(1.)))
            });
        let (document, revision) = {
            let e = self.shared.lock().unwrap();
            (e.doc.id.clone(), e.doc.revision)
        };
        self.edit(vec![command], label);
        let e = self.shared.lock().unwrap();
        self.straight.last = last
            .filter(|_| e.doc.id == document && e.doc.revision == revision + 1)
            .map(|(point, pressure)| Last {
                document,
                revision: e.doc.revision,
                layer: self.selected.clone(),
                mask: self.mask,
                step: self.mask_step.clone(),
                tool: self.tool,
                point,
                pressure,
            });
    }
    pub(super) fn stroke_command(&self, points: Vec<[f32; 2]>, color: Pixel) -> Value {
        let b = self.brush;
        let pressures = (self.stroke_has_pressure && self.stroke_pressures.len() == points.len())
            .then_some(&self.stroke_pressures);
        let sampled_points = points.clone();
        let mut command = json!({"op":if self.tool==Tool::Smudge {"smudge"}else{"paint"},"layer":self.selected,"points":points,"radius":self.radius,"color":color,
            "mask":self.mask,"step":self.mask_step,"erase":self.tool==Tool::Eraser,
            "hardness":b.hardness,"opacity":b.opacity,"flow":b.flow,"spacing":b.spacing,
            "roundness":b.roundness,"angle":b.angle,"smoothing":b.smoothing,
            "tip":b.tip.name(),"density":b.density,"grain":b.grain,"seed":b.seed,
            "pressure_size":b.pressure_size,"pressure_opacity":b.pressure_opacity,"pressure_gamma":b.pressure_gamma,"pressure_min":b.pressure_min,"pressure_max":b.pressure_max,
            "taper_start":b.taper_start,"taper_end":b.taper_end,"taper_size":b.taper_size,"taper_opacity":b.taper_opacity,
            "wetness":b.wetness,"load":b.load,"pickup":b.pickup});
        if let Some(pressures) = pressures {
            command["pressures"] = json!(pressures);
        }
        if let Some((start, axis)) = self.straight.axis {
            command["stroke_axis"] = json!(axis);
            command["stroke_axis_from"] = json!(start);
        }
        if let Some(revision) = self.drag_revision {
            command["source_revision"] = json!(revision);
            command["document_id"] = json!(self
                .straight
                .document
                .as_ref()
                .unwrap_or(&self.doc_snapshot.id));
        }
        self.retouch_command(&mut command, &sampled_points);
        command
    }
    pub(super) fn brush_settings(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new(match self.tool {
            Tool::Smudge => "Smudge",
            Tool::Clone => "Clone",
            Tool::Heal => "Heal",
            _ => "Brush",
        })
        .open(&mut open)
        .resizable(false)
        .collapsible(false)
        .default_pos(egui::pos2(85.0, 125.0))
        .default_width(680.0)
        .show(ctx, |ui| {
            ui.columns(2, |columns| {
                self.preset_browser(&mut columns[0]);
                self.brush_parameters(&mut columns[1]);
            });
        });
        self.show_brush = open;
    }
    fn brush_parameters(&mut self, ui: &mut egui::Ui) {
        let time = ui.input(|i| i.time);
        let ai = self.tool_preferences_ai(time);
        let display = self.tool_display_values(time);
        let shown = |key: &str| display[key].as_f64().unwrap_or(0.) as f32;
        ui.label(RichText::new(&self.brush_name).color(if ai { AI_BLUE } else { ACCENT }));
        ui.horizontal(|ui| {
            controls::label(ui, "Tip");
            egui::ComboBox::from_id_salt("procedural tip")
                .selected_text(self.brush.tip.name())
                .show_ui(ui, |ui| {
                    for tip in [
                        crate::brush::TipKind::Round,
                        crate::brush::TipKind::Dry,
                        crate::brush::TipKind::Chalk,
                        crate::brush::TipKind::Grain,
                        crate::brush::TipKind::Bristle,
                    ] {
                        ui.selectable_value(&mut self.brush.tip, tip, tip.name());
                    }
                });
        });
        let mut current = self.brush;
        current.radius = self.radius;
        let key = format!("{current:?}");
        if self.brush_preview.as_ref().map(|p| p.0.as_str()) != Some(key.as_str()) {
            if let Ok(r) = crate::brush_library::preview(current, 260, 54) {
                let texture = ui.ctx().load_texture(
                    "brush tip",
                    egui::ColorImage::from_rgba_unmultiplied([260, 54], &r.rgba()),
                    egui::TextureOptions::LINEAR,
                );
                self.brush_preview = Some((key, texture));
            }
        }
        if let Some((_, texture)) = &self.brush_preview {
            ui.image((texture.id(), Vec2::new(260., 54.)));
        }
        egui::Grid::new("brush parameters")
            .spacing(Vec2::new(10., 8.))
            .show(ui, |ui| {
                let mut size = self.radius * 2.;
                controls::label(ui, "Size");
                let response = tool_settings::number(
                    ui,
                    "tip size",
                    &mut size,
                    shown("radius") * 2.,
                    1.0..=1024.,
                    200.,
                    " px",
                    0,
                    true,
                    ai,
                );
                if response.changed() {
                    self.radius = size / 2.;
                }
                ui.end_row();
                for (id, label, value, min, max) in [
                    ("hardness", "Hardness", &mut self.brush.hardness, 0., 100.),
                    (
                        "opacity",
                        if self.tool == Tool::Smudge {
                            "Strength"
                        } else {
                            "Opacity"
                        },
                        &mut self.brush.opacity,
                        0.,
                        100.,
                    ),
                    ("flow", "Flow", &mut self.brush.flow, 0., 100.),
                    ("spacing", "Spacing", &mut self.brush.spacing, 1., 200.),
                    (
                        "roundness",
                        "Roundness",
                        &mut self.brush.roundness,
                        5.,
                        100.,
                    ),
                    (
                        "smoothing",
                        "Smoothing",
                        &mut self.brush.smoothing,
                        0.,
                        100.,
                    ),
                ] {
                    let mut percent = *value * 100.;
                    controls::label(ui, label);
                    if tool_settings::number(
                        ui,
                        id,
                        &mut percent,
                        shown(id) * 100.,
                        min..=max,
                        200.,
                        "%",
                        0,
                        false,
                        ai,
                    )
                    .changed()
                    {
                        *value = percent / 100.;
                    }
                    ui.end_row();
                }
                for (id, label, value, min, max, suffix, decimals) in [
                    ("angle", "Angle", &mut self.brush.angle, -180., 180., "°", 0),
                    (
                        "pressure_gamma",
                        "Pressure curve",
                        &mut self.brush.pressure_gamma,
                        0.1,
                        4.,
                        "",
                        2,
                    ),
                ] {
                    controls::label(ui, label);
                    tool_settings::number(
                        ui,
                        id,
                        value,
                        shown(id),
                        min..=max,
                        200.,
                        suffix,
                        decimals,
                        false,
                        ai,
                    );
                    ui.end_row();
                }
                if self.brush.tip != crate::brush::TipKind::Round {
                    let mut density = self.brush.density * 100.;
                    controls::label(ui, "Density");
                    if tool_settings::number(
                        ui,
                        "tip density",
                        &mut density,
                        shown("density") * 100.,
                        5.0..=100.,
                        200.,
                        "%",
                        0,
                        false,
                        ai,
                    )
                    .changed()
                    {
                        self.brush.density = density / 100.;
                    }
                    ui.end_row();
                    controls::label(ui, "Grain");
                    tool_settings::number(
                        ui,
                        "tip grain",
                        &mut self.brush.grain,
                        shown("grain"),
                        0.5..=32.,
                        200.,
                        " px",
                        1,
                        false,
                        ai,
                    );
                    ui.end_row();
                }
                for (id, label, value) in [
                    ("taper_start", "Start", &mut self.brush.taper_start),
                    ("taper_end", "End", &mut self.brush.taper_end),
                ] {
                    controls::label(ui, label);
                    tool_settings::number(
                        ui,
                        id,
                        value,
                        shown(id),
                        0.0..=512.,
                        200.,
                        " px",
                        0,
                        false,
                        ai,
                    )
                    .on_hover_text("Taper length along the stroke");
                    ui.end_row();
                }
                if self.tool == Tool::Smudge {
                    for (id, label, value) in [
                        ("wetness", "Wet", &mut self.brush.wetness),
                        ("load", "Load", &mut self.brush.load),
                        ("pickup", "Pickup", &mut self.brush.pickup),
                    ] {
                        let mut percent = *value * 100.;
                        controls::label(ui, label);
                        if tool_settings::number(
                            ui,
                            id,
                            &mut percent,
                            shown(id) * 100.,
                            0.0..=100.,
                            200.,
                            "%",
                            0,
                            false,
                            ai,
                        )
                        .changed()
                        {
                            *value = percent / 100.;
                        }
                        ui.end_row();
                    }
                }
            });
        ui.horizontal(|ui| {ui.label(RichText::new("Pressure").small().color(MUTED));ui.checkbox(&mut self.brush.pressure_size,"Size").on_hover_text("Uses genuine device force when supplied, or explicit agent pressure. Mouse strokes use taper.");ui.checkbox(&mut self.brush.pressure_opacity,"Opacity");});
        if ui.small_button("Calibrate pressure…").clicked() {
            self.show_calibration = true;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new("Taper").small().color(MUTED));
            ui.checkbox(&mut self.brush.taper_size, "Size");
            ui.checkbox(&mut self.brush.taper_opacity, "Opacity");
        });
    }
    fn preset_browser(&mut self, ui: &mut egui::Ui) {
        let library = self.shared.lock().unwrap().brush_library.clone();
        let (mut presets, error) = {
            let l = library.lock().unwrap();
            (l.presets(), l.error.clone())
        };
        presets.sort_by_key(|preset| {
            crate::brush_library::CATEGORIES
                .iter()
                .position(|category| *category == preset.category.as_str())
                .unwrap_or(crate::brush_library::CATEGORIES.len())
        });
        self.brush_thumbnails
            .retain(|id, _| presets.iter().any(|p| &p.id == id));
        ui.add(
            egui::TextEdit::singleline(&mut self.brush_search)
                .hint_text("Search brushes")
                .desired_width(ui.available_width()),
        );
        let mut categories = vec!["All".to_owned()];
        for category in crate::brush_library::CATEGORIES {
            categories.push((*category).into());
        }
        categories.push("Custom".to_owned());
        for preset in &presets {
            if !categories.contains(&preset.category) {
                categories.push(preset.category.clone());
            }
        }
        egui::ComboBox::from_id_salt("brush category")
            .selected_text(&self.brush_category)
            .show_ui(ui, |ui| {
                for category in categories {
                    ui.selectable_value(&mut self.brush_category, category.clone(), category);
                }
            });
        let search = self.brush_search.to_lowercase();
        let mut count = 0;
        egui::ScrollArea::vertical()
            .id_salt("brush library")
            .max_height(350.0)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut section = None;
                for preset in &presets {
                    if self.brush_category != "All"
                        && !(self.brush_category == "Custom" && preset.custom)
                        && self.brush_category != preset.category
                    {
                        continue;
                    }
                    if !format!("{} {}", preset.name, preset.category)
                        .to_lowercase()
                        .contains(&search)
                    {
                        continue;
                    }
                    count += 1;
                    if section != Some(preset.category.as_str()) {
                        if section.is_some() {
                            ui.add_space(6.);
                        }
                        ui.label(RichText::new(&preset.category).small().color(ACCENT));
                        section = Some(preset.category.as_str());
                    }
                    let cached = self
                        .brush_thumbnails
                        .get(&preset.id)
                        .filter(|(settings, _)| *settings == preset.settings)
                        .map(|(_, texture)| texture.clone());
                    let texture = cached.or_else(|| {
                        let raster =
                            crate::brush_library::preview(preset.settings, 240, 36).ok()?;
                        let texture = ui.ctx().load_texture(
                            format!("brush {}", preset.id),
                            egui::ColorImage::from_rgba_unmultiplied([240, 36], &raster.rgba()),
                            egui::TextureOptions::LINEAR,
                        );
                        self.brush_thumbnails
                            .insert(preset.id.clone(), (preset.settings, texture.clone()));
                        Some(texture)
                    });
                    let selected = self.brush_preset.as_deref() == Some(&preset.id);
                    let (rect, response) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), 52.0),
                        egui::Sense::click(),
                    );
                    if response.hovered() || selected {
                        ui.painter().rect_filled(
                            rect,
                            0,
                            Color32::from_white_alpha(if selected { 12 } else { 5 }),
                        );
                    }
                    ui.painter().text(
                        rect.left_top() + Vec2::new(5., 7.),
                        egui::Align2::LEFT_TOP,
                        &preset.name,
                        egui::FontId::proportional(12.),
                        if selected { ACCENT } else { Color32::WHITE },
                    );
                    if preset.custom {
                        ui.painter().text(
                            rect.right_top() - Vec2::new(5., -7.),
                            egui::Align2::RIGHT_TOP,
                            "Custom",
                            egui::FontId::proportional(10.),
                            MUTED,
                        );
                    }
                    if let Some(texture) = texture {
                        ui.painter().image(
                            texture.id(),
                            Rect::from_min_size(
                                rect.left_bottom() - Vec2::new(-5., 33.),
                                Vec2::new(240., 32.),
                            ),
                            Rect::from_min_max(Pos2::ZERO, egui::pos2(1., 1.)),
                            Color32::WHITE,
                        );
                    }
                    if response
                        .on_hover_text(format!(
                            "{} · {}{}",
                            preset.name,
                            preset.category,
                            if preset.category == "Blend" {
                                " · choose Smudge to blend existing paint"
                            } else {
                                ""
                            }
                        ))
                        .clicked()
                    {
                        self.select_brush_preset(preset);
                    }
                }
                if count == 0 {
                    ui.label(RichText::new("No matching brushes").small().color(MUTED));
                }
            });
        ui.add_space(6.);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Name").small().color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut self.brush_name).desired_width(215.));
        });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Category").small().color(MUTED));
            ui.add(egui::TextEdit::singleline(&mut self.brush_save_category).desired_width(195.));
        });
        let custom = self
            .brush_preset
            .as_ref()
            .is_some_and(|id| presets.iter().any(|p| p.id == *id && p.custom));
        ui.horizontal(|ui| {
            if ui
                .small_button("Save new")
                .on_hover_text("Save the current settings as a custom brush")
                .clicked()
            {
                self.save_brush_preset(false);
            }
            if ui
                .add_enabled(custom, egui::Button::new("Update").small())
                .clicked()
            {
                self.save_brush_preset(true);
            }
            if ui
                .add_enabled(custom, egui::Button::new("Delete").small())
                .clicked()
            {
                let result = library
                    .lock()
                    .unwrap()
                    .delete(self.brush_preset.as_deref().unwrap());
                match result {
                    Ok(()) => {
                        self.brush_preset = None;
                        self.shared.lock().unwrap().status = "Custom brush deleted".into();
                    }
                    Err(e) => self.shared.lock().unwrap().status = e,
                }
            }
        });
        if let Some(error) = error {
            ui.label(RichText::new(error).small().color(Color32::LIGHT_RED));
        }
    }
    pub(super) fn select_brush_preset(&mut self, preset: &crate::brush_library::Preset) {
        if preset.category == "Blend" {
            self.select_tool(Tool::Smudge);
        } else if self.tool == Tool::Smudge {
            self.select_tool(Tool::Brush);
        }
        self.brush = preset.settings;
        self.radius = preset.settings.radius;
        self.brush_preset = Some(preset.id.clone());
        self.brush_name = preset.name.clone();
        self.brush_save_category = preset.category.clone();
        self.store_current_tool_profile();
    }
    pub(super) fn save_brush_preset(&mut self, update: bool) {
        let library = self.shared.lock().unwrap().brush_library.clone();
        let mut settings = self.brush;
        settings.radius = self.radius;
        let result = library.lock().unwrap().save(
            if update {
                self.brush_preset.as_deref()
            } else {
                None
            },
            &self.brush_name,
            &self.brush_save_category,
            settings,
        );
        match result {
            Ok(preset) => {
                self.select_brush_preset(&preset);
                self.shared.lock().unwrap().status = "Custom brush saved".into();
            }
            Err(error) => self.shared.lock().unwrap().status = error,
        }
    }
}
