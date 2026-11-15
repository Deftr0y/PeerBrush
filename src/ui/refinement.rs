use super::*;

pub(super) enum PreviewProjection<'a> {
    Selection(&'a str),
    LayerMask(&'a str),
}

pub(super) fn preview_projection<'a>(
    editor: Option<&'a Editor>,
    parameter: Option<&'a ParameterEdit>,
    selection_drag: bool,
) -> Option<PreviewProjection<'a>> {
    if let Some(editor) = editor {
        return match editor.target.as_deref() {
            None => Some(PreviewProjection::Selection(&editor.preview)),
            Some(layer) if editor.preview == "mask" => Some(PreviewProjection::LayerMask(layer)),
            Some(_) => None,
        };
    }
    if parameter.is_some_and(|edit| {
        edit.commands.iter().any(|command| command["op"] == "selection.wand_update")
    }) {
        Some(PreviewProjection::Selection("cutout"))
    } else {
        selection_drag.then_some(PreviewProjection::Selection("overlay"))
    }
}

pub(super) struct Editor {
    pub document: String,
    pub revision: u64,
    pub target: Option<String>,
    pub preview: String,
    pub gesture: String,
    pub(super) values: [f32; 6],
    pub error: Option<String>,
    apply_error: Option<String>,
    pending: bool,
    preview_drag: Option<egui::Id>,
    merged: bool,
    layer: String,
}
impl Editor {
    pub(super) fn command(&self) -> Value {
        json!({"op":if self.target.is_some(){"mask.refine"}else{"selection.refine"},"layer":self.target.as_ref().unwrap_or(&self.layer),"document_id":self.document,"source_revision":self.revision,"smooth":self.values[0],"feather":self.values[1],"shift":self.values[2],"contrast":self.values[3],"edge":self.values[4],"edge_radius":self.values[5],"sample_merged":self.merged})
    }

    pub(super) fn pointer_preview_key(
        &self,
        ctx: &egui::Context,
        doc: DocumentView<'_>,
    ) -> Option<(u64, &str)> {
        let control = self.preview_drag.filter(|&id| ctx.dragged_id() == Some(id))?;
        (!self.pending && self.document == doc.id() && self.revision == doc.revision()
            && !PeerBrush::pointer_contact_cancelled(ctx)
            && ctx.input(|input| input.focused && input.pointer.primary_down()
                && !input.key_pressed(egui::Key::Escape)
                && !input.events.iter().any(|event| matches!(event, egui::Event::WindowFocused(false)))))
            .then_some((control.value(), self.gesture.as_str()))
    }
}
impl PeerBrush {
    pub(super) fn segment_subject<'a>(&mut self, doc: impl Into<DocumentView<'a>>, region: bool) {
        let doc = doc.into();
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; refinement draft preserved".into();
            return;
        }
        self.cancel_refinement();
        self.interrupt_animation();
        let shared = self.shared.clone();
        let params = json!({"actor":"human","document_id":doc.id(),"expected_revision":doc.revision(),"rect":if region {doc.selection()}else{None},"mode":self.selection_mode,"feedback":"request"});
        self.job(move || {
            server::dispatch(&shared, "segment", &params)?;
            Ok("Subject selected · refine the edge from Select".into())
        });
    }
    pub(super) fn refinement_pending(&self) -> bool {
        self.refinement
            .as_ref()
            .is_some_and(|editor| editor.pending)
    }
    fn apply_refinement(&mut self) {
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; refinement draft preserved".into();
            return;
        }
        let Some(editor) = self.refinement.as_mut() else {
            return;
        };
        let command = editor.command();
        let document = editor.document.clone();
        let revision = editor.revision;
        let token = egui::Id::new(&editor.gesture);
        editor.apply_error = None;
        editor.pending = true;
        self.apply_action(
            vec![command],
            "Refine edge",
            Some((&document, revision)),
            edit_job::Origin::Refinement(token),
        );
    }
    pub(super) fn refinement_finished(&mut self, token: egui::Id, result: Result<(), String>) {
        let Some(editor) = self
            .refinement
            .as_mut()
            .filter(|editor| editor.pending && egui::Id::new(&editor.gesture) == token)
        else {
            return;
        };
        editor.pending = false;
        match result {
            Ok(()) => self.cancel_refinement(),
            Err(error) => editor.apply_error = Some(error),
        }
    }
    pub(super) fn cancel_refinement(&mut self) {
        if self.refinement_pending() {
            self.cancel_pending_edit();
            return;
        }
        self.refinement = None;
        self.transient
            .retain(|c| c["op"] != "selection.refine" && c["op"] != "mask.refine");
        self.last_preview = None;
    }
    pub(super) fn open_refinement<'a>(
        &mut self,
        doc: impl Into<DocumentView<'a>>,
        target: Option<String>,
    ) {
        let doc = doc.into();
        if self.edit_worker.pending() {
            self.message = "An edit is still being applied; refinement draft preserved".into();
            return;
        }
        self.cancel_filters();
        self.points.clear();
        self.drag_start = None;
        self.gizmo_handle = 0;
        self.transient.clear();
        self.interrupt_animation();
        self.isolate = false;
        self.refinement = Some(Editor {
            document: doc.id().to_owned(),
            revision: doc.revision(),
            target,
            preview: "cutout".into(),
            gesture: crate::engine::id(),
            values: [0., 0., 0., 0., 0., 8.],
            error: None,
            apply_error: None,
            pending: false,
            preview_drag: None,
            merged: true,
            layer: self.selected.clone(),
        });
    }
    pub(super) fn refinement_window<'a>(
        &mut self,
        ctx: &egui::Context,
        doc: impl Into<DocumentView<'a>>,
    ) {
        let doc = doc.into();
        if self.refinement_pending() {
            egui::Window::new("Applying refinement")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label("Applying edge refinement…");
                    if ui.button("Cancel apply").clicked() {
                        self.cancel_pending_edit();
                    }
                });
            return;
        }
        let Some(mut editor) = self.refinement.take() else {
            return;
        };
        if editor.document != doc.id() || editor.revision != doc.revision() {
            self.cancel_refinement();
            return;
        }
        let before = editor.command();
        let preview_before = editor.preview.clone();
        let was_dragging = editor.preview_drag.is_some();
        if editor.pointer_preview_key(ctx, doc).is_none() {
            editor.preview_drag = None;
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
            for (i, name, min, max, suffix, help) in [
                (0, "Smooth", 0., 32., " px", "Round jagged edges and small irregularities."),
                (1, "Feather", 0., 64., " px", "Soften the transition at the edge."),
                (2, "Shift edge", -32., 32., " px", "Move left to contract the edge or right to expand it."),
                (3, "Contrast", 0., 100., "%", "Make the edge transition sharper."),
                (4, "Follow image edge", 0., 100., "%", "Guide the edge using the sampled artwork."),
                (5, "Edge radius", 1., 32., " px", "Set how far to sample around the edge when following the artwork."),
            ] {
                ui.horizontal(|ui| {
                    let label = ui.add_sized([112., 20.], egui::Label::new(name)).on_hover_text(help);
                    let id = format!("refine-{i}");
                    let value = &mut editor.values[i];
                    let response = if i == 2 {
                        controls::centered_range(ui, id, value, max, 130., suffix)
                    } else {
                        controls::range(ui, id, value, min..=max, 130., suffix, 0, false)
                    }.labelled_by(label.id).on_hover_text(help);
                    if response.changed() && response.dragged_by(egui::PointerButton::Primary) {
                        editor.preview_drag = ctx.dragged_id();
                    }
                });
            }
            if editor.target.is_none() {
                controls::layer_sampling(
                    ui,
                    &mut editor.merged,
                    "Sample active layer",
                    "Follow image edges from the layer active when refinement opened.",
                    "Follow image edges from the visible rendered composite.",
                );
            }
            ui.horizontal(|ui| {
                ui.label("Preview");
                for (mode, icon, label) in [
                    ("cutout", Icon::Cutout, "Show the cutout on transparency"),
                    ("mask", Icon::Mask, "Show selection coverage as a grayscale mask"),
                    ("overlay", Icon::SelectionOverlay, "Show selection coverage over the artwork"),
                ] {
                    if (mode != "overlay" || editor.target.is_none())
                        && icons::selectable(ui, icon, editor.preview == mode, label).clicked()
                    {
                        editor.preview = mode.into();
                    }
                }
            });
            if let Some(error) = editor.error.as_ref().or(editor.apply_error.as_ref()) {
                ui.colored_label(ACCENT, error);
            }
            ui.horizontal(|ui| {
                apply = ui
                    .add_enabled(!self.busy, egui::Button::new("Apply"))
                    .clicked();
                cancel = ui.button("Cancel").clicked();
            });
        });
        let command = editor.command();
        if command != before {
            editor.error = None;
            editor.apply_error = None;
        }
        if command != before || preview_before != editor.preview || (was_dragging && editor.preview_drag.is_none()) {
            ctx.request_repaint();
        }
        if apply {
            self.refinement = Some(editor);
            self.apply_refinement();
        } else if !open || cancel {
            self.cancel_refinement();
        } else {
            self.transient = vec![command];
            self.refinement = Some(editor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{text_tests, workspace_tests as workspace};
    use super::*;

    #[test]
    fn selection_and_mask_refinement_preserve_drafts_and_native_pixels_on_refusal() {
        for mask in [false, true] {
            for outcome in ["immediate", "commit", "cancel", "reserved", "replacement"] {
                let (mut app, ctx) = workspace::fixture(16);
                let layer = app.selected.clone();
                let setup = if mask {
                    json!({"op":"mask.add","layer":layer,"value":128})
                } else {
                    json!({"op":"selection","kind":"rectangle","rect":[4,4,20,18]})
                };
                app.edit(vec![setup], "Refinement source");
                let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                app.open_refinement(&doc, mask.then(|| layer.clone()));
                let editor = app.refinement.as_mut().unwrap();
                editor.values[1] = 2.;
                let command = editor.command();
                let token = egui::Id::new(&editor.gesture);
                let expected =
                    crate::engine::Engine::preview_edits(doc.clone(), &[command.clone()]).unwrap();
                let shared = app.shared.clone();
                if outcome == "reserved" {
                    shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "agent",
                            "Protected refinement",
                            vec![crate::engine::Scope::layer(&layer)],
                        )
                        .unwrap();
                }
                let mut guard = shared.lock().unwrap();
                if outcome == "replacement" {
                    guard.resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
                }
                let before = serde_json::to_value(&guard.resident_state().expect("resident fixture").document).unwrap();
                let history = guard.history_lengths();
                app.selection_commit_failed = false;
                if outcome == "immediate" {
                    drop(guard);
                    app.apply_refinement();
                } else {
                    app.refinement.as_mut().unwrap().pending = true;
                    app.submit_edit_job(
                        edit_job::Request {
                            project: app.project_id.clone(),
                            document: doc.id.clone(),
                            revision: doc.revision,
                            label: "Refine edge".into(),
                            selection_edit: !mask,
                            origin: edit_job::Origin::Refinement(token),
                        },
                        edit_job::Commands::resident(vec![command.clone()]),
                        &ctx,
                    )
                    .unwrap();
                    app.refinement_finished(egui::Id::new("other refinement"), Ok(()));
                    app.open_refinement(&doc, None);
                    app.segment_subject(&doc, false);
                    app.apply_refinement();
                    assert!(app.refinement_pending());
                    assert_eq!(app.refinement.as_ref().unwrap().command(), command);
                    if outcome == "cancel" {
                        app.cancel_refinement();
                        assert!(app.refinement_pending());
                    }
                    drop(guard);
                    text_tests::wait_edit(&mut app);
                }
                let engine = shared.lock().unwrap();
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels, doc.layers[0].pixels);
                if matches!(outcome, "immediate" | "commit") {
                    assert!(app.refinement.is_none());
                    assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
                    if mask {
                        let actual = engine.resident_state().expect("resident fixture").document.layers[0].mask.as_ref().unwrap();
                        let original = doc.layers[0].mask.as_ref().unwrap();
                        let expected = expected.layers[0].mask.as_ref().unwrap();
                        assert_eq!(actual.steps.len(), original.steps.len() + 1);
                        for (actual, original) in actual.steps.iter().zip(&original.steps) {
                            assert_eq!(actual.id, original.id);
                            assert_eq!(actual.pixels, original.pixels);
                        }
                        assert_eq!(
                            actual.steps.last().unwrap().pixels,
                            expected.steps.last().unwrap().pixels
                        );
                        assert_eq!(actual.steps.last().unwrap().pixels.depth, 16);
                    } else {
                        assert_eq!(engine.resident_state().expect("resident fixture").document.selection, expected.selection);
                        assert_eq!(
                            engine
                                .resident_state().expect("resident fixture").document
                                .selection_coverage
                                .as_ref()
                                .map(|c| (&c.mask, c.origin, c.bounds)),
                            expected
                                .selection_coverage
                                .as_ref()
                                .map(|c| (&c.mask, c.origin, c.bounds))
                        );
                    }
                    assert!(!app.selection_commit_failed);
                } else {
                    let editor = app.refinement.as_ref().unwrap();
                    assert!(!editor.pending);
                    assert!(editor.apply_error.is_some());
                    assert_eq!(editor.command(), command);
                    assert_eq!(engine.history_lengths(), history);
                    assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
                    if outcome != "replacement" {
                        assert_eq!(app.selection_commit_failed, !mask);
                    }
                    assert!(editor.error.is_none());
                    drop(engine);
                    if outcome == "cancel" {
                        app.apply_refinement();
                        assert!(
                            app.refinement.is_none(),
                            "An unchanged cancelled draft can be retried"
                        );
                        assert_eq!(
                            shared.lock().unwrap().history_lengths(),
                            [history[0] + 1, 0]
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn pending_editor_remains_visible_through_lifecycle_gate() {
        let (mut app, ctx) = workspace::fixture(16);
        let doc = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.open_refinement(&doc, None);
        let editor = app.refinement.as_mut().unwrap();
        editor.pending = true;
        let token = egui::Id::new(&editor.gesture);
        let (release, held) = mpsc::channel();
        app.edit_worker
            .submit(
                edit_job::Request {
                    project: app.project_id.clone(),
                    document: doc.id.clone(),
                    revision: doc.revision,
                    label: "Apply editor".into(),
                    selection_edit: false,
                    origin: edit_job::Origin::Refinement(token),
                },
                edit_job::Commands::resident(vec![]),
                move |_, _| {
                    held.recv().unwrap();
                    Err("Apply cancelled".into())
                },
                || {},
            )
            .unwrap();
        let output = workspace::draw(&mut app, &ctx, Vec2::new(1360., 900.), vec![]);
        workspace::label(&output, "Applying edge refinement…");
        workspace::label(&output, "Cancel apply");
        assert!(app.refinement_pending());
        release.send(()).unwrap();
        text_tests::wait_edit(&mut app);
        assert!(app.refinement.is_some());
        assert!(!app.refinement_pending());
    }
}
