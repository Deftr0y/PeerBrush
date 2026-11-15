use super::*;
use crate::engine::primitive::{gradient_color, GradientDirection};

pub(super) struct Drag {
    source: selection::GestureSource,
    screen: Pos2,
    start: [f32; 2],
    tool: Tool,
    mask: bool,
    step: Option<String>,
    color: [u8; 4],
    color16: Option<crate::raster::Pixel16>,
    direction: GradientDirection,
    active: bool,
    previewing: bool,
    id: String,
}

pub(super) struct FillPress {
    source: selection::GestureSource,
    mask: bool,
    step: Option<String>,
    selection: HashSet<String>,
}

fn paint_gradient_swatch(
    painter: &egui::Painter,
    rect: Rect,
    color: [u8; 4],
    direction: GradientDirection,
) {
    color::checker(painter, rect);
    let transparent = gradient_color(color, 1.);
    let colors = match direction {
        GradientDirection::Right => [color, transparent, transparent, color],
        GradientDirection::Left => [transparent, color, color, transparent],
        GradientDirection::Down => [color, color, transparent, transparent],
        GradientDirection::Up => [transparent, transparent, color, color],
    };
    let mut mesh = egui::Mesh::default();
    for (pos, [r, g, b, a]) in [rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom()]
        .into_iter().zip(colors)
    {
        mesh.colored_vertex(pos, Color32::from_rgba_unmultiplied(r, g, b, a));
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

impl Drag {
    pub(super) fn observes(&self, app: &PeerBrush, doc: DocumentView<'_>) -> bool {
        self.source.observes(app, doc)
            && self.tool == app.tool
            && self.mask == app.mask
            && self.step == app.mask_step
    }

    pub(super) fn live_key(&self) -> Option<&str> {
        self.previewing.then_some(self.id.as_str())
    }
}

impl PeerBrush {
    pub(super) fn gradient_toolbar(&mut self, ui: &mut egui::Ui) {
        let foreground = self.stroke_color();
        let label = if self.mask {
            "Gradient mask value"
        } else {
            "Gradient foreground color"
        };
        let response = ui
            .add_sized([72., 24.], egui::Button::new("").frame(false))
            .on_hover_text(format!("{label} · fades to transparency"));
        response.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), label)
        });
        if response.clicked() {
            let target = if self.mask {
                color::Target::MaskForeground
            } else {
                color::Target::Foreground
            };
            self.open_color(target);
        }
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 3.;
            for (direction, label) in [
                (GradientDirection::Right, "Fade left to right"),
                (GradientDirection::Left, "Fade right to left"),
                (GradientDirection::Down, "Fade top to bottom"),
                (GradientDirection::Up, "Fade bottom to top"),
            ] {
                let selected = self.gradient_direction == direction;
                let button = ui.add_sized([26., 24.], egui::Button::new("").selected(selected))
                    .on_hover_text(format!("{label} in document coordinates"));
                button.widget_info(|| egui::WidgetInfo::selected(
                    egui::WidgetType::RadioButton, button.enabled(), selected, label,
                ));
                paint_gradient_swatch(ui.painter(), button.rect.shrink(4.), [235, 235, 235, 255], direction);
                icons::paint_focus(ui, &button);
                if button.clicked() {
                    self.gradient_direction = direction;
                }
            }
        });
        paint_gradient_swatch(ui.painter(), response.rect.shrink(3.), foreground, self.gradient_direction);
        icons::paint_focus(ui, &response);
    }

    fn invalidate_primitive_preview(&mut self) {
        self.preview_epoch.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.last_preview = None;
        self.pending = false;
    }

    pub(super) fn fill_canvas(
        &mut self,
        ui: &egui::Ui,
        doc: DocumentView<'_>,
        response: &egui::Response,
        mut pending: Option<FillPress>,
    ) {
        if self.busy || self.edit_worker.pending() || doc.read_only() || Self::pointer_contact_cancelled(ui.ctx())
            || ui.input(|input| {
                !input.focused || input.key_pressed(egui::Key::Escape)
                    || input.key_down(egui::Key::Space)
                    || input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
            })
        {
            return;
        }
        let (press, release) = ui.input(|input| canvas_button_samples(input, pending.is_some()));
        if press.is_some_and(|pos| canvas_press_hits(ui, response, pos)) {
            match self.canvas_gesture_source(doc) {
                Ok(source) => pending = Some(FillPress {
                    source,
                    mask: self.mask,
                    step: self.mask_step.clone(),
                    selection: self.selection_layers.clone(),
                }),
                Err(error) => self.message = error.into(),
            }
        }
        let Some(gesture) = pending.filter(|gesture| {
            gesture.source.observes(self, doc) && gesture.mask == self.mask
                && gesture.step == self.mask_step && gesture.selection == self.selection_layers
        }) else { return; };
        if let Some((pos, _)) = release {
            if response.clicked_by(egui::PointerButton::Primary) && canvas_press_hits(ui, response, pos) {
                if !self.selection_layers.contains(&self.selected) {
                    self.selection_layers = HashSet::from([self.selected.clone()]);
                }
                let color = self.stroke_color();
                let color16 = self.foreground_words(doc.depth());
                let commands = self.selection_layers.iter().map(|layer| {
                    let mut command = json!({"op":"paint.fill","layer":layer,"color":color,"mask":gesture.mask,"step":gesture.step});
                    if let Some(words) = color16 {
                        command["color16"] = json!(words);
                    }
                    command
                }).collect();
                self.apply_action(commands, "Fill layer", Some((doc.id(), doc.revision())),
                    doc.native().map_or(edit_job::Origin::Action, |source| {
                        edit_job::Origin::CanvasGesture(source.key())
                    }));
            }
        } else if ui.input(|input| input.pointer.primary_down()) {
            self.fill_press = Some(gesture);
        }
    }

    pub(super) fn primitive_canvas(
        &mut self,
        ui: &egui::Ui,
        doc: DocumentView<'_>,
        response: &egui::Response,
        painter: &egui::Painter,
        projection: &view::Projection,
        mut draft: Option<Drag>,
    ) {
        let cancelled = Self::pointer_contact_cancelled(ui.ctx())
            || ui.input(|input| {
                !input.focused
                    || input.key_pressed(egui::Key::Escape)
                    || input.key_down(egui::Key::Space)
                    || input
                        .events
                        .iter()
                        .any(|event| matches!(event, egui::Event::WindowFocused(false)))
            });
        if cancelled || doc.read_only() || self.busy {
            if draft.is_some_and(|drag| drag.previewing) {
                self.invalidate_primitive_preview();
            }
            return;
        }
        if draft.as_ref().is_some_and(|drag| !drag.observes(self, doc)) {
            if draft.is_some_and(|drag| drag.previewing) {
                self.invalidate_primitive_preview();
            }
            self.message = "Drawing source changed · start a new drag".into();
            return;
        }
        let held = draft.is_some();
        let (press, release) = ui.input(|input| canvas_button_samples(input, held));
        if let Some(pos) = press.filter(|&pos| canvas_press_hits(ui, response, pos)) {
            let source = match self.canvas_gesture_source(doc) {
                Ok(source) => source,
                Err(error) => {
                    self.message = error.into();
                    return;
                }
            };
            draft = Some(Drag {
                source,
                screen: pos,
                start: projection.document(pos),
                tool: self.tool,
                mask: self.mask,
                step: self.mask_step.clone(),
                color: self.stroke_color(),
                color16: self.foreground_words(doc.depth()),
                direction: self.gradient_direction,
                active: false,
                previewing: false,
                id: crate::engine::id(),
            });
        }
        let Some(mut drag) = draft else { return };
        let pos = release.map(|(pos, _)| pos).or_else(|| {
            ui.input(|input| {
                input
                    .pointer
                    .primary_down()
                    .then(|| input.pointer.interact_pos())
                    .flatten()
            })
        });
        let Some(pos) = pos else {
            if drag.previewing {
                self.invalidate_primitive_preview();
            }
            return;
        };
        if !drag.active {
            let slop = ui
                .ctx()
                .options(|options| options.input_options.max_click_dist);
            drag.active = response.dragged_by(egui::PointerButton::Primary)
                || drag.screen.distance(pos) > slop
                || release.is_some()
                    && ui.input(|input| {
                        selection::motion_before_release(input, held)
                            .any(|p| drag.screen.distance(p) > slop)
                    });
        }
        let [x, y] = drag.start;
        let [end_x, end_y] = projection.document(pos);
        let area = [
            x.min(end_x) as i32,
            y.min(end_y) as i32,
            x.max(end_x) as i32,
            y.max(end_y) as i32,
        ];
        let reaches = |bounds: [i32; 4]| (0..2).all(|axis| {
            let [low, high] = doc.selection().map_or([area[axis], area[axis + 2]], |selection| {
                [area[axis].max(selection[axis]), area[axis + 2].min(selection[axis + 2])]
            });
            low.max(bounds[axis]) < high.min(bounds[axis + 2])
        });
        let has_area = drag.active && area[0] < area[2] && area[1] < area[3];
        let has_footprint = has_area && (reaches([0, 0, doc.width() as i32, doc.height() as i32])
            || doc.find_layer(&self.selected).is_some_and(|layer| {
                let [x, y] = layer.origin();
                let size = layer.pixels();
                reaches([x, y, x + size.width as i32, y + size.height as i32])
            }));
        if drag.previewing && (!has_footprint || release.is_some()) {
            self.invalidate_primitive_preview();
        }
        drag.previewing = has_footprint && release.is_none();
        if has_footprint {
            let gradient = drag.tool == Tool::Gradient;
            let mut command = json!({
                "op": if gradient { "gradient" } else { "shape" },
                "rect": area,
                "layer": self.selected,
                "color": drag.color,
                "mask": drag.mask,
                "step": drag.step,
                "document_id": doc.id(),
                "source_revision": doc.revision(),
            });
            if let Some(words) = drag.color16 {
                command["color16"] = json!(words);
            }
            if gradient {
                if drag.direction != GradientDirection::Right {
                    command["direction"] = json!(drag.direction.name());
                }
            } else {
                command["kind"] = json!(if drag.tool == Tool::Ellipse {
                    "ellipse"
                } else {
                    "rectangle"
                });
            }
            if release.is_some() {
                if !self.selection_layers.contains(&self.selected) {
                    self.selection_layers = [self.selected.clone()].into_iter().collect();
                }
                self.apply_action(
                    vec![command],
                    if gradient {
                        "Draw gradient"
                    } else {
                        "Draw shape"
                    },
                    Some((doc.id(), doc.revision())),
                    doc.native().map_or(edit_job::Origin::Action, |source| {
                        edit_job::Origin::CanvasGesture(source.key())
                    }),
                );
                self.last_preview = None;
            } else {
                self.transient.push(command);
            }
        }
        if release.is_none() {
            if has_area {
                let outline = if drag.tool == Tool::Ellipse {
                    let [left, top, right, bottom] = area.map(|side| side as f32);
                    let [rx, ry] = [(right - left) * 0.5, (bottom - top) * 0.5];
                    (0..64).map(|i| {
                        let (sin, cos) = (i as f32 * (std::f32::consts::TAU / 64.)).sin_cos();
                        projection.point([left + rx * (1. + cos), top + ry * (1. + sin)])
                    }).collect()
                } else {
                    projection.region_corners(area).to_vec()
                };
                painter.add(egui::Shape::closed_line(outline, Stroke::new(1., ACCENT)));
            }
            self.primitive_drag = Some(drag);
        }
    }
}
