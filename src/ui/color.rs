use super::*;
use crate::raster::{project16, Pixel16};
use egui::ecolor::Hsva;

#[derive(Clone)]
pub(super) enum Target {
    Foreground,
    Background,
    MaskForeground,
    MaskBackground,
}
pub(super) struct Editor {
    target: Target,
    rgba: Pixel16,
    original: Pixel16,
    native_hex: bool,
    hex: String,
    hsv: Hsva,
    gradient: Option<(u16, TextureHandle)>,
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

fn opaque_hsv([r, g, b, _]: Pixel) -> Hsva {
    Hsva::from_srgba_unmultiplied([r, g, b, 255])
}

impl Editor {
    fn sync_picker(&mut self) {
        let [r, g, b, _] = self.rgba;
        self.hsv = if self.native_hex {
            Hsva::from_rgb([r, g, b].map(|word| {
                egui::ecolor::linear_from_gamma(f32::from(word) / 65535.)
            }))
        } else {
            opaque_hsv(self.rgba.map(project16))
        };
    }

    fn hex_value(&self) -> String {
        super::effect_color::format_hex(self.rgba.map(|word| f64::from(word) / 65535.), self.native_hex)
    }

    fn parsed_hex(&self) -> Option<Pixel16> {
        let ([r, g, b], alpha) = super::effect_color::parse_hex(&self.hex, self.native_hex, true)?;
        Some([r, g, b, alpha.unwrap_or(1.)].map(|value| (value * 65535.).round() as u16))
    }
}

fn picker_input(ui: &egui::Ui, response: &egui::Response) -> Option<Pos2> {
    let primary = egui::PointerButton::Primary;
    let clicked = response.clicked_by(primary);
    let dragging = response.dragged_by(primary)
        || response.is_pointer_button_down_on() && ui.input(|input| input.pointer.primary_down());
    if clicked || dragging {
        response.request_focus();
    }
    if !ui.is_enabled() || !ui.input(|i| i.focused && !i.key_pressed(egui::Key::Escape)) {
        return None;
    }
    if clicked || response.drag_stopped_by(primary) {
        let local = ui
            .ctx()
            .layer_transform_from_global(ui.layer_id())
            .unwrap_or_default();
        ui.input(|input| {
            primary_release_samples(input)
                .next()
                .map(|(pos, _)| local * pos)
        })
        .or_else(|| response.interact_pointer_pos())
    } else if dragging {
        response.interact_pointer_pos()
    } else {
        None
    }
}

fn picker_arrows(ui: &egui::Ui, response: &egui::Response) -> Vec2 {
    if !response.has_focus() || !ui.is_enabled() {
        return Vec2::ZERO;
    }
    ui.input_mut(|input| {
        if !input.focused || input.key_pressed(egui::Key::Escape) || input.pointer.primary_down() {
            return Vec2::ZERO;
        }
        let modifiers = if input.modifiers.shift {
            egui::Modifiers::SHIFT
        } else {
            egui::Modifiers::NONE
        };
        let mut count = |key| input.count_and_consume_key(modifiers, key) as f32;
        Vec2::new(
            count(egui::Key::ArrowRight) - count(egui::Key::ArrowLeft),
            count(egui::Key::ArrowDown) - count(egui::Key::ArrowUp),
        ) * if modifiers.shift { 0.1 } else { 0.01 }
    })
}

/// Display encoding stays with the caller, so artwork and effect settings keep
/// their existing color representations.
pub(super) fn picker(
    ui: &mut egui::Ui,
    hsv: &mut Hsva,
    field: &mut Option<(u16, TextureHandle)>,
    side: f32,
    display: impl Fn(Hsva) -> Pixel,
) -> Option<egui::Response> {
    const SAMPLES: u32 = 96;
    ui.horizontal(|ui| {
        let mut changed = None;
        let (rect, field_response) =
            ui.allocate_exact_size(Vec2::splat(side), egui::Sense::click_and_drag());
        let field_response = field_response
            .on_hover_text("Saturation and brightness · Arrow keys adjust the focused picker");
        field_response.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::WidgetType::Slider,
                field_response.enabled(),
                "Color saturation and brightness",
            )
        });
        if let Some(pos) = picker_input(ui, &field_response) {
            hsv.s = ((pos.x - rect.left()) / rect.width()).clamp(0., 1.);
            hsv.v = (1. - (pos.y - rect.top()) / rect.height()).clamp(0., 1.);
            changed = Some(field_response.clone());
        } else {
            let delta = picker_arrows(ui, &field_response);
            if delta != Vec2::ZERO {
                hsv.s = (hsv.s + delta.x).clamp(0., 1.);
                hsv.v = (hsv.v - delta.y).clamp(0., 1.);
                changed = Some(field_response.clone());
            }
        }
        let (rail, response) =
            ui.allocate_exact_size(Vec2::new(20., side), egui::Sense::click_and_drag());
        let response = response.on_hover_text("Hue · Arrow keys adjust the focused picker");
        if let Some(pos) = picker_input(ui, &response) {
            hsv.h = ((pos.y - rail.top()) / rail.height()).clamp(0., 1.);
            changed = Some(response.clone());
        } else {
            let delta = picker_arrows(ui, &response);
            if delta != Vec2::ZERO {
                hsv.h = (hsv.h + delta.x + delta.y).rem_euclid(1.);
                changed = Some(response.clone());
            }
        }
        response.widget_info(|| {
            egui::WidgetInfo::slider(response.enabled(), f64::from(hsv.h) * 360., "Color hue")
        });

        let hue = (hsv.h * 65535.).round() as u16;
        let last = (SAMPLES - 1) as f32;
        if field.as_ref().map(|(key, _)| *key) != Some(hue) {
            let bytes: Vec<_> = (0..SAMPLES * SAMPLES)
                .flat_map(|sample| {
                    display(Hsva::new(
                        f32::from(hue) / 65535.,
                        (sample % SAMPLES) as f32 / last,
                        1. - (sample / SAMPLES) as f32 / last,
                        1.,
                    ))
                })
                .collect();
            let image = egui::ColorImage::from_rgba_unmultiplied([SAMPLES as usize; 2], &bytes);
            if let Some((key, texture)) = field {
                *key = hue;
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                *field = Some((
                    hue,
                    ui.ctx()
                        .load_texture("color field", image, egui::TextureOptions::LINEAR),
                ));
            }
        }
        if let Some((_, texture)) = field {
            ui.painter().image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
        }
        ui.painter().circle_stroke(
            egui::pos2(
                rect.left() + hsv.s * rect.width(),
                rect.top() + (1. - hsv.v) * rect.height(),
            ),
            4.,
            Stroke::new(1.5, Color32::WHITE),
        );
        let step = rail.height() / SAMPLES as f32;
        for i in 0..SAMPLES {
            let [r, g, b, _] = display(Hsva::new(i as f32 / last, 1., 1., 1.));
            ui.painter().rect_filled(
                Rect::from_min_size(
                    rail.min + Vec2::new(0., i as f32 * step),
                    Vec2::new(rail.width(), step + 0.5),
                )
                .intersect(rail),
                0.,
                Color32::from_rgb(r, g, b),
            );
        }
        let y = rail.top() + hsv.h * rail.height();
        ui.painter().line_segment(
            [
                egui::pos2(rail.left() - 2., y),
                egui::pos2(rail.right() + 2., y),
            ],
            Stroke::new(2., Color32::WHITE),
        );
        icons::paint_focus(ui, &field_response);
        icons::paint_focus(ui, &response);
        changed
    })
    .inner
}

fn mask_ramp(ui: &mut egui::Ui, value: &mut u16, max: u16) {
    let (rect, response) =
        ui.allocate_exact_size(Vec2::new(198., 22.), egui::Sense::click_and_drag());
    let response = response.on_hover_text("Black conceals, white reveals · Arrow keys adjust the mask value");
    if let Some(pos) = picker_input(ui, &response) {
        let position = ((pos.x - rect.left()) / rect.width()).clamp(0., 1.);
        *value = (position * f32::from(max)).round() as u16;
    } else {
        let arrows = picker_arrows(ui, &response);
        let step = ((arrows.x - arrows.y) * 100.).round() as i32;
        *value = (i32::from(*value) + step).clamp(0, i32::from(max)) as u16;
    }
    response.widget_info(|| {
        egui::WidgetInfo::slider(response.enabled(), f64::from(*value), "Mask value")
    });
    gradient_stops::paint_ramp(ui.painter(), rect, |position| {
        Color32::from_gray((position * 255.).round() as u8)
    });
    let x = (rect.left() + rect.width() * (f32::from(*value) / f32::from(max)))
        .clamp(rect.left() + 2.5, rect.right() - 2.5);
    let marker = Rect::from_center_size(egui::pos2(x, rect.center().y), Vec2::new(5., 18.));
    ui.painter().rect_filled(marker, 2., Color32::WHITE);
    ui.painter().rect_stroke(marker, 2., Stroke::new(1., Color32::BLACK), egui::StrokeKind::Inside);
    icons::paint_focus(ui, &response);
}

impl PeerBrush {
    pub(super) fn foreground_words(&self, depth: u16) -> Option<Pixel16> {
        if depth != 16 {
            return None;
        }
        if self.mask {
            let alpha = self.color16.map_or(u16::from(self.color[3]) * 257, |rgba| rgba[3]);
            let gray = self.mask_value16.unwrap_or(u16::from(self.mask_value) * 257);
            return (self.mask_value16.is_some() || alpha % 257 != 0)
                .then_some([gray, gray, gray, alpha]);
        }
        self.color16
    }

    pub(super) fn palette_value(&self, target: &Target, native: bool) -> Pixel16 {
        let value = match target {
            Target::Foreground => (self.color, self.color16),
            Target::Background => (self.background, self.background16),
            Target::MaskForeground => {
                let v = self.mask_value;
                ([v, v, v, 255], self.mask_value16.map(|gray| [gray, gray, gray, u16::MAX]))
            }
            Target::MaskBackground => {
                let v = self.mask_background;
                ([v, v, v, 255], self.mask_background16.map(|gray| [gray, gray, gray, u16::MAX]))
            }
        };
        match value {
            (_, Some(words)) if native => words,
            (bytes, _) => bytes.map(|value| u16::from(value) * 257),
        }
    }

    pub(super) fn open_color(&mut self, target: Target) {
        let native_hex = self.doc_snapshot.view().depth() == 16;
        let rgba = self.palette_value(&target, native_hex);
        let mut editor = Editor {
            target,
            rgba,
            original: rgba,
            native_hex,
            hex: String::new(),
            hsv: opaque_hsv(rgba.map(project16)),
            gradient: None,
        };
        editor.sync_picker();
        editor.hex = editor.hex_value();
        self.color_editor = Some(editor);
    }
    pub(super) fn color_window(&mut self, ctx: &egui::Context) {
        let Some(mut editor) = self.color_editor.take() else {
            return;
        };
        let native_hex = self.doc_snapshot.view().depth() == 16;
        if native_hex != editor.native_hex {
            self.open_color(editor.target);
            return;
        }
        let current = self.palette_value(&editor.target, native_hex);
        if current != editor.rgba {
            editor.rgba = current;
            editor.sync_picker();
            editor.hex = editor.hex_value();
        }
        let mut open = true;
        egui::Window::new("Color")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(egui::pos2(85.0, 200.0))
            .show(ctx, |ui| {
                let before = editor.rgba;
                let swatch = |ui: &egui::Ui, rect, rgba: Pixel16| {
                    let [r, g, b, a] = rgba.map(project16);
                    checker(ui.painter(), rect);
                    ui.painter().rect_filled(rect, 2, Color32::from_rgba_unmultiplied(r, g, b, a));
                };
                let mut current_swatch = None;
                ui.horizontal(|ui| {
                    for (label, pixel) in [("Previous", editor.original), ("Current", editor.rgba)]
                    {
                        ui.vertical(|ui| {
                            let previous = label == "Previous";
                            let sense = if previous {
                                egui::Sense::click()
                            } else {
                                egui::Sense::hover()
                            };
                            let (rect, response) =
                                ui.allocate_exact_size(Vec2::new(122.0, 25.0), sense);
                            controls::label(ui, label);
                            if previous {
                                swatch(ui, rect, pixel);
                                let response = response.on_hover_text("Restore the previous color");
                                response.widget_info(|| {
                                    egui::WidgetInfo::labeled(
                                        egui::WidgetType::Button,
                                        ui.is_enabled(),
                                        "Restore the previous color",
                                    )
                                });
                                icons::paint_focus(ui, &response);
                                if response.clicked() {
                                    editor.rgba = editor.original;
                                    editor.sync_picker();
                                    editor.hex = editor.hex_value();
                                }
                            } else {
                                current_swatch = Some(rect);
                            }
                        });
                    }
                });
                ui.add_space(6.0);
                let mask = matches!(
                    editor.target,
                    Target::MaskForeground | Target::MaskBackground
                );
                if mask {
                    ui.horizontal(|ui| {
                        controls::label(ui, "Value");
                        let (mut value, max) = if native_hex {
                            (editor.rgba[0], u16::MAX)
                        } else {
                            (u16::from(project16(editor.rgba[0])), 255)
                        };
                        mask_ramp(ui, &mut value, max);
                        let exact = controls::numeric(ui, &mut value, 0..=max)
                            .on_hover_text(format!("Click to type an exact mask value from 0 to {max}"));
                        exact.widget_info(|| {
                            egui::WidgetInfo::slider(exact.enabled(), f64::from(value), "Exact mask value")
                        });
                        let gray = if native_hex { value } else { value * 257 };
                        editor.rgba = [gray, gray, gray, u16::MAX];
                    });
                } else {
                    let before_rgb = editor.hsv.to_rgb();
                    if picker(ui, &mut editor.hsv, &mut editor.gradient, 224., |hsv| {
                        hsv.to_srgba_unmultiplied()
                    })
                    .is_some()
                    {
                        let selected = editor.hsv.to_rgb();
                        if selected != before_rgb {
                            let words = if native_hex {
                                selected.map(|value| {
                                    (egui::ecolor::gamma_from_linear(value) * 65535.).round() as u16
                                })
                            } else {
                                editor.hsv.to_srgb().map(|value| u16::from(value) * 257)
                            };
                            editor.rgba[..3].copy_from_slice(&words);
                        }
                    }
                    ui.horizontal(|ui| {
                        controls::label(ui, "Opacity");
                        let mut opacity = f32::from(editor.rgba[3]) * (100. / 65535.);
                        if controls::range(
                            ui,
                            "color opacity",
                            &mut opacity,
                            0.0..=100.,
                            192.,
                            "%",
                            if native_hex { 4 } else { 1 },
                            false,
                        )
                        .changed()
                        {
                            editor.rgba[3] = if native_hex {
                                (opacity * (65535. / 100.)).round() as u16
                            } else {
                                (opacity * (255. / 100.)).round() as u16 * 257
                            };
                        }
                    });
                }
                if editor.rgba != before {
                    editor.hex = editor.hex_value();
                }
                ui.horizontal(|ui| {
                    controls::label(ui, "HEX");
                    ui.label("#");
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut editor.hex)
                            .frame(false)
                            .desired_width(150.0),
                    );
                    edit.clone().on_hover_text(if mask {
                        "Pasted colors are converted to grayscale"
                    } else if native_hex {
                        "Use 12 digits for exact RGB or 16 for exact RGBA"
                    } else {
                        "Enter RGB, RRGGBB or RRGGBBAA"
                    });
                    if edit.changed() {
                        if let Some(rgba) = editor.parsed_hex() {
                            editor.rgba = if mask {
                                let value = if native_hex {
                                    let [r, g, b, _] = rgba.map(f64::from);
                                    (r * 0.2126 + g * 0.7152 + b * 0.0722).round() as u16
                                } else {
                                    u16::from(gray(rgba.map(project16))) * 257
                                };
                                [value, value, value, 65535]
                            } else {
                                rgba
                            };
                            editor.sync_picker();
                        }
                    }
                    if mask && edit.lost_focus() && editor.parsed_hex().is_some() {
                        editor.hex = editor.hex_value();
                    }
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(format!("#{}", editor.hex_value()));
                    }
                });
                if editor.parsed_hex().is_none() {
                    ui.label(
                        RichText::new(if native_hex { "Use 3, 6, 8, 12 or 16 HEX digits" } else { "Use 3, 6 or 8 HEX digits" })
                            .size(11.0)
                            .color(ACCENT),
                    );
                }
                if let Some(rect) = current_swatch {
                    swatch(ui, rect, editor.rgba);
                }
            });
        if editor.rgba != current {
            let pixel = editor.rgba.map(project16);
            let words = native_hex.then_some(editor.rgba);
            match &editor.target {
                Target::Foreground => (self.color, self.color16) = (pixel, words),
                Target::Background => (self.background, self.background16) = (pixel, words),
                Target::MaskForeground => {
                    self.mask_value = pixel[0];
                    self.mask_value16 = words.map(|rgba| rgba[0]);
                }
                Target::MaskBackground => {
                    self.mask_background = pixel[0];
                    self.mask_background16 = words.map(|rgba| rgba[0]);
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

    #[test]
    fn picker_keeps_exact_release_position_and_supports_focused_arrows() {
        let ctx = egui::Context::default();
        let mut hsv = opaque_hsv([51, 120, 196, 0]);
        assert_eq!(hsv.to_srgba_unmultiplied(), [51, 120, 196, 255]);
        let mut field = None;
        let frame = |hsv: &mut Hsva, field: &mut Option<(u16, TextureHandle)>, events| {
            ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::splat(300.))),
                    events,
                    ..Default::default()
                },
                |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| {
                        picker(ui, hsv, field, 100., |value| value.to_srgba_unmultiplied());
                    });
                },
            )
        };
        frame(&mut hsv, &mut field, vec![]);
        let initial = frame(&mut hsv, &mut field, vec![]);
        let texture = field.as_ref().unwrap().1.id();
        let rect = initial
            .shapes
            .iter()
            .find_map(|shape| match &shape.shape {
                egui::Shape::Mesh(mesh) if mesh.texture_id == texture => Some(mesh.calc_bounds()),
                _ => None,
            })
            .expect("Rendered saturation and brightness field");
        let button = |pos, pressed| egui::Event::PointerButton {
            pos,
            pressed,
            button: egui::PointerButton::Primary,
            modifiers: Default::default(),
        };
        let start = rect.min + Vec2::new(15., 20.);
        let end = rect.min + Vec2::new(62., 40.);
        frame(
            &mut hsv,
            &mut field,
            vec![egui::Event::PointerMoved(start), button(start, true)],
        );
        frame(
            &mut hsv,
            &mut field,
            vec![egui::Event::PointerMoved(rect.center())],
        );
        frame(
            &mut hsv,
            &mut field,
            vec![button(end, false), egui::Event::PointerMoved(rect.max)],
        );
        assert!((hsv.s - 0.62).abs() < 0.001);
        assert!((hsv.v - 0.60).abs() < 0.001);
        frame(
            &mut hsv,
            &mut field,
            vec![egui::Event::Key {
                key: egui::Key::ArrowUp,
                physical_key: Some(egui::Key::ArrowUp),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!((hsv.s - 0.62).abs() < 0.001);
        assert!((hsv.v - 0.61).abs() < 0.001);
    }
}
