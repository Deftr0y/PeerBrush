use super::*;
pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
    pub mode: &'static str,
    pub width: u32,
    pub height: u32,
    pub rect: [i32; 4],
    pub anchor: String,
    pub proportional: bool,
    original: [u32; 2],
    pub error: Option<String>,
}
impl Editor {
    pub(super) fn command(&self) -> Value {
        json!({"op":self.mode,"width":self.width,"height":self.height,"rect":if self.mode=="crop"{Some(self.rect)}else{None},"anchor":self.anchor,"document_id":self.document,"source_revision":self.revision})
    }
    pub(super) fn size(&self) -> [u32; 2] {
        if self.mode == "crop" {
            [
                (self.rect[2] - self.rect[0]).max(1) as u32,
                (self.rect[3] - self.rect[1]).max(1) as u32,
            ]
        } else {
            [self.width, self.height]
        }
    }
}
impl PeerBrush {
    pub(super) fn open_geometry(&mut self, doc: &Document, mode: &'static str) {
        self.cancel_filters();
        self.cancel_source();
        self.cancel_refinement();
        self.points.clear();
        self.drag_start = None;
        self.gizmo_handle = 0;
        self.transient.clear();
        self.animation.cancel();
        self.isolate = false;
        self.geometry = Some(Editor {
            document: doc.id.clone(),
            revision: doc.revision,
            mode,
            width: doc.width,
            height: doc.height,
            rect: doc
                .selection
                .filter(|b| b[0] < b[2] && b[1] < b[3])
                .unwrap_or([0, 0, doc.width as i32, doc.height as i32]),
            anchor: "center".into(),
            proportional: true,
            original: [doc.width, doc.height],
            error: None,
        });
        self.last_preview = None;
    }
    pub(super) fn cancel_geometry(&mut self) {
        self.geometry = None;
        self.transient.retain(|c| {
            !matches!(
                c["op"].as_str(),
                Some("crop" | "canvas.resize" | "image.resize")
            )
        });
        self.last_preview = None;
    }
    pub(super) fn geometry_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(mut editor) = self.geometry.take() else {
            return;
        };
        if editor.document != doc.id || editor.revision != doc.revision {
            self.cancel_geometry();
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        let before = editor.command();
        egui::Window::new(match editor.mode {
            "crop" => "Crop",
            "canvas.resize" => "Canvas size",
            _ => "Image size",
        })
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .default_width(295.)
        .show(ctx, |ui| {
            if editor.mode == "crop" {
                for (i, label) in ["Left", "Top", "Right", "Bottom"].iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized([70., 20.], egui::Label::new(*label));
                        controls::numeric(ui, &mut editor.rect[i], -100000..=100000);
                        ui.label("px");
                    });
                }
                if ui
                    .add_enabled(
                        doc.selection.is_some(),
                        egui::Button::new("Use selection bounds"),
                    )
                    .clicked()
                {
                    editor.rect = doc.selection.unwrap();
                }
                ui.label("Artwork outside the crop is retained.");
            } else {
                ui.horizontal(|ui| {
                    ui.label("Width");
                    if controls::numeric(ui, &mut editor.width, 1..=8192).changed()
                        && editor.proportional
                        && editor.mode == "image.resize"
                    {
                        editor.height = (editor.width as f64 * editor.original[1] as f64
                            / editor.original[0] as f64)
                            .round()
                            .clamp(1., 8192.) as u32;
                    }
                    ui.label("px");
                });
                ui.horizontal(|ui| {
                    ui.label("Height");
                    if controls::numeric(ui, &mut editor.height, 1..=8192).changed()
                        && editor.proportional
                        && editor.mode == "image.resize"
                    {
                        editor.width = (editor.height as f64 * editor.original[0] as f64
                            / editor.original[1] as f64)
                            .round()
                            .clamp(1., 8192.) as u32;
                    }
                    ui.label("px");
                });
                if editor.mode == "canvas.resize" {
                    ui.label("Anchor");
                    egui::Grid::new("canvas anchors")
                        .spacing(Vec2::new(5., 3.))
                        .show(ui, |ui| {
                            for (y, row) in [
                                ["top_left", "top", "top_right"],
                                ["left", "center", "right"],
                                ["bottom_left", "bottom", "bottom_right"],
                            ]
                            .iter()
                            .enumerate()
                            {
                                for (x, name) in row.iter().enumerate() {
                                    let (rect, response) = ui.allocate_exact_size(
                                        Vec2::splat(24.),
                                        egui::Sense::click(),
                                    );
                                    if response.clicked() {
                                        editor.anchor = (*name).into();
                                    }
                                    let selected = editor.anchor == *name;
                                    let color = if selected {
                                        ACCENT
                                    } else {
                                        ui.visuals().text_color()
                                    };
                                    if selected || response.hovered() {
                                        ui.painter().rect_filled(
                                            rect,
                                            3.,
                                            ui.visuals()
                                                .selection
                                                .bg_fill
                                                .gamma_multiply(if selected { 0.6 } else { 0.25 }),
                                        );
                                    }
                                    let center = rect.center();
                                    if x == 1 && y == 1 {
                                        ui.painter().circle_filled(center, 2.5, color);
                                    } else {
                                        let direction =
                                            Vec2::new(x as f32 - 1., y as f32 - 1.).normalized();
                                        let tip = center + direction * 6.;
                                        let side = Vec2::new(-direction.y, direction.x) * 3.;
                                        let stroke = egui::Stroke::new(1.5_f32, color);
                                        ui.painter()
                                            .line_segment([center - direction * 5., tip], stroke);
                                        ui.painter().line_segment(
                                            [tip - direction * 4. + side, tip],
                                            stroke,
                                        );
                                        ui.painter().line_segment(
                                            [tip - direction * 4. - side, tip],
                                            stroke,
                                        );
                                    }
                                    response.on_hover_text(name.replace('_', " "));
                                }
                                ui.end_row();
                            }
                        });
                    ui.label("Moves artwork without resampling pixels.");
                } else {
                    ui.checkbox(&mut editor.proportional, "Keep aspect ratio");
                    ui.label("Resamples all layers and masks at their original depth.");
                }
            }
            let [w, h] = editor.size();
            ui.label(format!("Result {w} × {h} · {} bit", doc.bit_depth));
            let invalid = crate::raster::check_size(w, h).err().or_else(|| {
                (editor.mode == "crop"
                    && (editor.rect[0] >= editor.rect[2] || editor.rect[1] >= editor.rect[3]))
                    .then(|| "Choose a positive crop rectangle".into())
            });
            if let Some(error) = invalid.as_ref().or(editor.error.as_ref()) {
                ui.colored_label(ACCENT, error);
            }
            ui.horizontal(|ui| {
                apply = ui
                    .add_enabled(
                        !self.busy && invalid.is_none() && editor.error.is_none(),
                        egui::Button::new("Apply"),
                    )
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        let command = editor.command();
        if command != before {
            editor.error = None;
        }
        if apply {
            self.cancel_geometry();
            self.edit(
                vec![command],
                match editor.mode {
                    "crop" => "Crop canvas",
                    "canvas.resize" => "Resize canvas",
                    _ => "Resize image",
                },
            );
            self.frame_pending = true;
        } else if !open || cancel || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_geometry();
        } else {
            self.transient = vec![command];
            self.geometry = Some(editor);
        }
    }
}
