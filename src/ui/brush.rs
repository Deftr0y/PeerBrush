use super::*;
impl PeerBrush {
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
        json!({"op":"paint","layer":self.selected,"points":points,"radius":self.radius,"color":color,
            "mask":self.mask,"step":self.mask_step,"erase":self.tool==Tool::Eraser,
            "hardness":b.hardness,"opacity":b.opacity,"flow":b.flow,"spacing":b.spacing,
            "roundness":b.roundness,"angle":b.angle,"smoothing":b.smoothing})
    }
    pub(super) fn brush_settings(&mut self, ctx: &egui::Context) {
        let mut open = true;
        egui::Window::new("Brush")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_pos(egui::pos2(85.0, 125.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for name in ["Hard round", "Soft round", "Calligraphy"] {
                        if ui.selectable_label(false, name).clicked() {
                            self.brush.hardness = if name == "Soft round" { 0.0 } else { 1.0 };
                            self.brush.roundness = if name == "Calligraphy" { 0.2 } else { 1.0 };
                            self.brush.angle = if name == "Calligraphy" { -35.0 } else { 0.0 };
                            self.brush.spacing = 0.15;
                        }
                    }
                });
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
                                "Opacity",
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
                    });
            });
        self.show_brush = open;
    }
}
