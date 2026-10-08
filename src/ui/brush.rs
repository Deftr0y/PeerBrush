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
        let mut command = json!({"op":if self.tool==Tool::Smudge {"smudge"}else{"paint"},"layer":self.selected,"points":points,"radius":self.radius,"color":color,
            "mask":self.mask,"step":self.mask_step,"erase":self.tool==Tool::Eraser,
            "hardness":b.hardness,"opacity":b.opacity,"flow":b.flow,"spacing":b.spacing,
            "roundness":b.roundness,"angle":b.angle,"smoothing":b.smoothing,
            "tip":b.tip.name(),"density":b.density,"grain":b.grain,"seed":b.seed,
            "pressure_size":b.pressure_size,"pressure_opacity":b.pressure_opacity,
            "taper_start":b.taper_start,"taper_end":b.taper_end,"taper_size":b.taper_size,"taper_opacity":b.taper_opacity,
            "wetness":b.wetness,"load":b.load,"pickup":b.pickup});
        if let Some(pressures) = pressures {
            command["pressures"] = json!(pressures);
        }
        command
    }
    pub(super) fn brush_settings(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new(if self.tool==Tool::Smudge {"Smudge"}else{"Brush"})
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_pos(egui::pos2(85.0, 125.0))
            .show(ctx, |ui| {
                self.tip_presets(ui);
                let key = format!("{:?}", self.brush);
                if self.brush_preview.as_ref().map(|p| p.0.as_str()) != Some(key.as_str()) {
                    let mut r = crate::raster::Raster::new(260, 54);
                    let mut settings = self.brush;
                    settings.radius = 16.0;
                    let points = (0..30)
                        .map(|i| [20.0 + i as f32 * 7.5, 27.0 + (i as f32 * 0.25).sin() * 7.0])
                        .collect::<Vec<_>>();
                    if crate::brush::paint(
                        &mut r,
                        &points,
                        settings,
                        [245, 242, 240, 255],
                        false,
                        None,
                    )
                    .is_ok()
                    {
                        let texture = ctx.load_texture(
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
                    ui.checkbox(&mut self.brush.taper_size,"Size");ui.checkbox(&mut self.brush.taper_opacity,"Opacity");
                });
            });
        self.show_brush = open;
    }
    fn tip_presets(&mut self, ui: &mut egui::Ui) {
        use crate::brush::{Settings, TipKind};
        ui.horizontal_wrapped(|ui| {
            for (name, tip, hardness, roundness, density, grain) in [
                ("Hard", TipKind::Round, 1.0, 1.0, 1.0, 2.0),
                ("Soft", TipKind::Round, 0.0, 1.0, 1.0, 2.0),
                ("Ink", TipKind::Round, 1.0, 0.2, 1.0, 2.0),
                ("Dry", TipKind::Dry, 0.85, 0.65, 0.65, 2.5),
                ("Chalk", TipKind::Chalk, 0.8, 1.0, 0.75, 3.0),
                ("Grain", TipKind::Grain, 0.9, 1.0, 0.65, 1.0),
                ("Bristle", TipKind::Bristle, 1.0, 0.7, 0.75, 3.0),
            ] {
                let id = egui::Id::new(("brush preset thumbnail", name));
                let texture = ui
                    .ctx()
                    .data_mut(|data| data.get_temp::<TextureHandle>(id))
                    .unwrap_or_else(|| {
                        let mut raster = crate::raster::Raster::new(44, 30);
                        let settings = Settings {
                            radius: 10.0,
                            tip,
                            hardness,
                            roundness,
                            density,
                            grain,
                            angle: if name == "Ink" { -35.0 } else { 0.0 },
                            ..Default::default()
                        };
                        let _ = crate::brush::paint(
                            &mut raster,
                            &[[12., 15.], [32., 15.]],
                            settings,
                            [245, 242, 240, 255],
                            false,
                            None,
                        );
                        let texture = ui.ctx().load_texture(
                            name,
                            egui::ColorImage::from_rgba_unmultiplied([44, 30], &raster.rgba()),
                            egui::TextureOptions::LINEAR,
                        );
                        ui.ctx()
                            .data_mut(|data| data.insert_temp(id, texture.clone()));
                        texture
                    });
                let (rect, response) =
                    ui.allocate_exact_size(Vec2::new(44.0, 48.0), egui::Sense::click());
                let selected = self.brush.tip == tip
                    && self.brush.hardness == hardness
                    && self.brush.roundness == roundness;
                let image = Rect::from_min_size(rect.min, Vec2::new(44.0, 30.0));
                ui.painter().image(
                    texture.id(),
                    image,
                    Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.painter().text(
                    rect.center_bottom() - Vec2::new(0., 8.),
                    egui::Align2::CENTER_CENTER,
                    name,
                    egui::FontId::proportional(10.0),
                    if selected { ACCENT } else { MUTED },
                );
                if selected {
                    ui.painter().line_segment(
                        [rect.left_bottom(), rect.right_bottom()],
                        Stroke::new(1.5_f32, ACCENT),
                    );
                }
                if response.on_hover_text(format!("{name} brush")).clicked() {
                    self.brush.tip = tip;
                    self.brush.hardness = hardness;
                    self.brush.roundness = roundness;
                    self.brush.density = density;
                    self.brush.grain = grain;
                    self.brush.angle = if name == "Ink" { -35.0 } else { 0.0 };
                    self.brush.spacing = 0.15;
                }
            }
        });
    }
}
