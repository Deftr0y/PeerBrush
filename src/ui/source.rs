use super::*;
use crate::engine::view::{DocumentView, SourceView};
use crate::psd::memory36::Admitted;
use crate::raster::CanvasLimits;
use crate::source::{Content, Source, TextLayout};
use crate::source::text_position::{Caret, NativeCarets, TextCarets};
use std::sync::atomic::{AtomicU64, Ordering};
#[path = "source/buffer.rs"]
mod buffer;
#[path = "source/font_sample.rs"]
mod font_sample;
#[path = "source/inline.rs"]
mod inline;
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
const TEXT_PREVIEW_IDLE: Duration = Duration::from_millis(250);
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
    frame_drag: Option<FrameDrag>,
    cancelled_frame_drag: Option<u64>,
    pub(super) canvas_text: bool,
    canvas_focus: Option<CanvasFocus>,
    canvas_input: Option<egui::Id>,
    focused_handle: Option<egui::Id>,
    typing_until: Option<f64>,
    inline_anchor: Option<usize>,
    inline_point: Option<inline::Point>,
    inline_click: Option<inline::ClickSequence>,
    inline_affinity: Option<Caret>,
    inline_column: Option<f32>,
    layout: Option<([u8; 32], Option<Arc<inline::Layout>>)>,
    fit_text: bool,
    apply_requested: bool,
    pub(super) show_field_geometry: bool,
    apply_error: Option<String>,
    font_search: String,
    font_sample: font_sample::Preview,
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum CanvasFocus {
    StartEditing,
    ResumeEditing,
}
pub(super) struct Placement {
    anchor: [f32; 2],
    dragged: bool,
    requires_drag: bool,
}
#[derive(Clone, Copy)]
struct FrameDrag {
    handle: FieldHandle,
    press: [f32; 2],
    size: [u32; 2],
    matrix: [f32; 6],
    fit_text: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub(super) enum FieldHandle {
    Resize(usize),
    Move,
    Rotate,
}
pub(super) struct TransformDraft {
    pub token: EditToken,
    pub bounds: [f64; 4],
    pub corners: [[f64; 2]; 4],
    pub resume_focus: bool,
    matrix: [f32; 6],
    fit_text: bool,
}

#[derive(Debug, serde::Serialize)]
pub(super) enum CanvasPreviewKey {
    Field { editor: u64, handle: Option<FieldHandle>, start: [f32; 2] },
    Typing { editor: u64 },
}

fn text_field_input_allowed(ctx: &egui::Context) -> bool {
    !PeerBrush::pointer_contact_cancelled(ctx) && ctx.input(|input| {
        input.focused && !input.key_pressed(egui::Key::Escape)
            && !input.key_down(egui::Key::Space) && !input.pointer.middle_down()
            && !input.pointer.secondary_down()
            && !input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
    })
}

fn text_field_pointer_active(ctx: &egui::Context) -> bool {
    text_field_input_allowed(ctx) && ctx.input(|input| input.pointer.primary_down())
}

impl Editor {
    pub(super) fn moving_text_field(&self) -> bool {
        self.placement.is_some() || self.frame_drag.is_some()
    }

    pub(super) fn previewing(&self) -> bool {
        self.placement.as_ref().is_none_or(|placement| !placement.requires_drag || placement.dragged)
    }

    fn field_drag(&self) -> Option<(Option<FieldHandle>, [f32; 2])> {
        if let Some(drag) = self.frame_drag {
            Some((Some(drag.handle), drag.press))
        } else {
            self.placement.as_ref().filter(|placement| placement.dragged)
                .map(|placement| (None, placement.anchor))
        }
    }

    pub(super) fn canvas_preview_key(
        &self,
        ctx: &egui::Context,
        doc: DocumentView<'_>,
    ) -> Option<CanvasPreviewKey> {
        if !self.canvas_text || self.pending || doc.read_only() || !self.matches_source(doc) {
            return None;
        }
        let editor = self.token.nonce();
        if let Some((handle, start)) = self.field_drag() {
            return text_field_pointer_active(ctx)
                .then_some(CanvasPreviewKey::Field { editor, handle, start });
        }
        self.typing_preview_active(ctx).then_some(CanvasPreviewKey::Typing { editor })
    }

    fn text_changed(&mut self, ctx: &egui::Context) {
        self.error = None;
        self.apply_error = None;
        self.inline_affinity = None;
        self.inline_column = None;
        self.inline_point = None;
        self.inline_click = None;
        self.typing_until = Some(ctx.input(|input| input.time) + TEXT_PREVIEW_IDLE.as_secs_f64());
        ctx.request_repaint_after(TEXT_PREVIEW_IDLE);
    }

    fn typing_preview_active(&self, ctx: &egui::Context) -> bool {
        self.typing_until.is_some_and(|deadline| self.field_drag().is_none()
            && self.has_canvas_focus(ctx) && ctx.input(|input| {
                input.time < deadline && input.focused && !input.key_pressed(egui::Key::Escape)
                    && !input.pointer.middle_down() && !input.pointer.secondary_down()
                    && !input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))
            }))
    }

    pub(super) fn finish_typing_preview(&mut self, ctx: &egui::Context) -> bool {
        if self.typing_preview_active(ctx) {
            return false;
        }
        self.typing_until.take().is_some()
    }

    pub(super) fn needs_preview_carets(&self) -> bool {
        self.canvas_text && matches!(&self.source.content,
            Content::Text { layout: TextLayout::Shaped | TextLayout::Paragraph, .. }
            | Content::Text { font: crate::fonts::TextFont::Installed(_), .. }
        )
    }

    fn layout_key(&self) -> Option<[u8; 32]> {
        if self.is_native() {
            self.owned_command().map(|command| command.signature())
        } else {
            self.signature().ok()
        }
    }

    fn accept_layout(&mut self, signature: [u8; 32], layout: Option<inline::Layout>) {
        if self.needs_preview_carets() && self.layout_key() == Some(signature) {
            self.layout = Some((signature, layout.map(Arc::new)));
        }
    }

    pub(super) fn accept_native_carets(&mut self, signature: [u8; 32], layout: Option<NativeCarets>) {
        self.accept_layout(signature, layout.map(inline::Layout::Native));
    }

    pub(super) fn accept_resident_carets(&mut self, signature: [u8; 32], layout: Option<TextCarets>) {
        self.accept_layout(signature, layout.map(inline::Layout::Resident));
    }

    pub(super) fn resident_caret_request(&self) -> Option<[u8; 32]> {
        (self.needs_preview_carets() && !self.is_native()).then(|| self.layout_key()).flatten()
    }

    pub(super) fn matches_source(&self, doc: DocumentView<'_>) -> bool {
        self.document == doc.id()
            && self.revision == doc.revision()
            && self.native.as_ref().map(|native| native.key) == doc.native().map(|doc| doc.key())
    }

    fn frame_limits(&self) -> CanvasLimits {
        if self.is_native() {
            CanvasLimits::Native
        } else {
            CanvasLimits::Resident
        }
    }

    fn frame_edge(&self) -> u32 {
        self.frame_limits().edge()
    }

    fn invalid(&mut self) -> Option<String> {
        // Native validation belongs to the captured worker and its actual
        // FontOwner/SVG preparation. Never consult the legacy global cache.
        if let Some(native) = &self.native {
            return native.error.clone();
        }
        use ab_glyph::Font;
        let Content::Text {
            layout: TextLayout::Shaped | TextLayout::Paragraph,
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
        json!({"op":if self.layer.is_some(){"source.update"}else{"source.add"},"layer":self.layer,"parent":self.parent,"source":self.source,"x":self.x,"y":self.y,"fit_text":self.fit_text,"document_id":self.document,"source_revision":self.revision})
    }
    pub(super) fn token(&self) -> EditToken {
        self.token
    }
    pub(super) fn is_native(&self) -> bool {
        self.native.is_some()
    }
    pub(super) fn owned_command(&self) -> Option<super::native_dispatch::SharedCommand> {
        // Keep the rejected render's command current until the draft changes.
        let native = self.native.as_ref().filter(|native| native.error.is_none())?;
        native.owned_command(self.signature().ok()?)
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
            &(&self.source, self.x, self.y, &self.layer, &self.parent, self.fit_text),
        )
        .map_err(|_| "Source draft fingerprint refused")?;
        Ok(hash
            .0
            .finish()
            .as_ref()
            .try_into()
            .map_err(|_| "Source draft fingerprint size")?)
    }
    pub(super) fn edit_command(&mut self, doc: DocumentView<'_>) -> Result<EditCommand, String> {
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
                fit_text,
                native,
                ..
            } = self;
            let native = native.as_mut().unwrap();
            let value=native.command(doc,signature,||json!({"op":if layer.is_some(){"source.update"}else{"source.add"},"layer":layer,"parent":parent,"source":source,"x":x,"y":y,"fit_text":fit_text,"document_id":document,"source_revision":revision}))?;
            Ok(EditCommand::Native(value))
        } else {
            Ok(EditCommand::Resident(self.command()))
        }
    }

    pub(super) fn transform_draft(&self, doc: DocumentView<'_>) -> Option<TransformDraft> {
        if !self.canvas_text || self.pending || self.placement.is_some()
            || self.frame_drag.is_some() || !self.matches_source(doc)
        {
            return None;
        }
        let corners = text_field_document_corners(self);
        let mut bounds = [corners[0][0], corners[0][1], corners[0][0], corners[0][1]];
        for [x, y] in corners {
            bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x), bounds[3].max(y)];
        }
        Some(TransformDraft {
            token: self.token,
            bounds,
            corners,
            resume_focus: false,
            matrix: self.source.matrix,
            fit_text: self.fit_text,
        })
    }

    pub(super) fn has_canvas_focus(&self, ctx: &egui::Context) -> bool {
        self.canvas_input.is_some_and(|id| ctx.memory(|memory| memory.has_focus(id)))
    }

    pub(super) fn has_field_handle_focus(&self, ctx: &egui::Context) -> bool {
        self.focused_handle.is_some_and(|id| ctx.memory(|memory| memory.has_focus(id)))
    }

    pub(super) fn resume_canvas_focus(&mut self) {
        self.canvas_focus = Some(CanvasFocus::ResumeEditing);
    }

    pub(super) fn pause_canvas_pointer_for_navigation(&mut self, ctx: &egui::Context) {
        self.inline_anchor = None;
        self.inline_point = None;
        self.inline_click = None;
        self.inline_column = None;
        if self.canvas_input.is_some_and(|id| ctx.memory(|memory| {
            memory.has_focus(id) || memory.had_focus_last_frame(id)
        })) {
            self.resume_canvas_focus();
        }
    }

    pub(super) fn project_transform(
        &mut self,
        draft: &TransformDraft,
        [a, b, c, d, tx, ty]: [f64; 6],
    ) -> Result<(), String> {
        let [x, y] = [self.x, self.y].map(f64::from);
        let local = [a, b, c, d, a * x + c * y + tx - x, b * x + d * y + ty - y];
        let matrix = crate::source::prepend_matrix(local.map(|n| n as f32), draft.matrix);
        if matrix == draft.matrix {
            self.restore_transform(draft);
        } else if matrix != self.source.matrix || !self.fit_text {
            let ([width, height], _, _) = crate::source::projected_text_frame(
                matrix, [self.source.width, self.source.height], None, [self.x, self.y],
            )?;
            self.frame_limits().check(width, height)?;
            self.set_transform(matrix, true);
        }
        Ok(())
    }

    pub(super) fn restore_transform(&mut self, draft: &TransformDraft) {
        self.set_transform(draft.matrix, draft.fit_text);
    }

    fn set_transform(&mut self, matrix: [f32; 6], fit_text: bool) {
        if (matrix, fit_text) != (self.source.matrix, self.fit_text) {
            self.source.matrix = matrix;
            self.fit_text = fit_text;
            self.error = None;
            self.apply_error = None;
        }
    }

    fn set_frame(&mut self, [width, height]: [u32; 2], matrix: [f32; 6], fit_text: bool) {
        self.set_transform(matrix, fit_text);
        if [self.source.width, self.source.height] != [width, height] {
            self.source.width = width;
            self.source.height = height;
            self.inline_column = None;
            self.error = None;
            self.apply_error = None;
        }
    }
}
fn native_color(
    ui: &mut egui::Ui,
    label: &str,
    color: &mut [u16; 4],
    depth: u16,
) -> bool {
    ui.horizontal(|ui| {
        ui.label(label);
        let original = *color;
        let before = color.map(|word| f64::from(word) / 65535.);
        let mut rgb = [before[0], before[1], before[2]];
        let mut alpha = before[3];
        if effect_color::control(ui, ("source color", label), label, &mut rgb, &mut alpha, Some("Opacity"), depth == 16)
            .is_some_and(|response| response.changed())
        {
            for (index, value) in [rgb[0], rgb[1], rgb[2], alpha].into_iter().enumerate() {
                if value != before[index] {
                    let value = value.clamp(0., 1.);
                    color[index] = if depth == 16 {
                        (value * 65535.).round() as u16
                    } else {
                        (value * 255.).round() as u16 * 257
                    };
                }
            }
        }
        *color != original
    }).inner
}

fn text_typography(ui: &mut egui::Ui, editor: &mut Editor, depth: u16) -> bool {
    let Content::Text { font, size, align, color, .. } = &mut editor.source.content else {
        return false;
    };
    let mut changed = font_picker(
        ui, font, &mut editor.font_search, &mut editor.font_sample, editor.native.as_mut(),
    );
    let size_label = ui.label("Size");
    changed |= widgets::numeric(ui, size, 1. ..=1024., editor.native.as_mut())
        .labelled_by(size_label.id)
        .on_hover_text("Font size in document pixels")
        .changed();
    ui.label("px");
    ui.label("Alignment");
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.;
        for (name, icon, label) in [
            ("left", Icon::AlignLeft, "Align left"),
            ("center", Icon::AlignCenter, "Center text"),
            ("right", Icon::AlignRight, "Align right"),
        ] {
            if icons::selectable(ui, icon, align == name, label).clicked() && align != name {
                *align = name.into();
                changed = true;
            }
        }
    });
    changed | native_color(ui, "Color", color, depth)
}

fn paragraph_toggle(ui: &mut egui::Ui, layout: &mut TextLayout) -> bool {
    let mut wrap = *layout == TextLayout::Paragraph;
    let response = icons::labeled_choice(ui, Icon::WrapText, "Wrap to field", Some(wrap), 0.)
        .on_hover_text("Automatically start a new line at the field edge. Text reflows when the width changes.");
    if response.clicked() {
        wrap = !wrap;
        *layout = if wrap { TextLayout::Paragraph } else { TextLayout::Shaped };
    }
    response.widget_info(|| egui::WidgetInfo::selected(
        egui::WidgetType::Checkbox, response.enabled(), wrap, "Wrap to field",
    ));
    response.clicked()
}

fn shaping_toggle(ui: &mut egui::Ui, layout: &mut TextLayout) -> bool {
    let mut shaped = layout.uses_shaping();
    let changed = ui.add_enabled(
        *layout != TextLayout::Paragraph,
        egui::Checkbox::new(&mut shaped, "Ligatures and joining"),
    ).on_hover_text("Enable font shaping and bidirectional ordering. Paragraph text always uses shaping.")
        .changed();
    if changed {
        *layout = if shaped { TextLayout::Shaped } else { TextLayout::Legacy };
    }
    changed
}

pub(super) fn path_closure(ui: &mut egui::Ui, closed: &mut bool, enabled: bool) -> bool {
    ui.add_enabled_ui(enabled, |ui| {
        ui.horizontal(|ui| {
            let before = *closed;
            for (value, icon, label, help) in [
                (false, Icon::VectorPath, "Open path", "Leave the first and last anchors separate."),
                (true, Icon::ClosedPath, "Closed path", "Join the last anchor back to the first."),
            ] {
                let response = icons::selectable(ui, icon, *closed == value, label)
                    .on_hover_text(help);
                if response.clicked() {
                    *closed = value;
                }
            }
            *closed != before
        }).inner
    }).inner
}

fn text_spacing(
    ui: &mut egui::Ui,
    letter_spacing: &mut f32,
    line_height: &mut f32,
    mut native: Option<&mut native::Native>,
) -> bool {
    let letter_label = ui.label("Letter spacing");
    let letters = widgets::numeric(ui, letter_spacing, -1024. ..=1024., native.as_deref_mut())
        .labelled_by(letter_label.id)
        .on_hover_text("Extra space between letters, in document pixels");
    ui.label("px");
    let line_label = ui.label("Line spacing");
    let lines = widgets::numeric(ui, line_height, 0.5..=4., native)
        .labelled_by(line_label.id)
        .on_hover_text("Line height as a multiple of the font size");
    ui.label("×");
    letters.changed() || lines.changed()
}

fn source_geometry(ui: &mut egui::Ui, editor: &mut Editor) -> bool {
    let origin = [editor.x, editor.y];
    let text = matches!(editor.source.content, Content::Text { .. });
    let mut matrix = editor.source.matrix;
    let size = [editor.source.width, editor.source.height];
    let mut next_size = size;
    let mut moved = false;
    if text {
        // The field's local origin can differ from the raster origin after a
        // rotation or a reframe. Translate the matrix to keep its basis intact.
        let position = [
            f64::from(editor.x) + f64::from(matrix[4]),
            f64::from(editor.y) + f64::from(matrix[5]),
        ];
        let mut next = position;
        ui.horizontal(|ui| {
            for (axis, name) in ["X", "Y"].into_iter().enumerate() {
                let label = ui.label(name);
                widgets::numeric(ui, &mut next[axis], -100000. ..=100000., editor.native.as_mut())
                    .labelled_by(label.id)
                    .on_hover_text("Position of the text field's origin in document pixels");
            }
            ui.label("px");
        });
        if ui.is_enabled() {
            for axis in 0..2 {
                if next[axis] != position[axis] {
                    matrix[axis + 4] = (next[axis] - f64::from(origin[axis])) as f32;
                }
            }
        }
        moved = matrix != editor.source.matrix;
    } else if editor.layer.is_none() {
        ui.horizontal(|ui| {
            ui.label("X");
            widgets::numeric(ui, &mut editor.x, -100000..=100000, editor.native.as_mut())
                .on_hover_text("Horizontal position in document pixels");
            ui.label("Y");
            widgets::numeric(ui, &mut editor.y, -100000..=100000, editor.native.as_mut())
                .on_hover_text("Vertical position in document pixels");
            ui.label("px");
        });
    }
    if text {
        let edge = editor.frame_edge();
        ui.horizontal(|ui| {
            for (value, name) in next_size.iter_mut().zip(["Field width", "Height"]) {
                let label = ui.label(name);
                widgets::numeric(ui, value, 1..=edge, editor.native.as_mut()).labelled_by(label.id);
            }
            ui.label("px");
        });
        if ui.is_enabled() {
            editor.set_frame(next_size, matrix, editor.fit_text || moved);
        }
    }
    moved || [editor.x, editor.y] != origin || [editor.source.width, editor.source.height] != size
}

fn prepare_widgets(ctx: &egui::Context, editor: &mut Editor) {
    if let Some(native) = &mut editor.native {
        native.error = None;
        if native.widgets.is_none() {
            match widgets::Widgets::new(ctx, native.pool()) {
                Ok(widgets) => native.widgets = Some(widgets),
                Err(error) => native.error = Some(error),
            }
        }
    }
}

fn text_field_document_corners(editor: &Editor) -> [[f64; 2]; 4] {
    let [a, b, c, d, tx, ty] = editor.source.matrix.map(f64::from);
    let [w, h] = [editor.source.width, editor.source.height].map(f64::from);
    [[0., 0.], [w, 0.], [w, h], [0., h]].map(|[x, y]| {
        [f64::from(editor.x) + a * x + c * y + tx, f64::from(editor.y) + b * x + d * y + ty]
    })
}

fn text_field_corners(editor: &Editor, projection: &view::Projection) -> [Pos2; 4] {
    text_field_document_corners(editor).map(|point| projection.point(point.map(|v| v as f32)))
}

const FRAME_HANDLES: [([i8; 2], &str); 8] = [
    ([-1, -1], "Resize top left of text field"),
    ([0, -1], "Resize top of text field"),
    ([1, -1], "Resize top right of text field"),
    ([1, 0], "Resize right of text field"),
    ([1, 1], "Resize bottom right of text field"),
    ([0, 1], "Resize bottom of text field"),
    ([-1, 1], "Resize bottom left of text field"),
    ([-1, 0], "Resize left of text field"),
];

impl FrameDrag {
    fn update(self, editor: &mut Editor, point: [f32; 2], shift: bool) -> Result<(), String> {
        let [a, b, c, d, tx, ty] = self.matrix.map(f64::from);
        let [x, y] = std::array::from_fn(|axis| f64::from(point[axis]) - f64::from(self.press[axis]));
        let handle = match self.handle {
            FieldHandle::Resize(handle) => handle,
            FieldHandle::Move => {
                let mut matrix = self.matrix;
                matrix[4] = (tx + x) as f32;
                matrix[5] = (ty + y) as f32;
                return self.apply_frame(editor, self.size, matrix, matrix != self.matrix);
            }
            FieldHandle::Rotate => {
                let [w, h] = self.size.map(|edge| f64::from(edge) * 0.5);
                let cx = a * w + c * h;
                let cy = b * w + d * h;
                let px = f64::from(self.press[0]) - (f64::from(editor.x) + tx + cx);
                let py = f64::from(self.press[1]) - (f64::from(editor.y) + ty + cy);
                let qx = px + x;
                let qy = py + y;
                let mut angle = (px * qy - py * qx).atan2(px * qx + py * qy);
                if shift {
                    let step = crate::transform::ANGLE_SNAP.to_radians();
                    angle = (angle / step).round() * step;
                }
                return self.rotate(editor, angle);
            }
        };
        let determinant = a * d - b * c;
        if determinant.abs() <= 1e-6 {
            return Err("Text field transform cannot be resized".into());
        }
        let delta = [(d * x - c * y) / determinant, (a * y - b * x) / determinant];
        let axes = FRAME_HANDLES[handle].0;
        let edge = editor.frame_edge();
        let initial = self.size.map(f64::from);
        let [width, height] = initial;
        let size = if shift && axes[0] != 0 && axes[1] != 0 {
            let diagonal = [width * f64::from(axes[0]), height * f64::from(axes[1])];
            let scale = 1. + (delta[0] * diagonal[0] + delta[1] * diagonal[1])
                / (width * width + height * height);
            let scale = scale.min(f64::from(edge) / width.max(height));
            initial.map(|axis| (axis * scale).round().max(1.) as u32)
        } else {
            std::array::from_fn(|axis| {
                (initial[axis] + delta[axis] * f64::from(axes[axis]))
                    .round().clamp(1., f64::from(edge)) as u32
            })
        };
        let [left, top] = std::array::from_fn(|axis| {
            if axes[axis] < 0 { f64::from(self.size[axis]) - f64::from(size[axis]) } else { 0. }
        });
        let mut matrix = self.matrix;
        matrix[4] = (tx + a * left + c * top) as f32;
        matrix[5] = (ty + b * left + d * top) as f32;
        self.apply_frame(editor, size, matrix, left != 0. || top != 0.)
    }

    fn rotate(self, editor: &mut Editor, angle: f64) -> Result<(), String> {
        let [a, b, c, d, tx, ty] = self.matrix.map(f64::from);
        let [w, h] = self.size.map(|edge| f64::from(edge) * 0.5);
        let (cx, cy) = (a * w + c * h, b * w + d * h);
        let matrix = if angle == 0. {
            self.matrix
        } else {
            let (sin, cos) = angle.sin_cos();
            [
                cos * a - sin * b, sin * a + cos * b,
                cos * c - sin * d, sin * c + cos * d,
                tx + cx - (cos * cx - sin * cy),
                ty + cy - (sin * cx + cos * cy),
            ].map(|v| v as f32)
        };
        self.apply_frame(editor, self.size, matrix, matrix != self.matrix)
    }

    fn apply_frame(self, editor: &mut Editor, size: [u32; 2], matrix: [f32; 6], translated: bool) -> Result<(), String> {
        let ([width, height], _, _) = crate::source::projected_text_frame(matrix, size, None, [editor.x, editor.y])?;
        editor.frame_limits().check(width, height)?;
        editor.set_frame(size, matrix, self.fit_text || translated);
        editor.apply_error = None;
        Ok(())
    }
}

fn text_handle_keys(ui: &egui::Ui, response: &egui::Response, editor: &mut Editor, handle: FieldHandle) {
    if !response.enabled() || !response.has_focus() || editor.frame_drag.is_some() {
        return;
    }
    editor.focused_handle = Some(response.id);
    ui.memory_mut(|memory| memory.set_focus_lock_filter(response.id, egui::EventFilter {
        horizontal_arrows: true,
        vertical_arrows: true,
        ..Default::default()
    }));
    let [x, y] = ui.input_mut(|input| {
        let mut delta = [0.; 2];
        input.events.retain(|event| {
            let egui::Event::Key { key, modifiers, pressed: true, .. } = event else {
                return true;
            };
            if modifiers.any() && *modifiers != egui::Modifiers::SHIFT {
                return true;
            }
            let (axis, sign) = match key {
                egui::Key::ArrowLeft => (0, -1.),
                egui::Key::ArrowRight => (0, 1.),
                egui::Key::ArrowUp => (1, -1.),
                egui::Key::ArrowDown => (1, 1.),
                _ => return true,
            };
            delta[axis] += sign * if modifiers.shift { 10. } else { 1. };
            false
        });
        delta
    });
    if x == 0. && y == 0. {
        return;
    }
    let change = FrameDrag {
        handle,
        press: [0.; 2],
        size: [editor.source.width, editor.source.height],
        matrix: editor.source.matrix,
        fit_text: editor.fit_text,
    };
    let result = match handle {
        FieldHandle::Move => change.update(editor, [x, y], false),
        FieldHandle::Rotate => change.rotate(editor, f64::from(x + y).to_radians()),
        FieldHandle::Resize(index) => {
            let [horizontal, vertical] = FRAME_HANDLES[index].0;
            if (horizontal == 0 || x == 0.) && (vertical == 0 || y == 0.) {
                return;
            }
            let [a, b, c, d, ..] = change.matrix;
            change.update(editor, [a * x + c * y, b * x + d * y], false)
        }
    };
    if let Err(error) = result {
        editor.apply_error = Some(error);
    }
    ui.ctx().request_repaint();
}

fn frame_handles(corners: [Pos2; 4], active: Option<FieldHandle>) -> [Rect; 8] {
    let x = corners[1] - corners[0];
    let y = corners[3] - corners[0];
    let active_resize = match active {
        Some(FieldHandle::Resize(index)) => index,
        _ => 4,
    };
    std::array::from_fn(|index| {
        if (x.length() < 32. || y.length() < 32.) && index != active_resize {
            return Rect::NOTHING;
        }
        let [u, v] = FRAME_HANDLES[index].0.map(|axis| (f32::from(axis) + 1.) / 2.);
        Rect::from_center_size(corners[0] + u * x + v * y, Vec2::splat(14.))
    })
}

fn frame_control(corners: [Pos2; 4], opposite: bool) -> Rect {
    let side = corners[1] - corners[0];
    let down = corners[3] - corners[0];
    let mut normal = Vec2::new(-side.y, side.x).normalized();
    if normal.dot(down) > 0. {
        normal = -normal;
    }
    let center = corners[0] + side / 2. + if opposite { down - normal * 19. } else { normal * 19. };
    Rect::from_center_size(center, Vec2::splat(20.))
}

pub(super) fn text_actions_rect(canvas: Rect, bounds: Rect) -> Rect {
    let canvas = canvas.shrink(4.);
    let size = Vec2::new(52., 24.);
    // Leave room for the move and rotate handles outside the field edges.
    let gap = 34.;
    if canvas.width() < size.x || canvas.height() < size.y {
        return Rect::NOTHING;
    }
    let x = (bounds.right() - size.x).clamp(canvas.left(), canvas.right() - size.x);
    let y = if bounds.top() - gap - size.y >= canvas.top() {
        bounds.top() - gap - size.y
    } else if bounds.bottom() + gap + size.y <= canvas.bottom() {
        bounds.bottom() + gap
    } else {
        return Rect::from_min_size(egui::pos2(canvas.right() - size.x, canvas.top()), size);
    };
    Rect::from_min_size(egui::pos2(x, y.clamp(canvas.top(), canvas.bottom() - size.y)), size)
}

pub(super) fn text_canvas_actions(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    actions: Rect,
    id: egui::Id,
    apply_enabled: bool,
    labels: [(&str, &str); 2],
) -> (bool, bool) {
    if !actions.is_positive() {
        return (false, false);
    }
    painter.rect_filled(actions, 3., ui.visuals().panel_fill);
    let size = Vec2::new(actions.width() / 2., actions.height());
    let [apply, cancel] = [Icon::Check, Icon::Cross].map(|icon| {
        let apply = matches!(icon, Icon::Check);
        let (label, help) = labels[usize::from(!apply)];
        let min = if apply { actions.min } else { actions.min + Vec2::new(size.x, 0.) };
        let rect = Rect::from_min_size(min, size);
        let response = ui.add_enabled_ui(!apply || apply_enabled, |ui| {
            ui.interact(rect, id.with(label), egui::Sense::click())
        }).inner.on_hover_text(help).on_disabled_hover_text("Waiting for the current full-detail preview");
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), label));
        if response.hovered() {
            painter.rect_filled(rect, 3., ui.style().interact(&response).weak_bg_fill);
        }
        icons::paint(painter, rect.shrink(3.), icon, response.enabled());
        icons::paint_focus(ui, &response);
        response.clicked()
    });
    (apply, cancel)
}

fn drag_text_field(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    editor: &mut Editor,
    projection: &view::Projection,
    allow_input: bool,
) -> ([Rect; 10], bool) {
    let corners = text_field_corners(editor, projection);
    let active = editor.frame_drag.map(|drag| drag.handle);
    let handles = frame_handles(corners, active);
    let actions = text_actions_rect(painter.clip_rect(), Rect::from_points(&corners));
    let hit = |point| {
        if !painter.clip_rect().contains(point) || actions.contains(point)
            || ui.ctx().layer_id_at(point) != Some(ui.layer_id()) {
            return None;
        }
        handles.iter().position(|handle| handle.contains(point)).map(FieldHandle::Resize)
            .or_else(|| frame_control(corners, false).contains(point).then_some(FieldHandle::Move))
            .or_else(|| frame_control(corners, true).contains(point).then_some(FieldHandle::Rotate))
    };
    let (press, release, pointer) = ui.input(|input| {
        if !allow_input || input.key_down(egui::Key::Space) || input.pointer.middle_down() || input.pointer.secondary_down() {
            return (None, None, None);
        }
        let (press, release) = canvas_button_samples(input, active.is_some());
        (press, release, input.pointer.interact_pos())
    });
    if let Some((handle, point)) = press.and_then(|point| hit(point).map(|handle| (handle, point))) {
        editor.frame_drag = Some(FrameDrag {
            handle,
            press: projection.document(point),
            size: [editor.source.width, editor.source.height],
            matrix: editor.source.matrix,
            fit_text: editor.fit_text,
        });
        editor.inline_anchor = None;
        editor.inline_point = None;
        editor.inline_click = None;
        if let Some(id) = editor.canvas_input {
            ui.memory_mut(|memory| {
                if memory.had_focus_last_frame(id) {
                    memory.request_focus(id);
                }
            });
        }
    }
    let dragging = editor.frame_drag.is_some();
    if let Some(drag) = editor.frame_drag {
        let sample = release.or_else(|| pointer.map(|point| (point, ui.input(|i| i.modifiers))));
        if let Some((point, modifiers)) = sample {
            if let Err(error) = drag.update(editor, projection.document(point), modifiers.shift) {
                editor.apply_error = Some(error);
            }
        }
        if release.is_some() {
            editor.frame_drag = None;
        }
    }
    let corners = text_field_corners(editor, projection);
    let handles = frame_handles(corners, editor.frame_drag.map(|drag| drag.handle));
    for (index, handle) in handles.iter().copied().enumerate().filter(|(_, handle)| handle.is_positive()) {
        let ([x, y], label) = FRAME_HANDLES[index];
        let direction = (corners[1] - corners[0]).normalized() * f32::from(x)
            + (corners[3] - corners[0]).normalized() * f32::from(y);
        let cursor = if direction.x.abs() > 2.4 * direction.y.abs() {
            egui::CursorIcon::ResizeHorizontal
        } else if direction.y.abs() > 2.4 * direction.x.abs() {
            egui::CursorIcon::ResizeVertical
        } else if direction.x * direction.y >= 0. {
            egui::CursorIcon::ResizeNwSe
        } else {
            egui::CursorIcon::ResizeNeSw
        };
        let sense = if allow_input && !dragging && painter.clip_rect().intersects(handle) {
            egui::Sense::focusable_noninteractive()
        } else {
            egui::Sense::hover()
        };
        let response = ui.interact(handle, ui.id().with(("resize text field", editor.token.nonce(), index)), sense)
            .on_hover_cursor(cursor).on_hover_text(if x != 0 && y != 0 {
                format!("{label} · Shift-drag keeps proportions · Arrow keys resize 1 px, or 10 px with Shift")
            } else {
                format!("{label} · Arrow keys resize 1 px, or 10 px with Shift")
            });
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, ui.is_enabled(), label));
        painter.rect_filled(handle.shrink(3.), 1., ACCENT);
        icons::paint_focus(ui, &response);
        if sense.is_focusable() {
            text_handle_keys(ui, &response, editor, FieldHandle::Resize(index));
        }
    }
    let controls = [frame_control(corners, false), frame_control(corners, true)];
    for (rect, icon, label, tip, cursor, handle) in [
        (controls[0], Icon::Move, "Move text field", "Drag to move · Arrow keys move 1 px, or 10 px with Shift",
            egui::CursorIcon::Move, FieldHandle::Move),
        (controls[1], Icon::Rotate, "Shift-drag snaps to 15° · Arrow keys rotate 1°, or 10° with Shift",
            egui::CursorIcon::Grab, FieldHandle::Rotate),
    ] {
        let sense = if allow_input && !dragging && painter.clip_rect().intersects(rect) {
            egui::Sense::focusable_noninteractive()
        } else {
            egui::Sense::hover()
        };
        let response = ui.interact(rect, ui.id().with((label, editor.token.nonce())), sense)
            .on_hover_cursor(cursor).on_hover_text(tip);
        response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Slider, ui.is_enabled(), label));
        let active = editor.frame_drag.is_some_and(|drag| drag.handle == handle);
        if active {
            ui.ctx().set_cursor_icon(if handle == FieldHandle::Rotate { egui::CursorIcon::Grabbing } else { cursor });
        }
        painter.rect_filled(rect, 3., ui.visuals().panel_fill);
        let color = if response.hovered() || active { ACCENT } else { Color32::WHITE };
        icons::paint_tinted(painter, rect.shrink(1.), icon, color);
        icons::paint_focus(ui, &response);
        if sense.is_focusable() {
            text_handle_keys(ui, &response, editor, handle);
        }
    }
    let excluded = std::array::from_fn(|index| {
        handles.get(index).copied().unwrap_or_else(|| controls[index - handles.len()])
    });
    (excluded, dragging)
}

fn canvas_text_editor(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    editor: &mut Editor,
    projection: &view::Projection,
    placed_this_frame: bool,
) -> bool {
    // A batched placement press can be exactly on the new top-left handle.
    editor.focused_handle = None;
    let (handles, dragging) = drag_text_field(ui, painter, editor, projection, !placed_this_frame);
    let corners = text_field_corners(editor, projection);

    prepare_widgets(ui.ctx(), editor);
    let id = widgets::canvas_id(
        editor.native.as_ref(),
        egui::Id::new(("canvas text", editor.token.nonce())),
    );
    let bounds = Rect::from_points(&corners);
    let actions = text_actions_rect(painter.clip_rect(), bounds);
    let mut excluded_handles = [Rect::NOTHING; 11];
    excluded_handles[..10].copy_from_slice(&handles);
    excluded_handles[10] = actions;
    let exclude_canvas = [painter.clip_rect()];
    let excluded = if dragging {
        &exclude_canvas[..]
    } else if placed_this_frame && editor.layer.is_some() {
        // The press that opens an existing field places the caret.
        &[]
    } else {
        &excluded_handles[..]
    };
    let inline = inline::edit(ui, painter, editor, projection, bounds, excluded, id);
    let cancel = if !dragging && !placed_this_frame {
        let (apply, cancel) = text_canvas_actions(ui, painter, actions, id, true, [
            ("Apply text on canvas", "Apply text · Ctrl/Cmd+Enter"),
            ("Cancel text on canvas", "Discard the text draft · Escape"),
        ]);
        editor.apply_requested |= apply;
        cancel
    } else {
        false
    };
    if inline || cancel {
        return cancel;
    }
    editor.inline_point = None;
    editor.inline_anchor = None;
    editor.inline_click = None;
    editor.inline_column = None;
    let canvas = painter.clip_rect().shrink(4.);
    if canvas.width() < 72. || canvas.height() < 92. {
        return false;
    }
    let width = bounds.width().clamp(144., 320.).min(canvas.width());
    let height = 80.;
    let top = if bounds.bottom() + height + 8. <= canvas.bottom() {
        bounds.bottom() + 8.
    } else {
        bounds.top() - height - 8.
    };
    let field = Rect::from_min_size(
        egui::pos2(
            bounds.left().clamp(canvas.left(), canvas.right() - width),
            top.clamp(canvas.top(), canvas.bottom() - height),
        ),
        Vec2::new(width, height),
    );
    let field = if field.intersects(actions) && actions.top() - height >= canvas.top() {
        field.translate(Vec2::new(0., actions.top() - height - field.top()))
    } else if field.intersects(actions) && actions.bottom() + height <= canvas.bottom() {
        field.translate(Vec2::new(0., actions.bottom() - field.top()))
    } else {
        field
    };
    let Content::Text { text, .. } = &mut editor.source.content else { return false };
    if editor.canvas_focus == Some(CanvasFocus::ResumeEditing) && egui::TextEdit::load_state(ui.ctx(), id).is_some() {
        ui.ctx().memory_mut(|memory| memory.request_focus(id));
    }
    let selection = (editor.canvas_focus == Some(CanvasFocus::StartEditing) && editor.layer.is_none())
        .then(|| egui::text::CCursorRange::two(
            egui::text::CCursor::new(0), egui::text::CCursor::new(text.chars().count()),
        ));
    painter.rect_filled(field, 4., ui.visuals().panel_fill);
    let input = ui.scope_builder(
        egui::UiBuilder::new().id_salt(("canvas text", editor.token.nonce())).max_rect(field.shrink(4.)),
        |ui| {
            ui.set_clip_rect(field);
            egui::ScrollArea::vertical().max_height(height - 8.).show(ui, |ui| {
                widgets::canvas_body(ui, text, editor.native.as_mut(), id, Some(width - 12.), selection)
            }).inner
        },
    ).inner;
    editor.canvas_input = Some(input.id);
    input.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Canvas text"));
    if editor.canvas_focus.is_some() && input.id == id && egui::TextEdit::load_state(ui.ctx(), id).is_some() {
        editor.canvas_focus = None;
        input.request_focus();
        ui.ctx().request_repaint();
    }
    if input.changed() {
        editor.text_changed(ui.ctx());
    }
    false
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
        self.interrupt_animation();
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
            let color = self.palette_value(&super::color::Target::Foreground, doc.depth() == 16);
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
        let canvas_text = self.tool == Tool::Text && matches!(&source.content, Content::Text { .. });
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
            frame_drag: None,
            cancelled_frame_drag: None,
            canvas_text,
            canvas_focus: canvas_text.then_some(CanvasFocus::StartEditing),
            canvas_input: None,
            focused_handle: None,
            typing_until: None,
            inline_anchor: None,
            inline_point: None,
            inline_click: None,
            inline_affinity: None,
            inline_column: None,
            layout: None,
            fit_text: false,
            apply_requested: false,
            show_field_geometry: false,
            apply_error: None,
            font_search,
            font_sample: font_sample::Preview::default(),
            validation: None,
            token,
            pending: false,
            native,
            #[cfg(test)]
            validation_runs: 0,
        });
        self.last_preview = None;
    }
    pub(super) fn text_toolbar(&mut self, ui: &mut egui::Ui, doc: DocumentView<'_>) {
        let transforming = self.free_transform.is_some() || self.free_transform_frame;
        let can_transform = !transforming && !self.busy && !doc.read_only()
            && self.source_editor.as_ref().map_or_else(
                || doc.find_layer(&self.selected).is_some_and(|layer| layer.source_kind() == Some("text")),
                |editor| editor.transform_draft(doc).is_some(),
            );
        let mut editor = self.source_editor.as_mut().filter(|editor| editor.canvas_text);
        let available = editor.as_ref().is_some_and(|editor| {
            !transforming && !editor.pending && editor.placement.is_none() && editor.frame_drag.is_none()
        });
        let editable = available && !self.busy && !doc.read_only()
            && editor.as_ref().is_some_and(|editor| editor.matches_source(doc));
        let mut changed = false;
        let transform = ui.horizontal(|ui| {
            let transform = can_transform
                && icons::button(ui, Icon::Scale, "Free Transform text · Ctrl/Cmd+T").clicked();
            if let Some(editor) = editor.as_deref_mut() {
                if editor.native.as_ref().is_some_and(|native| native.widgets.is_none()) {
                    prepare_widgets(ui.ctx(), editor);
                }
                changed = ui.push_id(editor.token.nonce(), |ui| {
                    ui.add_enabled_ui(editable, |ui| {
                        let typography = text_typography(ui, editor, doc.depth());
                        let Content::Text { layout, .. } = &mut editor.source.content else {
                            return typography;
                        };
                        typography | shaping_toggle(ui, layout)
                    }).inner
                }).inner;
            } else if !doc.read_only() {
                ui.label("Click to edit or place text. Drag to draw a text field.");
            }
            transform
        }).inner;
        if transform {
            self.open_free_transform_from(ui.ctx(), doc, true);
            return;
        }
        let Some(editor) = editor else {
            return;
        };
        let cancel = ui.horizontal(|ui| {
            changed |= ui.push_id((editor.token.nonce(), "text spacing"), |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    let Content::Text { layout, letter_spacing, line_height, .. } = &mut editor.source.content else {
                        return false;
                    };
                    text_spacing(ui, letter_spacing, line_height, editor.native.as_mut())
                        | paragraph_toggle(ui, layout)
                }).inner
            }).inner;
            ui.add_enabled_ui(editable, |ui| {
                let dimensions = icons::labeled_choice(
                    ui,
                    Icon::CanvasBounds,
                    "Field dimensions",
                    Some(editor.show_field_geometry),
                    0.,
                );
                if dimensions.clicked() {
                    editor.show_field_geometry = !editor.show_field_geometry;
                    ui.ctx().request_repaint();
                }
                dimensions.widget_info(|| egui::WidgetInfo::selected(
                    egui::WidgetType::Checkbox,
                    dimensions.enabled(),
                    editor.show_field_geometry,
                    "Field dimensions",
                ));
            });
            if ui
                .add_enabled(editable, egui::Button::new("Apply text"))
                .on_hover_text("Apply the canvas text draft · Ctrl/Cmd+Enter")
                .clicked()
            {
                editor.apply_requested = true;
            }
            let cancel = ui
                .add_enabled(available, egui::Button::new("Cancel text"))
                .on_hover_text("Discard the canvas text draft · Escape")
                .clicked();
            if let Some(error) = editor.apply_error.as_ref().or(editor.error.as_ref()) {
                ui.add(egui::Label::new(RichText::new(error).color(ACCENT)).truncate()).on_hover_text(error);
            }
            cancel
        }).inner;
        if cancel {
            self.cancel_source();
            return;
        }
        if editor.show_field_geometry {
            changed |= ui.push_id((editor.token.nonce(), "text field geometry"), |ui| {
                ui.add_enabled_ui(editable, |ui| {
                    ui.horizontal_wrapped(|ui| source_geometry(ui, editor)).inner
                }).inner
            }).inner;
        }
        if changed {
            editor.error = None;
            editor.apply_error = None;
            editor.inline_affinity = None;
            editor.inline_column = None;
        }
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
        self.native_source_preview = None;
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
            .filter(|editor| editor.previewing() && matches!(editor.source.content, Content::Text { .. }))
        else {
            return;
        };
        let points = text_field_corners(editor, &self.view.projection(rect, scale));
        painter.add(egui::Shape::closed_line(points.to_vec(), Stroke::new(1., ACCENT)));
    }
    pub(super) fn cancel_interrupted_text_drag(&mut self, ctx: &egui::Context, doc: DocumentView<'_>) {
        let Some(editor) = &mut self.source_editor else { return };
        let Some(drag) = editor.frame_drag else {
            return;
        };
        let has_sample = ctx.input(|input| {
            input.pointer.primary_down() || primary_release_samples(input).next().is_some()
        });
        if editor.matches_source(doc) && !doc.read_only() && !self.busy
            && !self.lifecycle_frame && self.lifecycle.is_none()
            && text_field_input_allowed(ctx) && has_sample
        {
            return;
        }
        editor.frame_drag = None;
        editor.set_frame(drag.size, drag.matrix, drag.fit_text);
        editor.pause_canvas_pointer_for_navigation(ctx);
        editor.apply_error = None;
        editor.cancelled_frame_drag = Some(ctx.cumulative_pass_nr());
        self.preview_epoch.fetch_add(1, Ordering::Relaxed);
        self.last_preview = None;
        ctx.request_repaint();
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
        self.cancel_interrupted_text_drag(ui.ctx(), doc);
        let cancelled = Self::pointer_contact_cancelled(ui.ctx()) || ui.input(|input| {
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
            .is_some_and(|editor| !editor.matches_source(doc));
        let cancelled_drag = self.source_editor.as_ref().is_some_and(|editor| {
            editor.cancelled_frame_drag == Some(ui.ctx().cumulative_pass_nr())
        });
        if cancelled_drag && !stale && !doc.read_only() && !self.busy {
            self.preview_text_field(doc);
            self.text_field_outline(painter, rect, scale);
            return;
        }
        if cancelled || stale || doc.read_only() || self.busy {
            if let Some(editor) = &mut self.source_editor {
                editor.inline_anchor = None;
                editor.inline_point = None;
                editor.inline_click = None;
                editor.inline_column = None;
                if editor.placement.is_some() {
                    self.cancel_source();
                }
            }
            return;
        }
        if response.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
        }
        let mut first_event = 0;
        let mut opened_existing = false;
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
                            modifiers,
                        } if !modifiers.alt => Some((index, *pos)),
                        _ => None,
                    })
            });
            let Some((index, press)) = press.filter(|(_, point)| {
                response.rect.contains(*point)
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
                opened_existing = true;
            } else {
                self.open_source(doc, "text");
            }
            let Some(editor) = self.source_editor.as_mut() else {
                return;
            };
            if !opened_existing {
                editor.x = anchor[0].floor() as i32;
                editor.y = anchor[1].floor() as i32;
                editor.placement = Some(Placement {
                    anchor,
                    dragged: false,
                    requires_drag: !self.view.contains(press, rect, scale),
                });
            }
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
        let edge = editor.frame_edge();
        let was_field_drag = editor.field_drag().is_some();
        let mut ended = !ui.input(|input| input.pointer.primary_down());
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
                if let Content::Text { layout, .. } = &mut editor.source.content {
                    if *layout != TextLayout::Paragraph {
                        *layout = TextLayout::Paragraph;
                        editor.error = None;
                        editor.apply_error = None;
                    }
                }
                let left = point[0].min(anchor[0]).floor();
                let top = point[1].min(anchor[1]).floor();
                let (x, y) = (left as i32, top as i32);
                if (editor.x, editor.y) != (x, y) {
                    editor.x = x;
                    editor.y = y;
                    editor.error = None;
                    editor.apply_error = None;
                }
                let width = ((point[0].max(anchor[0]).ceil() - left) as u32).clamp(1, edge);
                let height = ((point[1].max(anchor[1]).ceil() - top) as u32).clamp(1, edge);
                editor.set_frame([width, height], editor.source.matrix, editor.fit_text);
            }
            if released {
                ended = true;
                break;
            }
        }
        if !editor.previewing() {
            if ended {
                self.cancel_source();
            }
            return;
        }
        let placed_this_frame = opened_existing || (ended && editor.placement.is_some());
        if ended {
            editor.placement = None;
        }
        if editor.canvas_text && editor.placement.is_none() && !editor.pending
            && canvas_text_editor(ui, painter, editor, &self.view.projection(rect, scale), placed_this_frame)
        {
            self.cancel_source();
            ui.ctx().request_repaint();
            return;
        }
        if was_field_drag && editor.field_drag().is_none() {
            self.last_preview = None;
        }
        self.preview_text_field(doc);
        self.text_field_outline(painter, rect, scale);
    }
    fn preview_text_field(&mut self, doc: DocumentView<'_>) {
        let Some(editor) = &mut self.source_editor else { return };
        match editor.edit_command(doc) {
            Ok(EditCommand::Resident(command)) => self.transient = vec![command],
            Ok(EditCommand::Native(_)) => {
                self.native_source_preview = editor.owned_command();
            }
            Err(error) => {
                self.native_source_preview = None;
                editor.error = Some(error);
            }
        }
    }

    fn validate_source_draft(&mut self, ctx: &egui::Context, editor: &mut Editor) -> Option<String> {
        let invalid = editor.invalid();
        let pending = |error: &String| {
            *error == crate::fonts::FontError::Loading.to_string()
                || *error == crate::fonts::FontError::Busy.to_string()
        };
        if invalid.as_ref().is_some_and(pending) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }
        if invalid.is_none() {
            if editor.error.as_ref().is_some_and(pending) {
                editor.error = None;
                self.last_preview = None;
            }
            if editor.apply_error.as_ref().is_some_and(pending) {
                editor.apply_error = None;
            }
        }
        invalid
    }
    pub(super) fn source_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        if self.free_transform_frame || self.free_transform.is_some() {
            return;
        }
        let Some(mut e) = self.source_editor.take() else {
            return;
        };
        let stale = !e.matches_source(doc);
        let cancelled_drag = e.cancelled_frame_drag == Some(ctx.cumulative_pass_nr());
        if stale && !e.pending {
            e.error=Some("Image changed. This draft is retained; copy it or cancel and reopen the source before applying.".into());
            e.placement = None;
        }
        if e.placement.is_some() {
            self.source_editor = Some(e);
            return;
        }
        let canvas_text = e.canvas_text && self.tool == Tool::Text && !stale;
        if !canvas_text {
            prepare_widgets(ctx, &mut e);
        }
        let before = e.signature().ok();
        let mut open = !canvas_text;
        let apply_requested = std::mem::take(&mut e.apply_requested);
        let mut apply = canvas_text
            && !self.busy
            && !doc.read_only()
            && e.frame_drag.is_none()
            && !cancelled_drag
            && (apply_requested || ctx.input(|i| command_key(i, egui::Key::Enter)));
        let mut cancel = false;
        let mut invalid = None;
        let window = (!canvas_text).then(|| egui::Window::new(e.source.label()));
        let drawn = window.and_then(|window| window
            .id(egui::Id::new(("source-editor",e.token.nonce())))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(410.)
            .show(ctx, |ui| {
                if e.pending || e.frame_drag.is_some() {
                    ui.disable();
                }
                source_geometry(ui, &mut e);
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
                        path_closure(ui, closed, nodes.len() >= 2);
                        native_color(ui, "Fill", fill, doc.depth());
                        native_color(ui, "Stroke", stroke, doc.depth());
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
                                widgets::text(ui,svg,crate::svg::MARKUP_LIMIT,true,false,390.,e.native.as_mut());
                            });
                    }
                    Content::Text {
                        text,
                        layout,
                        line_height,
                        letter_spacing,
                        ..
                    } => {
                        widgets::text(ui,text,16384,false,false,390.,e.native.as_mut());
                        paragraph_toggle(ui, layout);
                        shaping_toggle(ui, layout);
                        ui.horizontal_wrapped(|ui| text_spacing(ui, letter_spacing, line_height, e.native.as_mut()));
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
                            ui.spacing_mut().item_spacing.x = 2.;
                            for (name, icon, label) in [
                                ("rectangle", Icon::Rectangle, "Rectangle"),
                                ("ellipse", Icon::Ellipse, "Ellipse"),
                                ("path", Icon::VectorPath, "Vector path"),
                            ] {
                                if icons::selectable(ui, icon, shape == name, label).clicked() {
                                    *shape = name.into();
                                }
                            }
                        });
                        if shape == "path" {
                            path_closure(ui, closed, true);
                            egui::ScrollArea::vertical()
                                .max_height(180.)
                                .show(ui, |ui| {
                                    let count = points.len();
                                    let mut remove = None;
                                    for (i, p) in points.iter_mut().enumerate() {
                                        ui.horizontal(|ui| {
                                            ui.label(format!("Point {}", i + 1));
                                            for (axis, coordinate) in ["X", "Y"].into_iter().zip(p) {
                                                let label = ui.label(axis);
                                                widgets::numeric(ui, coordinate, -100000. ..=100000., e.native.as_mut())
                                                    .labelled_by(label.id);
                                            }
                                            if ui.add_enabled_ui(count > 2, |ui| {
                                                icons::small_button(ui, Icon::Trash, &format!("Remove point {}", i + 1))
                                            }).inner.clicked() {
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
                        native_color(ui, "Fill", fill, doc.depth());
                        native_color(ui, "Stroke", stroke, doc.depth());
                        ui.horizontal(|ui| {
                            ui.label("Stroke width");
                            widgets::numeric(ui, stroke_width, 0. ..=256.,e.native.as_mut());
                            ui.label("px");
                        });
                    }
                }
                if matches!(e.source.content, Content::Text { .. }) {
                    ui.horizontal_wrapped(|ui| text_typography(ui, &mut e, doc.depth()));
                }
                invalid = self.validate_source_draft(ctx, &mut e);
                if let Some(error) = invalid
                    .as_ref()
                    .or(e.error.as_ref())
                    .or(e.apply_error.as_ref())
                {
                    ui.colored_label(ACCENT, error);
                }
                ui.horizontal(|ui| {
                    apply |= ui
                        .add_enabled(
                            !self.busy && !doc.read_only() && invalid.is_none() && e.error.is_none(),
                            egui::Button::new("Apply"),
                        )
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            })).and_then(|window| window.inner);
        if drawn.is_none() {
            invalid = self.validate_source_draft(ctx, &mut e);
        }
        let escape = ctx.input(|input| input.events.iter().any(|event| matches!(event,
            egui::Event::Key { key: egui::Key::Escape, pressed: true, repeat: false, .. }
        )));
        if !canvas_text {
            cancel |= !open;
        }
        if !stale && e.signature().ok() != before {
            e.error = None;
            e.apply_error = None;
            e.inline_column = None;
        }
        if e.pending {
            self.source_editor = Some(e);
            return;
        }
        let command = if stale {
            Err("Image changed during the source draft".into())
        } else if let Some(error) = invalid {
            Err(error)
        } else {
            e.edit_command(doc)
        };
        if apply {
            match command {
                Ok(command) => {
                    if let Some(focus) = ctx.memory(|memory| memory.focused()) {
                        ctx.memory_mut(|memory| memory.surrender_focus(focus));
                    }
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
        } else if cancel || (escape && !cancelled_drag) {
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
    sample: &mut font_sample::Preview,
    mut native: Option<&mut native::Native>,
) -> bool {
    use crate::fonts::{self, CatalogStatus, TextFont};
    let font_label = ui.label("Font");
    let label = font.label().into_owned();
    let short: String = label.chars().take(44).collect();
    let mut changed = false;
    let sample_owner = ui.id();
    let popup = egui::ComboBox::from_id_salt("text font")
        .selected_text(if short.len() < label.len() {
            format!("{short}…")
        } else {
            short
        })
        .width(210.)
        .truncate()
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show_ui(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
            let area = font_sample::Preview::area(ui);
            let mut hovered = None;
            let mut focused = None;
            let mut font_row = |ui: &mut egui::Ui, candidate: TextFont| {
                let response = ui.selectable_value(font, candidate.clone(), candidate.label());
                changed |= response.changed();
                if response.clicked() {
                    ui.memory_mut(|memory| memory.close_popup());
                }
                if response.hovered() {
                    hovered = Some(candidate);
                } else if response.has_focus() {
                    focused = Some(candidate);
                }
            };
            for name in ["regular", "medium", "semibold"] {
                font_row(ui, name.into());
            }
            widgets::text(ui, search, 512, false, true, 390., native.as_deref_mut())
                .on_hover_text("Search installed fonts by family or PostScript name");
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
                            font_row(ui, TextFont::Installed(value.clone()));
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
            let candidate = hovered.as_ref().or(focused.as_ref()).unwrap_or(font);
            if candidate.bundled_name().is_some() || matches!(catalog.status, CatalogStatus::Ready) {
                sample.show(ui, sample_owner, area, candidate, catalog.generation, candidate != font);
            } else {
                sample.close(sample_owner);
            }
            if ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                ui.memory_mut(|memory| memory.close_popup());
            }
        });
    if popup.inner.is_none() {
        sample.close(sample_owner);
    }
    popup.response.labelled_by(font_label.id).on_hover_text(label);
    changed
}

#[cfg(test)]
mod validation_tests {
    use super::*;
    use crate::fonts::{self, FontError, InstalledFont, TextFont};

    #[test]
    fn inline_text_click_tracks_glyphs_in_rotated_mirrored_canvas() {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        let doc = app.shared.lock().unwrap().resident_state().unwrap().document.clone();
        app.select_tool(Tool::Text);
        app.open_source(&doc, "text");
        let editor = app.source_editor.as_mut().unwrap();
        editor.x = 0;
        editor.y = 0;
        editor.source.width = 160;
        editor.source.height = 120;
        let Content::Text { text, align, .. } = &mut editor.source.content else { unreachable!() };
        *text = "AVé\nW".into();
        *align = "center".into();
        let view = view::View { angle: 24., mirror_h: true, mirror_v: false };
        let rect = Rect::from_center_size(egui::pos2(260., 170.), Vec2::new(160., 120.));
        let projection = view.projection(rect, 1.);
        let (x, [top, bottom]) = crate::source::text_position::TextCarets::bundled_legacy(&editor.source)
            .unwrap().unwrap().at(Caret { index: 2, trailing: false }).unwrap();
        let point = projection.point([x, (top + bottom) * 0.5]);
        let mut frame = |events| {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(520., 340.))),
                events,
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let painter = ui.painter().clone();
                    canvas_text_editor(ui, &painter, editor, &projection, false);
                });
            });
        };
        frame(vec![]);
        frame(vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() },
            egui::Event::PointerButton { pos: point, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
            egui::Event::Text("X".into()),
        ]);
        let Content::Text { text, .. } = &editor.source.content else { unreachable!() };
        assert_eq!(text, "AVXé\nW");
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
    }

    #[test]
    fn opening_drag_selects_shaped_text_through_release_outside_the_canvas() {
        let (mut app, ctx) = super::super::workspace_tests::fixture(16);
        let doc = app.shared.lock().unwrap().resident_state().unwrap().document.clone();
        app.select_tool(Tool::Text);
        app.open_source(&doc, "text");
        let editor = app.source_editor.as_mut().unwrap();
        editor.layer = Some("existing-text".into());
        editor.x = 0;
        editor.y = 0;
        editor.source.width = 340;
        editor.source.height = 120;
        let Content::Text { text, layout, .. } = &mut editor.source.content else { unreachable!() };
        *text = "Alpha Beta".into();
        *layout = TextLayout::Paragraph;
        let carets = TextCarets::resident(&editor.source).unwrap();
        let rect = Rect::from_min_size(egui::pos2(90., 90.), Vec2::new(340., 120.));
        let projection = view::View::default().projection(rect, 1.);
        let caret_point = |index| {
            let (x, [top, bottom]) = carets.at(Caret { index, trailing: false }).unwrap();
            projection.point([x, (top + bottom) * 0.5])
        };
        let start = caret_point(1);
        let end = egui::pos2(540., start.y);
        let frame = |editor: &mut Editor, events, opening| {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(520., 340.))),
                events,
                ..Default::default()
            }, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let painter = ui.painter().clone();
                    canvas_text_editor(ui, &painter, editor, &projection, opening);
                });
            });
        };
        let button = |pos, pressed| egui::Event::PointerButton {
            pos, pressed, button: egui::PointerButton::Primary, modifiers: Default::default(),
        };
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(520., 340.));
        let _ = ctx.run(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ctx| {
            egui::CentralPanel::default().show(ctx, |_| {});
        });
        frame(editor, vec![egui::Event::PointerMoved(start), button(start, true),
            egui::Event::PointerMoved(end), button(end, false)], true);
        editor.accept_resident_carets(editor.layout_key().unwrap(), Some(carets));
        frame(editor, vec![], false);
        frame(editor, vec![egui::Event::Text("X".into())], false);
        let Content::Text { text, .. } = &editor.source.content else { unreachable!() };
        assert_eq!(text, "AX");
        assert_eq!(app.shared.lock().unwrap().history_lengths(), [0, 0]);
    }

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
            frame_drag: None,
            cancelled_frame_drag: None,
            canvas_text: false,
            canvas_focus: None,
            canvas_input: None,
            focused_handle: None,
            typing_until: None,
            inline_anchor: None,
            inline_point: None,
            inline_click: None,
            inline_affinity: None,
            inline_column: None,
            layout: None,
            fit_text: false,
            apply_requested: false,
            show_field_geometry: false,
            apply_error: None,
            font_search: String::new(),
            font_sample: font_sample::Preview::default(),
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
