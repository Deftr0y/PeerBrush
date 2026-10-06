use super::*;
#[derive(Clone)]
pub(super) enum Target {
    Foreground,
    Background,
    MaskForeground,
    MaskBackground,
    Layer(String),
}
pub(super) struct Editor {
    target: Target,
    rgba: Pixel,
    original: Pixel,
    hex: String,
    hsv: egui::ecolor::Hsva,
    gradient: Option<(u16, TextureHandle)>,
    gesture: String,
}
pub(super) fn format_hex(p: Pixel) -> String {
    if p[3] == 255 {
        format!("{:02X}{:02X}{:02X}", p[0], p[1], p[2])
    } else {
        format!("{:02X}{:02X}{:02X}{:02X}", p[0], p[1], p[2], p[3])
    }
}
pub(super) fn parse_hex(s: &str) -> Option<Pixel> {
    let s = s.trim().trim_start_matches('#');
    if !s.is_ascii() {
        return None;
    }
    let expanded = if s.len() == 3 {
        s.chars().flat_map(|c| [c, c]).collect::<String>()
    } else {
        s.into()
    };
    if ![6, 8].contains(&expanded.len()) {
        return None;
    }
    let mut p = [0, 0, 0, 255];
    for (i, pair) in expanded.as_bytes().chunks_exact(2).enumerate() {
        p[i] = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(p)
}
impl PeerBrush {
    pub(super) fn open_color(&mut self, target: Target, rgba: Pixel) {
        self.color_editor = Some(Editor {
            target,
            rgba,
            original: rgba,
            hex: format_hex(rgba),
            hsv: egui::ecolor::Hsva::from_srgba_unmultiplied(rgba),
            gradient: None,
            gesture: crate::engine::id(),
        });
    }
    pub(super) fn color_window(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.color_editor.take() else {
            return;
        };
        let mut open = true;
        egui::Window::new("Color")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(egui::pos2(85.0, 200.0))
            .show(ctx, |ui| {
                let before = editor.rgba;
                ui.horizontal(|ui| {
                    for (label, pixel) in [("Previous", editor.original), ("Current", editor.rgba)]
                    {
                        ui.vertical(|ui| {
                            let (rect, response) = ui
                                .allocate_exact_size(Vec2::new(122.0, 25.0), egui::Sense::click());
                            checker(ui.painter(), rect);
                            ui.painter().rect_filled(
                                rect,
                                2,
                                Color32::from_rgba_unmultiplied(
                                    pixel[0], pixel[1], pixel[2], pixel[3],
                                ),
                            );
                            controls::label(ui, label);
                            if response.clicked() && label == "Previous" {
                                editor.rgba = editor.original;
                                editor.hsv =
                                    egui::ecolor::Hsva::from_srgba_unmultiplied(editor.rgba);
                            }
                        });
                    }
                });
                ui.add_space(6.0);
                let mask = matches!(
                    editor.target,
                    Target::MaskForeground | Target::MaskBackground
                );
                if !mask {
                    let hue = (editor.hsv.h * 65535.0).round() as u16;
                    if editor.gradient.as_ref().map(|p| p.0) != Some(hue) {
                        let pixels = (0..128)
                            .flat_map(|y| {
                                (0..128).flat_map(move |x| {
                                    egui::ecolor::Hsva {
                                        h: hue as f32 / 65535.0,
                                        s: x as f32 / 127.0,
                                        v: 1.0 - y as f32 / 127.0,
                                        a: 1.0,
                                    }
                                    .to_srgba_unmultiplied()
                                })
                            })
                            .collect::<Vec<_>>();
                        let image = egui::ColorImage::from_rgba_unmultiplied([128, 128], &pixels);
                        if let Some((key, texture)) = &mut editor.gradient {
                            *key = hue;
                            texture.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            editor.gradient = Some((
                                hue,
                                ctx.load_texture(
                                    "color field",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ),
                            ));
                        }
                    }
                    ui.horizontal(|ui| {
                        let (rect, response) = ui
                            .allocate_exact_size(Vec2::splat(224.0), egui::Sense::click_and_drag());
                        if let Some((_, t)) = &editor.gradient {
                            ui.painter().image(
                                t.id(),
                                rect,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        if response.clicked() || response.dragged() {
                            if let Some(p) = response.interact_pointer_pos() {
                                editor.hsv.s = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                                editor.hsv.v =
                                    (1.0 - (p.y - rect.top()) / rect.height()).clamp(0.0, 1.0);
                                editor.rgba = editor.hsv.to_srgba_unmultiplied();
                            }
                        }
                        ui.painter().circle_stroke(
                            egui::pos2(
                                rect.left() + editor.hsv.s * rect.width(),
                                rect.top() + (1.0 - editor.hsv.v) * rect.height(),
                            ),
                            4.0,
                            Stroke::new(1.5_f32, Color32::WHITE),
                        );
                        let (rail, response) = ui.allocate_exact_size(
                            Vec2::new(20.0, 224.0),
                            egui::Sense::click_and_drag(),
                        );
                        for i in 0..128 {
                            let color = Color32::from(egui::ecolor::Hsva {
                                h: i as f32 / 127.0,
                                s: 1.0,
                                v: 1.0,
                                a: 1.0,
                            });
                            ui.painter().rect_filled(
                                Rect::from_min_size(
                                    rail.min + Vec2::new(0.0, i as f32 * rail.height() / 128.0),
                                    Vec2::new(rail.width(), rail.height() / 128.0 + 0.5),
                                ),
                                0,
                                color,
                            );
                        }
                        if response.clicked() || response.dragged() {
                            if let Some(p) = response.interact_pointer_pos() {
                                editor.hsv.h = ((p.y - rail.top()) / rail.height()).clamp(0.0, 1.0);
                                editor.rgba = editor.hsv.to_srgba_unmultiplied();
                            }
                        }
                        let y = rail.top() + editor.hsv.h * rail.height();
                        ui.painter().line_segment(
                            [
                                egui::pos2(rail.left() - 2.0, y),
                                egui::pos2(rail.right() + 2.0, y),
                            ],
                            Stroke::new(2.0_f32, Color32::WHITE),
                        );
                    });
                }
                egui::Grid::new("color channels")
                    .spacing(Vec2::new(9.0, 6.0))
                    .show(ui, |ui| {
                        let count = if mask { 1 } else { 4 };
                        for (i, label) in ["R", "G", "B", "Alpha"].iter().enumerate().take(count) {
                            controls::label(ui, if mask { "Value" } else { label });
                            let mut v = editor.rgba[i] as f32;
                            let response =
                                controls::range(ui, i, &mut v, 0.0..=255.0, 224.0, "", 0, false);
                            if response.changed() {
                                editor.rgba[i] = v.round() as u8;
                                if mask {
                                    editor.rgba = [v as u8, v as u8, v as u8, 255];
                                }
                                let hsv = egui::ecolor::Hsva::from_srgba_unmultiplied(editor.rgba);
                                editor.hsv = egui::ecolor::Hsva {
                                    h: if hsv.s == 0.0 { editor.hsv.h } else { hsv.h },
                                    ..hsv
                                };
                            }
                            ui.end_row();
                        }
                    });
                if editor.rgba != before {
                    editor.hex = format_hex(editor.rgba);
                }
                ui.horizontal(|ui| {
                    controls::label(ui, "HEX");
                    ui.label("#");
                    if ui
                        .add(
                            egui::TextEdit::singleline(&mut editor.hex)
                                .frame(false)
                                .desired_width(150.0),
                        )
                        .changed()
                    {
                        if let Some(rgba) = parse_hex(&editor.hex) {
                            editor.rgba = rgba;
                            editor.hsv = egui::ecolor::Hsva::from_srgba_unmultiplied(rgba);
                        }
                    }
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(format!("#{}", format_hex(editor.rgba)));
                    }
                });
                if parse_hex(&editor.hex).is_none() {
                    ui.label(
                        RichText::new("Use 3, 6 or 8 HEX digits")
                            .size(11.0)
                            .color(ACCENT),
                    );
                }
            });
        match &editor.target {
            Target::Foreground => self.color = editor.rgba,
            Target::Background => self.background = editor.rgba,
            Target::MaskForeground => self.mask_value = gray(editor.rgba),
            Target::MaskBackground => self.mask_background = gray(editor.rgba),
            Target::Layer(id) => {
                let changed = self
                    .shared
                    .lock()
                    .unwrap()
                    .doc
                    .layers
                    .iter()
                    .find(|l| l.id == *id)
                    .is_some_and(|l| l.color != editor.rgba);
                if changed {
                    let result = self.shared.lock().unwrap().edit_with_gesture(
                        "human",
                        &[json!({"op":"layer.update","layer":id,"color":editor.rgba})],
                        None,
                        None,
                        "Fill color",
                        Some(&editor.gesture),
                    );
                    if let Err(error) = result {
                        self.message = error;
                    } else {
                        self.last_preview = None;
                    }
                }
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        }
        if open {
            self.color_editor = Some(editor);
        }
    }
}
fn gray(p: Pixel) -> u8 {
    (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722).round() as u8
}
pub(super) fn checker(painter: &egui::Painter, rect: Rect) {
    for y in 0..(rect.height() / 8.0).ceil() as i32 {
        for x in 0..(rect.width() / 8.0).ceil() as i32 {
            painter.rect_filled(
                Rect::from_min_size(
                    rect.min + Vec2::new(x as f32 * 8.0, y as f32 * 8.0),
                    Vec2::splat(8.0),
                )
                .intersect(rect),
                0,
                Color32::from_gray(if (x + y) % 2 == 0 { 80 } else { 55 }),
            );
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hex_supports_short_rgb_alpha_and_rejects_bad_values() {
        assert_eq!(parse_hex("#abc"), Some([170, 187, 204, 255]));
        assert_eq!(parse_hex("E9542080"), Some([233, 84, 32, 128]));
        assert_eq!(format_hex([233, 84, 32, 255]), "E95420");
        assert_eq!(parse_hex("１２３"), None);
        assert_eq!(parse_hex("GGGGGG"), None);
    }
}
