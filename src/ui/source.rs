use super::*;
use crate::engine::view::{DocumentView, SourceView};
use crate::psd::memory36::Admitted;
use crate::source::{Content, Source};
use std::sync::atomic::{AtomicU64, Ordering};
#[path = "source/buffer.rs"]
mod buffer;
#[path = "source/native.rs"]
mod native;
#[path = "source/text_state.rs"]
mod text_state;
#[path = "source/widgets.rs"]
mod widgets;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct EditToken(u64);
impl EditToken {
    pub(super) fn nonce(self) -> u64 {
        self.0
    }
}
pub(super) enum EditCommand {
    Resident(Value),
    Native(Admitted<Value>),
}
static NEXT_EDITOR: AtomicU64 = AtomicU64::new(1);
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
    validation: Option<Validation>,
    token: EditToken,
    pending: bool,
    // Actual Source/String/validation payloads drop before their admission.
    native: Option<native::Native>,
    #[cfg(test)]
    pub(super) validation_runs: usize,
}
struct Validation {
    source: Source,
    font: ab_glyph::FontArc,
    result: Result<(), String>,
}
pub(super) struct Placement {
    anchor: [f32; 2],
    dragged: bool,
}
impl Editor {
    fn invalid(&mut self) -> Option<String> {
        // Native validation belongs to the captured worker and its actual
        // FontOwner/SVG preparation. Never consult the legacy global cache.
        if let Some(native) = &self.native {
            return native.error.clone();
        }
        use ab_glyph::Font;
        let Content::Text {
            layout: crate::source::TextLayout::Shaped,
            font,
            ..
        } = &self.source.content
        else {
            self.validation = None;
            return self.source.validate().err();
        };
        let font = match crate::fonts::snapshot(font) {
            Ok(font) => font,
            Err(error) => {
                self.validation = None;
                return Some(error.to_string());
            }
        };
        if let Some(validation) = &self.validation {
            if validation.source == self.source
                && std::ptr::eq(validation.font.font_data(), font.font_data())
            {
                return validation.result.clone().err();
            }
        }
        let result = self.source.validate_shaped_snapshot(&font);
        #[cfg(test)]
        {
            self.validation_runs += 1;
        }
        self.validation = Some(Validation {
            source: self.source.clone(),
            font,
            result: result.clone(),
        });
        result.err()
    }
    /// Resident compatibility only. Native transports use owned_command so a
    /// raw Value cannot outlive the allocation that admitted its copy.
    pub fn command(&self) -> Value {
        json!({"op":if self.layer.is_some(){"source.update"}else{"source.add"},"layer":self.layer,"parent":self.parent,"source":self.source,"x":self.x,"y":self.y,"document_id":self.document,"source_revision":self.revision})
    }
    pub(super) fn token(&self) -> EditToken {
        self.token
    }
    pub(super) fn is_native(&self) -> bool {
        self.native.is_some()
    }
    pub(super) fn owned_command(&self) -> Option<Admitted<Value>> {
        if self.error.is_some() || self.native.as_ref()?.error.is_some() {
            return None;
        }
        self.native.as_ref()?.owned_command(self.signature().ok()?)
    }
    fn signature(&self) -> Result<[u8; 32], String> {
        struct Hash(ring::digest::Context);
        impl std::io::Write for Hash {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut hash = Hash(ring::digest::Context::new(&ring::digest::SHA256));
        serde_json::to_writer(
            &mut hash,
            &(&self.source, self.x, self.y, &self.layer, &self.parent),
        )
        .map_err(|_| "Source draft fingerprint refused")?;
        Ok(hash
            .0
            .finish()
            .as_ref()
            .try_into()
            .map_err(|_| "Source draft fingerprint size")?)
    }
    fn edit_command(&mut self, doc: DocumentView<'_>) -> Result<EditCommand, String> {
        let signature = self.signature()?;
        if self.native.is_some() {
            let doc = doc.native().ok_or("Source editor authority changed")?;
            let Self {
                source,
                x,
                y,
                layer,
                parent,
                document,
                revision,
                native,
                ..
            } = self;
            let native = native.as_mut().unwrap();
            let value=native.command(doc,signature,||json!({"op":if layer.is_some(){"source.update"}else{"source.add"},"layer":layer,"parent":parent,"source":source,"x":x,"y":y,"document_id":document,"source_revision":revision}))?;
            Ok(EditCommand::Native(value))
        } else {
            Ok(EditCommand::Resident(self.command()))
        }
    }
}
fn native_color(
    ui: &mut egui::Ui,
    label: &str,
    color: &mut [u16; 4],
    depth: u16,
    mut native: Option<&mut native::Native>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        for (name, word) in ["R", "G", "B", "A"].iter().zip(color.iter_mut()) {
            ui.label(*name);
            if depth == 16 {
                widgets::numeric(ui, word, 0..=65535, native.as_deref_mut());
            } else {
                let mut value = crate::raster::project16(*word);
                if widgets::numeric(ui, &mut value, 0..=255, native.as_deref_mut()).changed() {
                    *word = u16::from(value) * 257;
                }
            }
        }
    });
}
impl PeerBrush {
    pub(super) fn open_source<'a>(&mut self, doc: impl Into<DocumentView<'a>>, kind: &str) {
        if self
            .source_editor
            .as_ref()
            .is_some_and(|editor| editor.pending)
        {
            self.message = "Wait for the pending source edit before opening another draft".into();
            return;
        }
        let doc = doc.into();
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
        let selected = doc.layers().find(|l| l.id() == self.selected);
        let mut native = match doc
            .native()
            .map(|doc| {
                native::Native::new(
                    doc,
                    selected.and_then(|layer| match layer.source() {
                        Some(SourceView::Native(source)) if kind == "edit" => Some(source),
                        _ => None,
                    }),
                )
            })
            .transpose()
        {
            Ok(value) => value,
            Err(error) => {
                self.message = error.into();
                return;
            }
        };
        if native.is_some()
            && [
                doc.id(),
                selected.map_or("", |layer| layer.id()),
                selected.and_then(|layer| layer.parent()).unwrap_or(""),
            ]
            .iter()
            .try_fold(0usize, |sum, value| sum.checked_add(value.len()))
            .is_none_or(|bytes| bytes > 512 * 1024)
        {
            self.message = "Source editor labels exceed their admitted resident capacity".into();
            return;
        }
        let (layer, source, x, y, parent) = if kind == "edit" {
            let Some(layer) = selected.filter(|l| l.source().is_some()) else {
                return;
            };
            (
                Some(layer.id().to_owned()),
                match layer.source().unwrap() {
                    SourceView::Resident(source) => source.clone(),
                    SourceView::Native(_) => match native.as_mut().unwrap().original_draft() {
                        Ok(Some(source)) => source,
                        Ok(None) => return,
                        Err(error) => {
                            self.message = error.into();
                            return;
                        }
                    },
                },
                layer.origin()[0],
                layer.origin()[1],
                layer.parent().map(str::to_owned),
            )
        } else {
            let w = doc
                .width()
                .min(if kind == "text" { 400 } else { 256 })
                .max(32);
            let h = doc
                .height()
                .min(if kind == "text" { 160 } else { 256 })
                .max(32);
            let color = self.color.map(|v| u16::from(v) * 257);
            let content = if kind == "text" {
                Content::Text {
                    text: "Text".into(),
                    layout: crate::source::TextLayout::Legacy,
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
                (doc.width() as i32 - w as i32) / 2,
                (doc.height() as i32 - h as i32) / 2,
                selected.and_then(|l| {
                    if l.kind() == "group" {
                        Some(l.id().to_owned())
                    } else {
                        l.parent().map(str::to_owned)
                    }
                }),
            )
        };
        let token = match NEXT_EDITOR
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        {
            Ok(value) => EditToken(value),
            Err(_) => {
                self.message = "Source editor nonce exhausted".into();
                return;
            }
        };
        let mut source = source;
        let mut font_search = String::new();
        if native.is_some() {
            if let Err(error) = native::prepare_buffers(&mut source, &mut font_search) {
                self.message = error.into();
                return;
            }
        }
        self.source_editor = Some(Editor {
            document: doc.id().to_owned(),
            revision: doc.revision(),
            layer,
            parent,
            source,
            x,
            y,
            error: None,
            placement: None,
            apply_error: None,
            font_search,
            validation: None,
            token,
            pending: false,
            native,
            #[cfg(test)]
            validation_runs: 0,
        });
        self.last_preview = None;
    }
    pub(super) fn cancel_source(&mut self) {
        if self
            .source_editor
            .as_ref()
            .is_some_and(|editor| editor.pending)
        {
            return;
        }
        self.transient
            .retain(|c| !c["op"].as_str().is_some_and(|op| op.starts_with("source.")));
        self.source_editor = None;
        self.last_preview = None;
    }
    pub(super) fn source_edit_finished(&mut self, token: EditToken, result: Result<(), String>) {
        if !self
            .source_editor
            .as_ref()
            .is_some_and(|editor| editor.token == token)
        {
            return;
        }
        self.source_editor.as_mut().unwrap().pending = false;
        match result {
            Ok(()) => self.cancel_source(),
            Err(error) => {
                let editor = self.source_editor.as_mut().unwrap();
                editor.pending = false;
                editor.apply_error = Some(error);
            }
        }
    }
    pub(super) fn source_edit_finished_nonce(&mut self, nonce: u64, result: Result<(), String>) {
        self.source_edit_finished(EditToken(nonce), result);
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
    pub(super) fn text_canvas<'a>(
        &mut self,
        ui: &mut egui::Ui,
        doc: impl Into<DocumentView<'a>>,
        response: &egui::Response,
        painter: &egui::Painter,
        rect: Rect,
        scale: f32,
    ) {
        let doc = doc.into();
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
            .is_some_and(|editor| editor.document != doc.id() || editor.revision != doc.revision());
        if cancelled || stale || doc.read_only() || self.busy {
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
                if let Err(error) = unlocked(doc, &id) {
                    self.message = error.into();
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
        match editor.edit_command(doc) {
            Ok(EditCommand::Resident(command)) => self.transient = vec![command],
            Ok(EditCommand::Native(_)) => {}
            Err(error) => editor.error = Some(error),
        }
        self.text_field_outline(painter, rect, scale);
        ui.ctx().request_repaint();
    }
    pub(super) fn source_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        let Some(mut e) = self.source_editor.take() else {
            return;
        };
        let stale = e.document != doc.id()
            || e.revision != doc.revision()
            || e.native
                .as_ref()
                .is_some_and(|native| doc.native().is_none_or(|doc| native.key != doc.key()));
        if stale && !e.pending {
            e.error=Some("Image changed. This draft is retained; copy it or cancel and reopen the source before applying.".into());
            e.placement = None;
        }
        if e.placement.is_some() {
            self.source_editor = Some(e);
            return;
        }
        if let Some(native) = &mut e.native {
            native.error = None;
            if native.widgets.is_none() {
                match widgets::Widgets::new(ctx, native.pool()) {
                    Ok(widgets) => native.widgets = Some(widgets),
                    Err(error) => native.error = Some(error),
                }
            }
        }
        let before = e.signature().ok();
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let mut valid = false;
        egui::Window::new(e.source.label())
            .id(egui::Id::new(("source-editor",e.token.nonce())))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(410.)
            .show(ctx, |ui| {
                ui.set_enabled(!e.pending);
                if e.layer.is_none() {
                    ui.horizontal(|ui| {
                        ui.label("X");
                        widgets::numeric(ui, &mut e.x, -100000..=100000,e.native.as_mut());
                        ui.label("Y");
                        widgets::numeric(ui, &mut e.y, -100000..=100000,e.native.as_mut());
                        ui.label("px");
                    });
                }
                if matches!(e.source.content, Content::Text { .. }) {
                    ui.horizontal(|ui| {
                        ui.label("Field width");
                        widgets::numeric(ui, &mut e.source.width, 1..=crate::raster::MAX_EDGE,e.native.as_mut());
                        ui.label("Height");
                        widgets::numeric(ui, &mut e.source.height, 1..=crate::raster::MAX_EDGE,e.native.as_mut());
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
                        native_color(ui, "Fill", fill, doc.depth(),e.native.as_mut());
                        native_color(ui, "Stroke", stroke, doc.depth(),e.native.as_mut());
                        ui.horizontal(|ui| {
                            ui.label("Stroke width");
                            widgets::numeric(ui, stroke_width, 0. ..=256.,e.native.as_mut());
                            ui.label("px");
                        });
                    }
                    Content::Svg { svg } => {
                        ui.label("SVG source");
                        egui::ScrollArea::vertical()
                            .id_salt("svg source")
                            .max_height(180.)
                            .show(ui, |ui| {
                                widgets::text(ui,svg,crate::svg::MARKUP_LIMIT,true,false,e.native.as_mut());
                            });
                    }
                    Content::Text {
                        text,
                        layout,
                        font,
                        size,
                        line_height,
                        letter_spacing,
                        align,
                        color,
                    } => {
                        widgets::text(ui,text,16384,false,false,e.native.as_mut());
                        let mut shaped = *layout == crate::source::TextLayout::Shaped;
                        if ui.checkbox(&mut shaped, "Ligatures and joining").on_hover_text("Enable font shaping and bidirectional ordering. Existing text keeps its original layout until enabled.").changed() {
                            *layout = if shaped { crate::source::TextLayout::Shaped } else { crate::source::TextLayout::Legacy };
                        }
                        ui.horizontal(|ui| {
                            font_picker(ui, font, &mut e.font_search,e.native.as_mut());
                            ui.label("Size");
                            widgets::numeric(ui, size, 1. ..=1024.,e.native.as_mut());
                            ui.label("px");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Letter spacing");
                            widgets::numeric(ui, letter_spacing, -1024. ..=1024.,e.native.as_mut());
                            ui.label("px");
                        });
                        ui.horizontal(|ui| {
                            ui.label("Line spacing");
                            widgets::numeric(ui, line_height, 0.5..=4.,e.native.as_mut());
                            for name in ["left", "center", "right"] {
                                ui.selectable_value(align, name.into(), name);
                            }
                        });
                        native_color(ui, "Color", color, doc.depth(),e.native.as_mut());
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
                                            widgets::numeric(ui, &mut p[0], -100000. ..=100000.,e.native.as_mut());
                                            ui.label("Y");
                                            widgets::numeric(ui, &mut p[1], -100000. ..=100000.,e.native.as_mut());
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
                                        widgets::numeric(ui, value, -100000. ..=100000.,e.native.as_mut());
                                    }
                                });
                            }
                        }
                        native_color(ui, "Fill", fill, doc.depth(),e.native.as_mut());
                        native_color(ui, "Stroke", stroke, doc.depth(),e.native.as_mut());
                        ui.horizontal(|ui| {
                            ui.label("Stroke width");
                            widgets::numeric(ui, stroke_width, 0. ..=256.,e.native.as_mut());
                            ui.label("px");
                        });
                    }
                }
                let invalid = e.invalid();
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
                            !self.busy && !doc.read_only() && invalid.is_none() && e.error.is_none(),
                            egui::Button::new("Apply"),
                        )
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if !stale && e.signature().ok() != before {
            e.error = None;
            e.apply_error = None;
        }
        if e.pending {
            self.source_editor = Some(e);
            return;
        }
        let command = if valid && !stale {
            e.edit_command(doc)
        } else {
            Err("Source draft is invalid".into())
        };
        if apply {
            match command {
                Ok(command) => {
                    let token = e.token;
                    e.pending = true;
                    self.source_editor = Some(e);
                    if let Err(error) = self.submit_source_edit(command, token, ctx) {
                        self.source_edit_finished(token, Err(error));
                    }
                }
                Err(error) => {
                    e.apply_error = Some(error);
                    self.source_editor = Some(e);
                }
            }
        } else if !open || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_source();
        } else {
            match command {
                Ok(EditCommand::Resident(command)) => self.transient = vec![command],
                Ok(EditCommand::Native(_)) => {}
                Err(_) => self.transient.retain(|command| {
                    !command["op"]
                        .as_str()
                        .is_some_and(|op| op.starts_with("source."))
                }),
            }
            self.source_editor = Some(e);
        }
    }
}

fn unlocked(doc: DocumentView<'_>, target: &str) -> Result<(), String> {
    let mut current = Some(target);
    for _ in 0..=16 {
        let Some(id) = current else { return Ok(()) };
        let layer = doc.find_layer(id).ok_or("Unknown image destination")?;
        if layer.locked() {
            return Err("Unlock the image destination first".into());
        }
        current = layer.parent();
    }
    Err("Group nesting limit exceeded".into())
}
fn text_hit<'a>(doc: impl Into<DocumentView<'a>>, point: [f32; 2]) -> Option<String> {
    fn hit(
        doc: DocumentView<'_>,
        parent: Option<&str>,
        point: [f32; 2],
        depth: usize,
    ) -> Option<String> {
        if depth > 16 {
            return None;
        }
        // Reverse the engine's child-before-folder, reversed-sibling order
        // without allocating a layer-order vector or touching any raster.
        for layer in doc.layers().filter(|layer| layer.parent() == parent) {
            if layer.source_kind() == Some("text") {
                let mut current = Some(layer.id());
                let mut visible = true;
                for _ in 0..=16 {
                    let Some(id) = current else { break };
                    let ancestor = doc.find_layer(id)?;
                    if !ancestor.visible() {
                        visible = false;
                        break;
                    }
                    current = ancestor.parent();
                }
                if visible && current.is_none() {
                    let source = layer.source()?;
                    let [a, b, c, d, tx, ty] = source.matrix().map(f64::from);
                    let det = a * d - b * c;
                    if det.is_finite() && det.abs() >= 0.000001 {
                        let origin = layer.origin();
                        let x = f64::from(point[0]) - f64::from(origin[0]) - tx;
                        let y = f64::from(point[1]) - f64::from(origin[1]) - ty;
                        let local = [(d * x - c * y) / det, (-b * x + a * y) / det];
                        let [width, height] = source.dimensions();
                        if local[0] >= 0.
                            && local[1] >= 0.
                            && local[0] < f64::from(width)
                            && local[1] < f64::from(height)
                        {
                            return Some(layer.id().to_owned());
                        }
                    }
                }
            }
            if layer.kind() == "group" {
                if let Some(id) = hit(doc, Some(layer.id()), point, depth + 1) {
                    return Some(id);
                }
            }
        }
        None
    }
    hit(doc.into(), None, point, 0)
}

fn font_picker(
    ui: &mut egui::Ui,
    font: &mut crate::fonts::TextFont,
    search: &mut String,
    mut native: Option<&mut native::Native>,
) {
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
            widgets::text(ui, search, 512, false, true, native.as_deref_mut());
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

#[cfg(test)]
mod validation_tests {
    use super::*;
    use crate::fonts::{self, FontError, InstalledFont, TextFont};
    #[test]
    fn shaped_validation_reuses_only_current_source_and_owned_ready_font_and_retries_status_changes(
    ) {
        let reference = InstalledFont {
            family: "Synthetic validation fixture".into(),
            postscript: "PeerBrushSynthetic-Regular".into(),
            style: "normal".into(),
            weight: 400,
            stretch: 5,
            index: 0,
            sha256: "0".repeat(64),
        };
        let font = ab_glyph::FontArc::new(crate::source::font("regular").unwrap());
        let source = Source {
            width: 180,
            height: 96,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Text {
                text: "office a\u{0301}".into(),
                layout: crate::source::TextLayout::Shaped,
                font: TextFont::Installed(reference.clone()),
                size: 24.,
                line_height: 1.2,
                letter_spacing: 0.,
                align: "left".into(),
                color: [12501, 23003, 34507, 65535],
            },
        };
        let mut editor = Editor {
            document: "synthetic-document".into(),
            revision: 0,
            layer: None,
            parent: None,
            source,
            x: 0,
            y: 0,
            error: None,
            placement: None,
            apply_error: None,
            font_search: String::new(),
            validation: None,
            token: EditToken(0),
            pending: false,
            native: None,
            validation_runs: 0,
        };
        {
            let guard = fonts::test_fonts(vec![(reference.clone(), Ok(font.clone()))]);
            assert_eq!(editor.invalid(), None);
            assert_eq!(editor.validation_runs, 1);
            for _ in 0..8 {
                assert_eq!(editor.invalid(), None);
            }
            assert_eq!(editor.validation_runs, 1);
            assert_eq!(
                guard.requests(),
                9,
                "One cache-only snapshot check per validation; no second resolution"
            );
            if let Content::Text { text, .. } = &mut editor.source.content {
                *text = "\u{10ffff}".into();
            }
            assert!(editor.invalid().is_some());
            assert_eq!(editor.validation_runs, 2);
            assert!(editor.invalid().is_some());
            assert_eq!(
                editor.validation_runs, 2,
                "Invalid layout is cached only for the same Source and snapshot"
            );
            if let Content::Text { text, .. } = &mut editor.source.content {
                *text = "office".into();
            }
            assert_eq!(editor.invalid(), None);
            assert_eq!(editor.validation_runs, 3);
        }
        {
            let _guard = fonts::test_fonts(vec![(reference.clone(), Err(FontError::Loading))]);
            assert_eq!(editor.invalid(), Some(FontError::Loading.to_string()));
            assert!(editor.validation.is_none());
        }
        {
            let _guard = fonts::test_fonts(vec![(reference.clone(), Ok(font.clone()))]);
            assert_eq!(editor.invalid(), None);
            assert_eq!(editor.validation_runs, 4);
        }
        {
            let same_bytes = ab_glyph::FontArc::try_from_vec(
                include_bytes!("../../assets/fonts/UbuntuSans-Regular.ttf").to_vec(),
            )
            .unwrap();
            let _guard = fonts::test_fonts(vec![(reference, Ok(same_bytes))]);
            assert_eq!(editor.invalid(), None);
            assert_eq!(
                editor.validation_runs, 5,
                "A different owned snapshot invalidates derived layout validation"
            );
        }
    }
}
