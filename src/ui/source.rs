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
        });
        self.last_preview = None;
    }
    pub(super) fn cancel_source(&mut self) {
        self.source_editor = None;
        self.transient
            .retain(|c| !c["op"].as_str().is_some_and(|op| op.starts_with("source.")));
        self.last_preview = None;
    }
    pub(super) fn source_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(mut e) = self.source_editor.take() else {
            return;
        };
        if e.document != doc.id || e.revision != doc.revision {
            self.cancel_source();
            return;
        }
        let before = e.command();
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
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
                match &mut e.source.content {
                    Content::Text {
                        text,
                        font,
                        size,
                        line_height,
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
                            egui::ComboBox::from_id_salt("text font")
                                .selected_text(format!("Ubuntu Sans {font}"))
                                .show_ui(ui, |ui| {
                                    for name in ["regular", "medium", "semibold"] {
                                        ui.selectable_value(font, name.into(), name);
                                    }
                                });
                            ui.label("Size");
                            controls::numeric(ui, size, 1. ..=1024.);
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
                if let Some(error) = invalid.as_ref().or(e.error.as_ref()) {
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
        }
        if apply {
            let creating = e.layer.is_none();
            self.cancel_source();
            self.edit(vec![command], "Edit text/vector source");
            if creating {
                let layer = self
                    .shared
                    .lock()
                    .unwrap()
                    .doc
                    .layers
                    .first()
                    .map(|l| l.id.clone());
                if let Some(id) = layer {
                    self.select_content(&id);
                }
            }
        } else if !open || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_source();
        } else {
            self.transient = vec![command];
            self.source_editor = Some(e);
        }
    }
}
