use super::*;
pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
    pub target: Option<String>,
    pub preview: String,
    pub gesture: String,
    pub(super) values: [f32; 6],
    merged: bool,
    layer: String,
}
impl Editor {
    pub(super) fn command(&self) -> Value {
        json!({"op":if self.target.is_some(){"mask.refine"}else{"selection.refine"},"layer":self.target.as_ref().unwrap_or(&self.layer),"document_id":self.document,"source_revision":self.revision,"smooth":self.values[0],"feather":self.values[1],"shift":self.values[2],"contrast":self.values[3],"edge":self.values[4],"edge_radius":self.values[5],"sample_merged":self.merged})
    }
}
impl PeerBrush {
    pub(super) fn segment_subject(&mut self, doc: &Document, region: bool) {
        self.cancel_refinement();
        self.animation.cancel();
        let shared = self.shared.clone();
        let params = json!({"actor":"human","document_id":doc.id,"expected_revision":doc.revision,"rect":if region {doc.selection}else{None},"mode":self.selection_mode,"feedback":"request"});
        self.job(move || {
            server::dispatch(&shared, "segment", &params)?;
            Ok("Subject selected · refine the edge from Select".into())
        });
    }
    pub(super) fn cancel_refinement(&mut self) {
        self.refinement = None;
        self.transient
            .retain(|c| c["op"] != "selection.refine" && c["op"] != "mask.refine");
        self.last_preview = None;
    }
    pub(super) fn open_refinement(&mut self, doc: &Document, target: Option<String>) {
        self.cancel_filters();
        self.points.clear();
        self.drag_start = None;
        self.gizmo_handle = 0;
        self.transient.clear();
        self.animation.cancel();
        self.isolate = false;
        self.refinement = Some(Editor {
            document: doc.id.clone(),
            revision: doc.revision,
            target,
            preview: "cutout".into(),
            gesture: crate::engine::id(),
            values: [0., 0., 0., 0., 0., 8.],
            merged: true,
            layer: self.selected.clone(),
        });
    }
    pub(super) fn refinement_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(mut editor) = self.refinement.take() else {
            return;
        };
        if editor.document != doc.id || editor.revision != doc.revision {
            self.cancel_refinement();
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new(if editor.target.is_some() {
            "Refine layer mask"
        } else {
            "Refine selection"
        })
        .open(&mut open)
        .collapsible(false)
        .default_width(270.)
        .show(ctx, |ui| {
            for (i, label, max, suffix) in [
                (0, "Smooth", 32., " px"),
                (1, "Feather", 64., " px"),
                (2, "Shift edge", 32., " px"),
                (3, "Contrast", 100., "%"),
                (4, "Follow image edge", 100., "%"),
                (5, "Edge radius", 32., " px"),
            ] {
                ui.horizontal(|ui| {
                    ui.add_sized([112., 20.], egui::Label::new(label));
                    controls::range(
                        ui,
                        format!("refine-{i}"),
                        &mut editor.values[i],
                        if i == 2 {
                            -32.
                        } else if i == 5 {
                            1.
                        } else {
                            0.
                        }..=max,
                        130.,
                        suffix,
                        0,
                        false,
                    );
                });
            }
            if editor.target.is_none() {
                ui.checkbox(&mut editor.merged, "Use merged colors");
            }
            ui.horizontal(|ui| {
                ui.label("Preview");
                egui::ComboBox::from_id_salt("refinement preview")
                    .selected_text(&editor.preview)
                    .show_ui(ui, |ui| {
                        for mode in if editor.target.is_some() {
                            vec!["cutout", "mask"]
                        } else {
                            vec!["cutout", "mask", "overlay"]
                        } {
                            ui.selectable_value(&mut editor.preview, mode.into(), mode);
                        }
                    });
            });
            ui.horizontal(|ui| {
                apply = ui
                    .add_enabled(!self.busy, egui::Button::new("Apply"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        let command = editor.command();
        if apply {
            self.cancel_refinement();
            let shared = self.shared.clone();
            self.job(move||{server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[command],"label":"Refine edge","feedback":"request"}))?;Ok("Edge refined · one undo step".into())});
        } else if !open || cancel {
            self.cancel_refinement();
        } else {
            self.transient = vec![command];
            self.refinement = Some(editor);
        }
    }
}
