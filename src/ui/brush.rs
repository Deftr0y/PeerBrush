use super::*;
impl PeerBrush {
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
                center + Vec2::new(x * cos - y * sin, x * sin + y * cos)
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
        self.edit(vec![self.stroke_command(points, color)], label);
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
            "pressure_size":b.pressure_size,"pressure_opacity":b.pressure_opacity,"pressure_gamma":b.pressure_gamma,
            "taper_start":b.taper_start,"taper_end":b.taper_end,"taper_size":b.taper_size,"taper_opacity":b.taper_opacity,
            "wetness":b.wetness,"load":b.load,"pickup":b.pickup});
        if let Some(pressures) = pressures {
            command["pressures"] = json!(pressures);
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
        ui.label(RichText::new(&self.brush_name).color(ACCENT));
        ui.horizontal(|ui| {
            ui.label(RichText::new("Tip").small().color(MUTED));
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
            ui.image((texture.id(), Vec2::new(260.0, 54.0)));
        }
        egui::Grid::new("brush parameters")
                    .spacing(Vec2::new(10.0, 8.0))
                    .show(ui, |ui| {
                        let mut size = self.radius * 2.0;
                        controls::label(ui, "Size");
                        controls::range(
                            ui,
                            "tip size",
                            &mut size,
                            1.0..=1024.0,
                            200.0,
                            " px",
                            0,
                            true,
                        );
                        ui.end_row();
                        self.radius = size / 2.0;
                        for (id, label, value, min, max, suffix) in [
                            (
                                "hardness",
                                "Hardness",
                                &mut self.brush.hardness,
                                0.0,
                                100.0,
                                "%",
                            ),
                            (
                                "opacity",
                                if self.tool==Tool::Smudge {"Strength"}else{"Opacity"},
                                &mut self.brush.opacity,
                                0.0,
                                100.0,
                                "%",
                            ),
                            ("flow", "Flow", &mut self.brush.flow, 0.0, 100.0, "%"),
                            (
                                "spacing",
                                "Spacing",
                                &mut self.brush.spacing,
                                1.0,
                                200.0,
                                "%",
                            ),
                            (
                                "roundness",
                                "Roundness",
                                &mut self.brush.roundness,
                                5.0,
                                100.0,
                                "%",
                            ),
                            (
                                "smoothing",
                                "Smoothing",
                                &mut self.brush.smoothing,
                                0.0,
                                100.0,
                                "%",
                            ),
                        ] {
                            let mut percent = *value * 100.0;
                            controls::label(ui, label);
                            controls::range(
                                ui,
                                id,
                                &mut percent,
                                min..=max,
                                200.0,
                                suffix,
                                0,
                                false,
                            );
                            *value = percent / 100.0;
                            ui.end_row();
                        }
                        controls::label(ui, "Angle");
                        controls::range(
                            ui,
                            "tip angle",
                            &mut self.brush.angle,
                            -180.0..=180.0,
                            200.0,
                            "°",
                            0,
                            false,
                        );
                        ui.end_row();
                        controls::label(ui, "Pressure curve");
                        controls::range(ui,"pressure response",&mut self.brush.pressure_gamma,0.1..=4.0,200.0,"",2,false).on_hover_text("1 is linear. Below 1 responds more at light pressure; above 1 needs more force.");
                        ui.end_row();
                        if self.brush.tip!=crate::brush::TipKind::Round {
                            let mut density=self.brush.density*100.0;
                            controls::label(ui,"Density");controls::range(ui,"tip density",&mut density,5.0..=100.0,200.0,"%",0,false);ui.end_row();self.brush.density=density/100.0;
                            controls::label(ui,"Grain");controls::range(ui,"tip grain",&mut self.brush.grain,0.5..=32.0,200.0," px",1,false);ui.end_row();
                        }
                        for (id,label,value) in [("taper start","Start",&mut self.brush.taper_start),("taper end","End",&mut self.brush.taper_end)] {
                            controls::label(ui,label);controls::range(ui,id,value,0.0..=512.0,200.0," px",0,false).on_hover_text("Taper length along the stroke");ui.end_row();
                        }
                        if self.tool==Tool::Smudge {
                            for (id,label,value) in [("wetness","Wet",&mut self.brush.wetness),("paint load","Load",&mut self.brush.load),("paint pickup","Pickup",&mut self.brush.pickup)] {
                                let mut percent=*value*100.0;controls::label(ui,label);controls::range(ui,id,&mut percent,0.0..=100.0,200.0,"%",0,false);ui.end_row();*value=percent/100.0;
                            }
                        }
                    });
        ui.horizontal(|ui| {
                    ui.label(RichText::new("Pressure").small().color(MUTED));
                    ui.checkbox(&mut self.brush.pressure_size,"Size").on_hover_text("Uses genuine device force when supplied, or explicit agent pressure. Mouse strokes use taper.");
                    ui.checkbox(&mut self.brush.pressure_opacity,"Opacity");
                });
        ui.horizontal(|ui| {
            ui.label(RichText::new("Taper").small().color(MUTED));
            ui.checkbox(&mut self.brush.taper_size, "Size");
            ui.checkbox(&mut self.brush.taper_opacity, "Opacity");
        });
    }
    fn preset_browser(&mut self, ui: &mut egui::Ui) {
        let library = self.shared.lock().unwrap().brush_library.clone();
        let (presets, error) = {
            let l = library.lock().unwrap();
            (l.presets(), l.error.clone())
        };
        self.brush_thumbnails
            .retain(|id, _| presets.iter().any(|p| &p.id == id));
        ui.add(
            egui::TextEdit::singleline(&mut self.brush_search)
                .hint_text("Search brushes")
                .desired_width(ui.available_width()),
        );
        let mut categories = vec!["All".to_owned(), "Custom".to_owned()];
        for category in crate::brush_library::CATEGORIES {
            categories.push((*category).into());
        }
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
            self.tool = Tool::Smudge;
        } else if self.tool == Tool::Smudge {
            self.tool = Tool::Brush;
        }
        self.brush = preset.settings;
        self.radius = preset.settings.radius;
        self.brush_preset = Some(preset.id.clone());
        self.brush_name = preset.name.clone();
        self.brush_save_category = preset.category.clone();
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
