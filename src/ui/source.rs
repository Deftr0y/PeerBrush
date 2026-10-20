use super::*;
use crate::source::{Content, Source};
pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
    pub layer: Option<String>,
    pub parent: Option<String>,
    pub source: Source,
    pub x: i32,
    pub y: i32,
    pub error: Option<String>,
    pub placement: Option<Placement>,
    apply_error: Option<String>,
    font_search: String,
}
pub(super) struct Placement {
    anchor: [f32; 2],
    dragged: bool,
}
impl Editor {
    pub fn command(&self) -> Value {
        json!({"op":if self.layer.is_some(){"source.update"}else{"source.add"},"layer":self.layer,"parent":self.parent,"source":self.source,"x":self.x,"y":self.y,"document_id":self.document,"source_revision":self.revision})
    }
}
fn native_color(ui: &mut egui::Ui, label: &str, color: &mut [u16; 4], depth: u16) {
    ui.horizontal(|ui| {
        ui.label(label);
        for (name, word) in ["R", "G", "B", "A"].iter().zip(color.iter_mut()) {
            ui.label(*name);
            if depth == 16 {
                controls::numeric(ui, word, 0..=65535);
            } else {
                let mut value = crate::raster::project16(*word);
                if controls::numeric(ui, &mut value, 0..=255).changed() {
                    *word = u16::from(value) * 257;
                }
            }
        }
    });
}
impl PeerBrush {
    pub(super) fn open_source(&mut self, doc: &Document, kind: &str) {
        self.cancel_filters();
        self.cancel_geometry();
        self.cancel_refinement();
        self.cancel_source();
        self.points.clear();
        self.drag_start = None;
        self.gizmo_handle = 0;
        self.animation.cancel();
        self.isolate = false;
        self.mask = false;
        let selected = doc.layers.iter().find(|l| l.id == self.selected);
        let (layer, source, x, y, parent) = if kind == "edit" {
            let Some(layer) = selected.filter(|l| l.source.is_some()) else {
                return;
            };
            (
                Some(layer.id.clone()),
                layer.source.clone().unwrap(),
                layer.x,
                layer.y,
                layer.parent.clone(),
            )
        } else {
            let w = doc
                .width
                .min(if kind == "text" { 400 } else { 256 })
                .max(32);
            let h = doc
                .height
                .min(if kind == "text" { 160 } else { 256 })
                .max(32);
            let color = self.color.map(|v| u16::from(v) * 257);
            let content = if kind == "text" {
                Content::Text {
                    text: "Text".into(),
                    font: "regular".into(),
                    size: 48.,
                    line_height: 1.2,
                    letter_spacing: 0.,
                    align: "left".into(),
                    color,
                }
            } else {
                Content::Shape {
                    shape: "rectangle".into(),
                    bounds: [8., 8., w as f32 - 8., h as f32 - 8.],
                    points: vec![
                        [8., h as f32 - 8.],
                        [w as f32 * 0.5, 8.],
                        [w as f32 - 8., h as f32 - 8.],
                    ],
                    closed: true,
                    fill: color,
                    stroke: [65535; 4],
                    stroke_width: 0.,
                }
            };
            (
                None,
                Source {
                    width: w,
                    height: h,
                    matrix: [1., 0., 0., 1., 0., 0.],
                    content,
                },
                (doc.width as i32 - w as i32) / 2,
                (doc.height as i32 - h as i32) / 2,
                selected.and_then(|l| {
                    if l.kind == "group" {
                        Some(l.id.clone())
                    } else {
                        l.parent.clone()
                    }
                }),
            )
        };
        self.source_editor = Some(Editor {
            document: doc.id.clone(),
            revision: doc.revision,
            layer,
            parent,
            source,
            x,
            y,
            error: None,
            placement: None,
            apply_error: None,
            font_search: String::new(),
        });
        self.last_preview = None;
    }
    pub(super) fn cancel_source(&mut self) {
        self.source_editor = None;
        self.transient
            .retain(|c| !c["op"].as_str().is_some_and(|op| op.starts_with("source.")));
        self.last_preview = None;
    }
    pub(super) fn text_field_outline(&self, painter: &egui::Painter, rect: Rect, scale: f32) {
        let Some(editor) = self
            .source_editor
            .as_ref()
            .filter(|editor| matches!(editor.source.content, Content::Text { .. }))
        else {
            return;
        };
        let [a, b, c, d, tx, ty] = editor.source.matrix;
        let w = editor.source.width as f32;
        let h = editor.source.height as f32;
        let points = [[0., 0.], [w, 0.], [w, h], [0., h], [0., 0.]].map(|[x, y]| {
            self.view.point(
                [
                    editor.x as f32 + a * x + c * y + tx,
                    editor.y as f32 + b * x + d * y + ty,
                ],
                rect,
                scale,
            )
        });
        painter.add(egui::Shape::line(points.to_vec(), Stroke::new(1., ACCENT)));
    }
    pub(super) fn text_canvas(
        &mut self,
        ui: &mut egui::Ui,
        doc: &Document,
        response: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
        scale: f32,
    ) {
        let cancelled = ui.input(|input| {
            !input.focused
                || input.key_pressed(egui::Key::Escape)
                || input
                    .events
                    .iter()
                    .any(|event| matches!(event, egui::Event::WindowFocused(false)))
        });
        let stale = self
            .source_editor
            .as_ref()
            .is_some_and(|editor| editor.document != doc.id || editor.revision != doc.revision);
        if cancelled || stale || doc.read_only || self.busy {
            if self
                .source_editor
                .as_ref()
                .is_some_and(|editor| editor.placement.is_some())
            {
                self.cancel_source();
            }
            return;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let mut first_event = 0;
        if self.source_editor.is_none() {
            let press = ui.input(|input| {
                input
                    .events
                    .iter()
                    .enumerate()
                    .find_map(|(index, event)| match event {
                        egui::Event::PointerButton {
                            pos,
                            button: egui::PointerButton::Primary,
                            pressed: true,
                            ..
                        } => Some((index, *pos)),
                        _ => None,
                    })
            });
            let Some((index, press)) = press.filter(|(_, point)| {
                response.rect.contains(*point)
                    && self.view.contains(*point, rect, scale)
                    && ui.ctx().layer_id_at(*point) == Some(ui.layer_id())
            }) else {
                return;
            };
            first_event = index;
            let anchor = self.view.document(press, rect, scale);
            if let Some(id) = text_hit(doc, anchor) {
                if let Err(error) = crate::placement::unlocked(doc, &id) {
                    self.message = error;
                    return;
                }
                self.select_content(&id);
                self.open_source(doc, "edit");
                return;
            }
            self.open_source(doc, "text");
            let editor = self.source_editor.as_mut().unwrap();
            editor.x = anchor[0].floor() as i32;
            editor.y = anchor[1].floor() as i32;
            editor.placement = Some(Placement {
                anchor,
                dragged: false,
            });
        }
        let positions = ui.input(|input| {
            input
                .events
                .iter()
                .skip(first_event)
                .filter_map(|event| match event {
                    egui::Event::PointerMoved(pos) => Some((*pos, false)),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        ..
                    } => Some((*pos, true)),
                    _ => None,
                })
                .collect::<Vec<_>>()
        });
        let editor = self.source_editor.as_mut().unwrap();
        for (pointer, released) in positions {
            let Some(placement) = &mut editor.placement else {
                break;
            };
            let anchor = placement.anchor;
            let point = self.view.document(pointer, rect, scale);
            placement.dragged |= (point[0] - anchor[0])
                .abs()
                .max((point[1] - anchor[1]).abs())
                * scale
                > 3.;
            if placement.dragged {
                let left = point[0].min(anchor[0]).floor();
                let top = point[1].min(anchor[1]).floor();
                editor.x = left as i32;
                editor.y = top as i32;
                editor.source.width = ((point[0].max(anchor[0]).ceil() - left) as u32)
                    .clamp(1, crate::raster::MAX_EDGE);
                editor.source.height = ((point[1].max(anchor[1]).ceil() - top) as u32)
                    .clamp(1, crate::raster::MAX_EDGE);
            }
            if released {
                editor.placement = None;
                break;
            }
        }
        if !ui.input(|input| input.pointer.primary_down()) {
            editor.placement = None;
        }
        self.transient = vec![editor.command()];
        self.text_field_outline(painter, rect, scale);
        ui.ctx().request_repaint();
    }
    pub(super) fn source_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(mut e) = self.source_editor.take() else {
            return;
        };
        if e.document != doc.id || e.revision != doc.revision {
            self.cancel_source();
            return;
        }
        if e.placement.is_some() {
            self.source_editor = Some(e);
            return;
        }
        let before = e.command();
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut valid = false;
        egui::Window::new(e.source.label())
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(410.)
            .show(ctx, |ui| {
                if e.layer.is_none() {
                    ui.horizontal(|ui| {
                        ui.label("X");
                        controls::numeric(ui, &mut e.x, -100000..=100000);
                        ui.label("Y");
                        controls::numeric(ui, &mut e.y, -100000..=100000);
                        ui.label("px");
                    });
                }
                if matches!(e.source.content, Content::Text { .. }) {
                    ui.horizontal(|ui| {
                        ui.label("Field width");
                        controls::numeric(ui, &mut e.source.width, 1..=crate::raster::MAX_EDGE);
                        ui.label("Height");
                        controls::numeric(ui, &mut e.source.height, 1..=crate::raster::MAX_EDGE);
                        ui.label("px");
                    });
                }
                match &mut e.source.content {
                    Content::Bezier {
                        nodes,
                        closed,
                        fill,
                        stroke,
                        stroke_width,
                    } => {
                        ui.label(format!(
                            "{} anchors · edit points and handles with Pen",
                            nodes.len()
                        ));
                        ui.add_enabled(
                            nodes.len() >= 2,
                            egui::Checkbox::new(closed, "Closed path"),
                        );
                        native_color(ui, "Fill", fill, doc.bit_depth);
                        native_color(ui, "Stroke", stroke, doc.bit_depth);
                        ui.horizontal(|ui| {
                            ui.label("Stroke width");
                            controls::numeric(ui, stroke_width, 0. ..=256.);
                            ui.label("px");
                        });
                    }
                    Content::Svg { svg } => {
                        ui.label("SVG source");
                        egui::ScrollArea::vertical()
                            .id_salt("svg source")
                            .max_height(180.)
                            .show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(svg)
                                        .code_editor()
                                        .desired_rows(8)
                                        .desired_width(390.)
                                        .char_limit(crate::svg::MARKUP_LIMIT)
                                        .frame(false),
                                );
                            });
                    }
                    Content::Text {
                        text,
                        font,
                        size,
                        line_height,
                        letter_spacing,
                        align,
                        color,
                    } => {
                        ui.add(
                            egui::TextEdit::multiline(text)
                                .desired_rows(3)
                                .desired_width(390.)
                                .char_limit(4096)
                                .frame(false),
                        );
                        ui.horizontal(|ui| {
                            font_picker(ui, font, &mut e.font_search);
                            ui.label("Size");
                            controls::numeric(ui, size, 1. ..=1024.);
                            ui.label("px");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Letter spacing");
                            controls::numeric(ui, letter_spacing, -1024. ..=1024.);
                            ui.label("px");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Line spacing");
                            controls::numeric(ui, line_height, 0.5..=4.);
                            for name in ["left", "center", "right"] {
                                ui.selectable_value(align, name.into(), name);
                            }
                        });
                        native_color(ui, "Color", color, doc.bit_depth);
                    }
                    Content::Shape {
                        shape,
                        bounds,
                        points,
                        closed,
                        fill,
                        stroke,
                        stroke_width,
                    } => {
                        ui.horizontal(|ui| {
                            for name in ["rectangle", "ellipse", "path"] {
                                ui.selectable_value(shape, name.into(), name);
                            }
                        });
                        if shape == "path" {
                            ui.checkbox(closed, "Closed path");
                            egui::ScrollArea::vertical()
                                .max_height(180.)
                                .show(ui, |ui| {
                                    let count = points.len();
                                    let mut remove = None;
                                    for (i, p) in points.iter_mut().enumerate() {
                                        ui.horizontal(|ui| {
                                            ui.label(format!("{}", i + 1));
                                            ui.label("X");
                                            controls::numeric(ui, &mut p[0], -100000. ..=100000.);
                                            ui.label("Y");
                                            controls::numeric(ui, &mut p[1], -100000. ..=100000.);
                                            if ui
                                                .add_enabled(count > 2, egui::Button::new("−"))
                                                .clicked()
                                            {
                                                remove = Some(i);
                                            }
                                        });
                                    }
                                    if let Some(i) = remove {
                                        points.remove(i);
                                    }
                                });
                            if ui
                                .add_enabled(points.len() < 128, egui::Button::new("Add point"))
                                .clicked()
                            {
                                let p = points.last().copied().unwrap_or([0., 0.]);
                                points.push([p[0] + 12., p[1] + 12.]);
                            }
                        } else {
                            for (labels, values) in [["Left", "Top"], ["Right", "Bottom"]]
                                .iter()
                                .zip(bounds.chunks_mut(2))
                            {
                                ui.horizontal(|ui| {
                                    for (name, value) in labels.iter().zip(values) {
                                        ui.label(*name);
                                        controls::numeric(ui, value, -100000. ..=100000.);
                                    }
                                });
                            }
                        }
                        native_color(ui, "Fill", fill, doc.bit_depth);
                        native_color(ui, "Stroke", stroke, doc.bit_depth);
                        ui.horizontal(|ui| {
                            ui.label("Stroke width");
                            controls::numeric(ui, stroke_width, 0. ..=256.);
                            ui.label("px");
                        });
                    }
                }
                let invalid = e.source.validate().err();
                valid = invalid.is_none();
                let pending = |error: &String| {
                    [
                        crate::fonts::FontError::Loading,
                        crate::fonts::FontError::Busy,
                    ]
                    .iter()
                    .any(|pending| error == &pending.to_string())
                };
                if invalid.as_ref().is_some_and(pending) {
                    ctx.request_repaint_after(Duration::from_millis(50));
                }
                if valid {
                    if e.error.as_ref().is_some_and(pending) {
                        e.error = None;
                        self.last_preview = None;
                    }
                    if e.apply_error.as_ref().is_some_and(pending) {
                        e.apply_error = None;
                    }
                }
                if let Some(error) = invalid
                    .as_ref()
                    .or(e.error.as_ref())
                    .or(e.apply_error.as_ref())
                {
                    ui.colored_label(ACCENT, error);
                }
                ui.horizontal(|ui| {
                    apply = ui
                        .add_enabled(
                            !self.busy && !doc.read_only && invalid.is_none() && e.error.is_none(),
                            egui::Button::new("Apply"),
                        )
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        let command = e.command();
        if command != before {
            e.error = None;
            e.apply_error = None;
        }
        if apply {
            if self.try_edit(vec![command.clone()], "Edit text/vector source") {
                self.cancel_source();
            } else {
                e.apply_error = Some(self.message.clone());
                self.transient = vec![command];
                self.source_editor = Some(e);
            }
        } else if !open || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_source();
        } else {
            if valid {
                self.transient = vec![command];
            } else {
                // Keep the saved raster when a font cannot reproduce this draft.
                self.transient.retain(|command| {
                    !command["op"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("source."))
                });
            }
            self.source_editor = Some(e);
        }
    }
}

fn text_hit(doc: &Document, point: [f32; 2]) -> Option<String> {
    crate::compositor::effect_order(doc)
        .into_iter()
        .rev()
        .find_map(|index| {
            let layer = &doc.layers[index];
            let source = layer.source.as_ref()?;
            if !matches!(source.content, Content::Text { .. }) {
                return None;
            }
            let mut current = Some(layer.id.as_str());
            for _ in 0..=16 {
                let Some(id) = current else { break };
                let ancestor = doc.layers.iter().find(|candidate| candidate.id == id)?;
                if !ancestor.visible {
                    return None;
                }
                current = ancestor.parent.as_deref();
            }
            if current.is_some() {
                return None;
            }
            let [a, b, c, d, tx, ty] = source.matrix.map(f64::from);
            let det = a * d - b * c;
            if !det.is_finite() || det.abs() < 0.000001 {
                return None;
            }
            let x = f64::from(point[0]) - f64::from(layer.x) - tx;
            let y = f64::from(point[1]) - f64::from(layer.y) - ty;
            let local = [(d * x - c * y) / det, (-b * x + a * y) / det];
            (local[0] >= 0.
                && local[1] >= 0.
                && local[0] < f64::from(source.width)
                && local[1] < f64::from(source.height))
            .then(|| layer.id.clone())
        })
}

fn font_picker(ui: &mut egui::Ui, font: &mut crate::fonts::TextFont, search: &mut String) {
    use crate::fonts::{self, CatalogStatus, TextFont};
    let label = font.label().into_owned();
    let short: String = label.chars().take(44).collect();
    egui::ComboBox::from_id_salt("text font")
        .selected_text(if short.len() < label.len() {
            format!("{short}…")
        } else {
            short
        })
        .width(220.)
        .show_ui(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            for name in ["regular", "medium", "semibold"] {
                ui.selectable_value(font, name.into(), format!("Ubuntu Sans {name}"));
            }
            ui.add(
                egui::TextEdit::singleline(search)
                    .hint_text("Search installed fonts")
                    .char_limit(128)
                    .frame(false),
            );
            if ui.button("Refresh installed fonts").clicked() {
                if let Err(error) = fonts::refresh() {
                    ui.colored_label(ACCENT, error.to_string());
                }
            }
            let catalog = fonts::catalog();
            match catalog.status {
                CatalogStatus::Loading => {
                    ui.label("Reading installed fonts…");
                    ui.ctx().request_repaint_after(Duration::from_millis(50));
                }
                CatalogStatus::Unavailable(error) => {
                    ui.colored_label(ACCENT, error.to_string());
                }
                CatalogStatus::Ready => {}
            }
            let search = search.to_lowercase();
            let faces: Vec<_> = catalog
                .faces
                .iter()
                .filter(|face| {
                    search.is_empty()
                        || face.family.to_lowercase().contains(&search)
                        || face.postscript.to_lowercase().contains(&search)
                })
                .collect();
            if faces.is_empty() && matches!(catalog.status, CatalogStatus::Ready) {
                ui.label("No matching installed fonts");
            }
            let height = ui
                .spacing()
                .interact_size
                .y
                .max(ui.text_style_height(&egui::TextStyle::Button));
            egui::ScrollArea::vertical()
                .id_salt("installed font rows")
                .max_height(220.)
                .show_rows(ui, height, faces.len(), |ui, range| {
                    for index in range {
                        let face = faces[index];
                        if let Some(value) = &face.font {
                            let candidate = TextFont::Installed(value.clone());
                            let label = candidate.label().into_owned();
                            ui.selectable_value(font, candidate, label);
                        } else {
                            ui.add_enabled(
                                false,
                                egui::Button::new(format!("{} · unavailable", face.family)),
                            )
                            .on_disabled_hover_text(
                                face.error
                                    .unwrap_or(fonts::FontError::Unsupported)
                                    .to_string(),
                            );
                        }
                    }
                });
        })
        .response
        .on_hover_text(label);
}
