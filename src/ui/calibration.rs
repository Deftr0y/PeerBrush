use super::*;
#[derive(Default)]
pub(super) struct Pad {
    document: Option<Document>,
    frozen: Option<crate::brush::Settings>,
    points: Vec<[f32; 2]>,
    forces: Vec<f32>,
    latest_force: Option<f32>,
    gesture_force: Option<f32>,
    texture: Option<TextureHandle>,
    rendered: Option<(usize, crate::brush::Settings)>,
}
impl Pad {
    fn clear_stroke(&mut self) {
        self.frozen = None;
        self.gesture_force = None;
        self.points.clear();
        self.forces.clear();
        self.texture = None;
        self.rendered = None;
    }

    fn draw(&mut self, ui: &mut egui::Ui, settings: crate::brush::Settings, depth: u16) -> Rect {
        if self
            .document
            .as_ref()
            .is_none_or(|doc| doc.bit_depth != depth)
        {
            self.document = Document::new_depth(360, 120, depth).ok();
            self.clear_stroke();
        }
        let cancelled = PeerBrush::pointer_contact_cancelled(ui.ctx());
        if cancelled && self.frozen.is_some() {
            self.clear_stroke();
        }
        let force = PeerBrush::pointer_pressure(ui);
        if let Some(force) = force {
            self.latest_force = Some(force);
        }
        let response_settings = self.frozen.unwrap_or(settings);
        if let Some(force) = self.latest_force {
            ui.label(format!(
                "Last device force {:.0}% → response {:.0}%",
                force * 100.,
                response_settings.pressure_response(force) * 100.
            ));
        } else {
            ui.label("No device force received · mouse tests use full pressure");
        }
        let (rect, response) = ui.allocate_exact_size(
            Vec2::new(ui.available_width().min(360.), 120.),
            egui::Sense::click_and_drag(),
        );
        ui.painter()
            .rect_filled(rect, 0., Color32::from_rgb(35, 33, 38));
        if !cancelled
            && (response.clicked_by(egui::PointerButton::Primary)
                || response.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down()))
        {
            if let Some(pos) = ui.input(|i| i.pointer.interact_pos()) {
                if self.frozen.is_none() {
                    self.frozen = Some(settings);
                    self.points.clear();
                    self.forces.clear();
                    self.rendered = None;
                    self.gesture_force = force;
                }
                if force.is_some() {
                    self.gesture_force = force;
                }
                if self.points.len() < 256 {
                    let point = [
                        (pos.x - rect.left()) * 360. / rect.width(),
                        (pos.y - rect.top()).clamp(0., 120.),
                    ];
                    if self.points.last().is_none_or(|old| *old != point) {
                        self.points.push(point);
                        self.forces.push(self.gesture_force.unwrap_or(1.));
                    }
                }
            }
        }
        let rendering = self.frozen.unwrap_or(settings);
        let key = (self.points.len(), rendering);
        if !self.points.is_empty() && self.rendered != Some(key) {
            if let Some(document) = &self.document {
                let mut command = serde_json::to_value(rendering).unwrap();
                command["op"] = json!("paint");
                command["layer"] = json!(document.layers[0].id);
                command["points"] = json!(self.points);
                command["pressures"] = json!(self.forces);
                command["color"] = json!([233, 84, 32, 255]);
                if let Ok(preview) =
                    crate::engine::Engine::preview_edits(document.clone(), &[command])
                {
                    if let Ok((w, h, bytes, _)) = preview.preview(None, 360, None, false) {
                        let image = egui::ColorImage::from_rgba_unmultiplied(
                            [w as usize, h as usize],
                            &bytes,
                        );
                        if let Some(texture) = &mut self.texture {
                            texture.set(image, egui::TextureOptions::LINEAR);
                        } else {
                            self.texture = Some(ui.ctx().load_texture(
                                "pressure test",
                                image,
                                egui::TextureOptions::LINEAR,
                            ));
                        }
                        self.rendered = Some(key);
                    }
                }
            }
        }
        if let Some(texture) = &self.texture {
            ui.painter().with_clip_rect(rect).image(
                texture.id(),
                rect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1., 1.)),
                Color32::WHITE,
            );
        }
        if ui.input(|i| i.pointer.any_released()) {
            self.frozen = None;
            self.gesture_force = None;
        }
        ui.label(
            RichText::new("Pressure test · shared brush response · artwork stays unchanged")
                .small()
                .color(MUTED),
        );
        if ui.small_button("Clear test").clicked() {
            self.clear_stroke();
        }
        rect
    }
}

fn response_curve(
    ui: &mut egui::Ui,
    id: &str,
    settings: &mut crate::brush::Settings,
    mut display: crate::brush::Settings,
    ai: bool,
) -> egui::Response {
    ui.horizontal(|ui| {
        let (area, graph) = ui.allocate_exact_size(Vec2::new(118., 61.), egui::Sense::hover());
        graph.on_hover_text("Device force increases to the right. Brush response increases upward. Drag the point to reshape the curve.");
        let plot = Rect::from_min_max(area.min + Vec2::new(8., 5.), area.max - Vec2::new(8., 15.));
        let sample = |force: f32, display: crate::brush::Settings| {
            egui::pos2(
                plot.left() + force * plot.width(),
                plot.bottom() - display.pressure_response(force) * plot.height(),
            )
        };
        let middle = (display.pressure_min + display.pressure_max) * 0.5;
        let (handle, position) = controls::point_handle(
            ui,
            ui.make_persistent_id((id, "curve point")),
            sample(middle, display),
            "Pressure response at half calibrated force",
        );
        if let Some(position) = position {
            let response = (plot.bottom() - position.y) / plot.height();
            let gamma = -response.clamp(0.5_f32.powf(4.), 0.5_f32.powf(0.1)).log2();
            settings.pressure_gamma = gamma.clamp(0.1, 4.);
            display.pressure_gamma = settings.pressure_gamma;
        }
        let number = tool_settings::number(
            ui, id, &mut settings.pressure_gamma, display.pressure_gamma,
            0.1..=4., 66., "", 2, false, ai,
        );
        if number.changed() {
            display.pressure_gamma = settings.pressure_gamma;
        }
        number.widget_info(|| egui::WidgetInfo::labeled(
            egui::WidgetType::Slider, ui.is_enabled(),
            format!("Pressure response exponent {}", display.pressure_gamma),
        ));
        let axis = ui.visuals().weak_text_color();
        let color = if ai { AI_BLUE } else { ui.visuals().selection.stroke.color };
        let painter = ui.painter();
        painter.line_segment([plot.left_top(), plot.left_bottom()], Stroke::new(1., axis));
        painter.line_segment([plot.left_bottom(), plot.right_bottom()], Stroke::new(1., axis));
        painter.text(
            egui::pos2(plot.center().x, area.bottom()), egui::Align2::CENTER_BOTTOM,
            "Device force", egui::FontId::proportional(10.), axis,
        );
        let mut points = Vec::with_capacity(35);
        points.push(sample(0., display));
        points.extend((0..=32).map(|i| {
            let t = i as f32 / 32.;
            sample(display.pressure_min + t * (display.pressure_max - display.pressure_min), display)
        }));
        points.push(sample(1., display));
        painter.add(egui::Shape::line(points, Stroke::new(1.5, color)));
        painter.circle_filled(sample(middle, display), if handle.hovered() { 4. } else { 3. }, color);
        handle
    }).inner
}

impl PeerBrush {
    pub(super) fn pressure_curve(&mut self, ui: &mut egui::Ui, id: &str) {
        let time = ui.input(|input| input.time);
        let ai = self.tool_preferences_ai(time);
        let display = crate::brush::Settings {
            pressure_gamma: self.tool_display_number("pressure_gamma", self.brush.pressure_gamma, time),
            pressure_min: self.tool_display_number("pressure_min", self.brush.pressure_min, time),
            pressure_max: self.tool_display_number("pressure_max", self.brush.pressure_max, time),
            ..self.brush
        };
        response_curve(ui, id, &mut self.brush, display, ai);
    }

    pub(super) fn pressure_calibration(&mut self, ctx: &egui::Context) {
        if !self.show_calibration {
            return;
        }
        let mut open = true;
        egui::Window::new("Pressure calibration")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .default_width(390.)
            .show(ctx, |ui| {
                if !self.brush_tool() {
                    ui.label("Choose Brush, Eraser, Smudge, Clone or Heal to calibrate pressure");
                    ui.disable();
                }
                ui.label(format!("{} pressure", self.tool_caption(self.tool)));
                let ai = self.tool_preferences_ai(ui.input(|i| i.time));
                for minimum in [true, false] {
                    let (id, label) = if minimum {
                        ("pressure_min", "Minimum force")
                    } else {
                        ("pressure_max", "Maximum force")
                    };
                    let actual = if minimum {
                        self.brush.pressure_min
                    } else {
                        self.brush.pressure_max
                    };
                    let mut value = actual * 100.;
                    let shown = self.tool_display_number(id, actual, ui.input(|i| i.time)) * 100.;
                    let bounds = if minimum {
                        0.0..=(self.brush.pressure_max * 100. - 1.).max(0.)
                    } else {
                        (self.brush.pressure_min * 100. + 1.).min(100.)..=100.
                    };
                    ui.horizontal(|ui| {
                        controls::label(ui, label);
                        if tool_settings::number(
                            ui, id, &mut value, shown, bounds, 200., "%", 0, false, ai,
                        )
                        .changed()
                        {
                            if minimum {
                                self.brush.pressure_min = value / 100.;
                            } else {
                                self.brush.pressure_max = value / 100.;
                            }
                        }
                    });
                }
                ui.horizontal(|ui| {
                    controls::label(ui, "Response curve");
                    self.pressure_curve(ui, "calibration curve");
                });
                ui.horizontal(|ui| {
                    let force = self.calibration.latest_force;
                    if ui
                        .add_enabled(
                            force.is_some(),
                            egui::Button::new("Use light force").small(),
                        )
                        .clicked()
                    {
                        self.brush.pressure_min = (force.unwrap() - 0.01)
                            .clamp(0., (self.brush.pressure_max - 0.01).max(0.));
                    }
                    if ui
                        .add_enabled(
                            force.is_some_and(|v| v >= self.brush.pressure_min + 0.01),
                            egui::Button::new("Use firm force").small(),
                        )
                        .clicked()
                    {
                        self.brush.pressure_max =
                            force.unwrap().clamp(self.brush.pressure_min + 0.01, 1.);
                    }
                    if ui.small_button("Reset pressure").clicked() {
                        self.brush.pressure_min = 0.;
                        self.brush.pressure_max = 1.;
                        self.brush.pressure_gamma = 1.;
                    }
                });
                let mut settings = self.brush;
                settings.radius = self.radius;
                let depth = crate::engine::view::current(&self.shared.lock().unwrap()).depth();
                self.calibration.draw(ui, settings, depth);
                let state = self.tool_settings_store.lock().unwrap().state();
                let note = if state["error"].is_string() || state["save_error"].is_string() {
                    "Preference file is protected; changes apply to this session"
                } else if state["persistence"] == "current instance" {
                    "Calibration applies to this session"
                } else {
                    "Calibration saves with this tool’s preferences when the control finishes"
                };
                ui.label(RichText::new(note).small().color(MUTED));
            });
        self.show_calibration = open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_curve_drag_uses_release_force_and_keeps_the_calibrated_endpoints() {
        let ctx = egui::Context::default();
        let mut settings = crate::brush::Settings {
            pressure_min: 0.18,
            pressure_max: 0.82,
            ..Default::default()
        };
        let frame = |settings: &mut crate::brush::Settings, display, ai, events| {
            let mut center = None;
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400., 180.))),
                focused: true,
                events,
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    center = Some(response_curve(ui, "curve", settings, display, ai).rect.center());
                });
            });
            center.unwrap()
        };
        let pointer = |pos, pressed| egui::Event::PointerButton {
            pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
        };
        let initial = settings;
        let animated = crate::brush::Settings { pressure_gamma: 0.6, ..settings };
        frame(&mut settings, animated, true, vec![]);
        assert_eq!(settings, initial);
        let center = frame(&mut settings, initial, false, vec![]);
        frame(&mut settings, initial, false, vec![egui::Event::PointerMoved(center), pointer(center, true)]);
        let held = center + Vec2::new(0., 8.);
        frame(&mut settings, initial, false, vec![egui::Event::PointerMoved(held)]);
        let during_drag = settings;
        let release = center + Vec2::new(0., 12.);
        frame(&mut settings, during_drag, false, vec![
            egui::Event::PointerMoved(release), pointer(release, false),
        ]);
        assert!(settings.pressure_gamma > during_drag.pressure_gamma);
        assert!(during_drag.pressure_gamma > initial.pressure_gamma);
        assert_eq!(settings.pressure_min, initial.pressure_min);
        assert_eq!(settings.pressure_max, initial.pressure_max);
        assert!((settings.pressure_response(0.5) - (0.5 - 12. / 41.)).abs() < 0.0001);
    }

    #[test]
    fn genuine_force_test_pad_freezes_the_shared_brush_without_touching_artwork() {
        for depth in [8, 16] {
            let ctx = egui::Context::default();
            let mut pad = Pad::default();
            let settings = crate::brush::Settings {
                radius: 7.,
                pressure_min: 0.2,
                pressure_max: 0.8,
                pressure_gamma: 1.6,
                ..Default::default()
            };
            let mut rect = Rect::NOTHING;
            let mut draw = |pad: &mut Pad, settings, events| {
                let time = ctx.input(|i| i.time) + 0.1;
                let output = ctx.run(
                    egui::RawInput {
                        time: Some(time),
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(500., 300.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| {
                        egui::CentralPanel::default()
                            .show(ctx, |ui| rect = pad.draw(ui, settings, depth));
                    },
                );
                (rect, output)
            };
            let (rect, _) = draw(&mut pad, settings, vec![]);
            let start = rect.left_center() + Vec2::new(35., 0.);
            let end = start + Vec2::new(90., 0.);
            let force = |pos, phase, value| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase,
                pos,
                force: Some(value),
            };
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            draw(
                &mut pad,
                settings,
                vec![
                    egui::Event::PointerMoved(start),
                    force(start, egui::TouchPhase::Start, 0.2),
                    button(start, true),
                ],
            );
            let changed = crate::brush::Settings {
                pressure_gamma: 0.5,
                ..settings
            };
            let (_, output) = draw(
                &mut pad,
                changed,
                vec![
                    egui::Event::PointerMoved(end),
                    force(end, egui::TouchPhase::Move, 0.8),
                ],
            );
            assert_eq!(pad.frozen, Some(settings));
            assert_eq!(pad.forces, vec![0.2, 0.8]);
            assert_eq!(pad.latest_force, Some(0.8));
            assert!(pad.texture.is_some());
            assert!(pad.rendered.is_some());
            assert!(output
                .textures_delta
                .set
                .iter()
                .any(|(_, delta)| match &delta.image {
                    egui::ImageData::Color(image) => image.pixels.iter().any(|p| p.a() > 0),
                    _ => false,
                }));
            let scratch = pad.document.as_ref().unwrap();
            assert_eq!(scratch.bit_depth, depth);
            assert!(scratch.layers[0]
                .pixels
                .rgba16()
                .chunks_exact(4)
                .all(|p| p == [0, 0, 0, 0]));
            assert_eq!(scratch.revision, 0);
            draw(
                &mut pad,
                changed,
                vec![force(end, egui::TouchPhase::End, 0.), button(end, false)],
            );
            assert!(pad.frozen.is_none());
            draw(&mut pad, changed, vec![]);
            assert_eq!(pad.rendered.unwrap().1, changed);
            draw(
                &mut pad,
                changed,
                vec![
                    force(start, egui::TouchPhase::Start, 0.4),
                    egui::Event::PointerMoved(start),
                    button(start, true),
                    force(start, egui::TouchPhase::End, 0.),
                    button(start, false),
                    egui::Event::PointerGone,
                ],
            );
            assert_eq!(pad.forces, vec![0.4]);
            assert_eq!(pad.latest_force, Some(0.4));
            assert_eq!(pad.rendered, Some((1, changed)));
            assert!(pad.frozen.is_none());
            draw(
                &mut pad,
                changed,
                vec![
                    force(start, egui::TouchPhase::Start, 0.3),
                    egui::Event::PointerMoved(start),
                    button(start, true),
                ],
            );
            assert!(pad.texture.is_some() && pad.frozen.is_some());
            draw(
                &mut pad,
                changed,
                vec![force(start, egui::TouchPhase::Cancel, 0.), egui::Event::PointerGone],
            );
            assert!(pad.points.is_empty() && pad.forces.is_empty());
            assert!(pad.texture.is_none() && pad.frozen.is_none());
            assert_eq!(pad.latest_force, Some(0.3));
        }
    }
    #[test]
    fn visible_calibration_pad_captures_input_without_painting_the_canvas_behind_it() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.recovery.open = false;
            app.show_calibration = true;
            {
                let mut e = app.shared.lock().unwrap();
                e.resident_state_mut().expect("resident fixture").document =
                    Document::new_depth(64, 48, depth).unwrap();
                e.resident_state_mut()
                    .expect("resident fixture")
                    .undo
                    .clear();
            }
            let frame = |app: &mut PeerBrush, events| {
                let time = ctx.input(|i| i.time) + 0.1;
                ctx.run(
                    egui::RawInput {
                        time: Some(time),
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            frame(&mut app, vec![]);
            frame(&mut app, vec![]);
            let output = frame(&mut app, vec![]);
            let rect = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Rect(r)
                        if r.fill == Color32::from_rgb(35, 33, 38)
                            && r.rect.width() > 200.
                            && r.rect.height() == 120. =>
                    {
                        Some(r.rect)
                    }
                    _ => None,
                })
                .expect("Visible pressure pad missing");
            let before = app
                .shared
                .lock()
                .unwrap()
                .resident_state()
                .expect("resident fixture")
                .document
                .export_png()
                .unwrap();
            let start = rect.left_center() + Vec2::new(35., 0.);
            let end = start + Vec2::new(90., 0.);
            let button = |pos, pressed| egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            let force = |pos, phase, value| egui::Event::Touch {
                device_id: egui::TouchDeviceId(1),
                id: egui::TouchId(1),
                phase,
                pos,
                force: Some(value),
            };
            frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(start),
                    force(start, egui::TouchPhase::Start, 0.2),
                    button(start, true),
                ],
            );
            frame(
                &mut app,
                vec![
                    egui::Event::PointerMoved(end),
                    force(end, egui::TouchPhase::Move, 0.8),
                ],
            );
            frame(
                &mut app,
                vec![force(end, egui::TouchPhase::End, 0.), button(end, false)],
            );
            assert!(app.points.is_empty());
            assert_eq!(app.calibration.forces, vec![0.2, 0.8]);
            assert_eq!(
                app.shared
                    .lock()
                    .unwrap()
                    .resident_state()
                    .expect("resident fixture")
                    .document
                    .export_png()
                    .unwrap(),
                before
            );
            assert!(app
                .shared
                .lock()
                .unwrap()
                .resident_state()
                .expect("resident fixture")
                .undo
                .is_empty());
        }
    }
}
