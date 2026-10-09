//! Native close/exit review. Decisions are tied to exact project sources.
use super::*;
use crate::workspace::lifecycle::Reviewed;

#[derive(Clone, Debug, PartialEq)]
pub(super) enum Intent {
    Close(String),
    Exit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use std::sync::Mutex;

    fn fixture(depth: u16) -> (PeerBrush, egui::Context, Shared, Shared) {
        let ctx = egui::Context::default();
        let mut e = Engine::new();
        e.doc = Document::new_depth(64, 64, depth).unwrap();
        let root = Arc::new(Mutex::new(e));
        crate::workspace::attach(&root);
        let second = crate::workspace::new_project(&root, 64, 64, depth, None).unwrap();
        let state_dir =
            std::env::temp_dir().join(format!("peerbrush-lifecycle-ui-{}", crate::engine::id()));
        std::fs::create_dir_all(&state_dir).unwrap();
        let app = PeerBrush::init(
            &ctx,
            root.clone(),
            Connection {
                port: 0,
                token: String::new(),
                state_dir,
                instance_lock: None,
            },
        );
        (app, ctx, root, second)
    }
    fn frame(
        app: &mut PeerBrush,
        ctx: &egui::Context,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                events,
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        )
    }
    fn dirty(shared: &Shared, name: &str) {
        let mut e = shared.lock().unwrap();
        let layer = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[json!({"op":"layer.update","layer":layer,"name":name})],
            None,
            None,
            name,
        )
        .unwrap();
    }
    fn click(app: &mut PeerBrush, ctx: &egui::Context, index: usize) {
        // A new centered modal measures its contents before settling its position.
        for _ in 0..3 {
            frame(app, ctx, vec![]);
        }
        let pos = app.lifecycle.as_ref().unwrap().buttons[index]
            .unwrap()
            .center();
        for pressed in [true, false] {
            frame(
                app,
                ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
    }
    fn waiting(app: &mut PeerBrush, ctx: &egui::Context) {
        for _ in 0..100 {
            frame(app, ctx, vec![]);
            if app.lifecycle.as_ref().is_none_or(|r| r.saving.is_none()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("Save reply did not arrive");
    }

    #[test]
    fn cancel_after_an_exit_discard_decision_keeps_every_native_project_and_history() {
        for depth in [8, 16] {
            let (mut app, ctx, root, second) = fixture(depth);
            dirty(&root, "First unsaved");
            dirty(&second, "Second unsaved");
            frame(&mut app, &ctx, vec![]);
            let before = [
                root.lock().unwrap().doc.export_png().unwrap(),
                second.lock().unwrap().doc.export_png().unwrap(),
            ];
            app.request_close(Intent::Exit);
            click(&mut app, &ctx, 1);
            assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
            click(&mut app, &ctx, 2);
            assert!(app.lifecycle.is_none());
            assert!(!app.exit_approved);
            for (shared, bytes) in [&root, &second].into_iter().zip(before) {
                let e = shared.lock().unwrap();
                assert!(!e.closed);
                assert_eq!(e.doc.bit_depth, depth);
                assert_eq!(e.doc.export_png().unwrap(), bytes);
                assert_eq!(e.undo.len(), 1);
                assert_eq!(e.saved_revision, 0);
            }
        }
    }
    #[test]
    fn escape_and_modal_input_do_not_duplicate_delete_group_or_paint_under_the_review() {
        let (mut app, ctx, root, _) = fixture(16);
        dirty(&root, "Keep artwork");
        frame(&mut app, &ctx, vec![]);
        let before = root.lock().unwrap().doc.clone();
        app.request_close(Intent::Close(app.project_id.clone()));
        frame(&mut app, &ctx, vec![]);
        let commands = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        for key in [egui::Key::Delete, egui::Key::G, egui::Key::D, egui::Key::V] {
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key,
                    physical_key: Some(key),
                    pressed: true,
                    repeat: false,
                    modifiers: commands,
                }],
            );
        }
        let point = app.view_rect.unwrap().left_top() + Vec2::splat(8.);
        for pressed in [true, false] {
            frame(
                &mut app,
                &ctx,
                vec![
                    egui::Event::PointerMoved(point),
                    egui::Event::PointerButton {
                        pos: point,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: Default::default(),
                    },
                ],
            );
        }
        assert_eq!(
            root.lock().unwrap().doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
        assert_eq!(root.lock().unwrap().doc.layers.len(), before.layers.len());
        assert_eq!(root.lock().unwrap().undo.len(), 1);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(app.lifecycle.is_none());
        assert!(!root.lock().unwrap().closed);
    }
    #[test]
    fn newer_edits_invalidate_an_earlier_dont_save_and_are_reviewed_again() {
        let (mut app, ctx, root, second) = fixture(16);
        dirty(&root, "Old work");
        dirty(&second, "Other work");
        frame(&mut app, &ctx, vec![]);
        app.request_close(Intent::Exit);
        click(&mut app, &ctx, 1);
        dirty(&root, "New human work");
        frame(&mut app, &ctx, vec![]);
        let review = app.lifecycle.as_ref().unwrap();
        let row = review.current().unwrap();
        assert_eq!(row.source.project, root.lock().unwrap().project_id);
        assert!(!row.source.discard);
        assert_eq!(row.source.revision, 2);
        assert!(review.error.as_ref().unwrap().contains("changed"));
        assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
        click(&mut app, &ctx, 2);
    }
    #[test]
    fn canceled_failed_and_delayed_save_replies_never_close_current_or_other_work() {
        for outcome in [Ok(false), Err("Disk write failed".into()), Ok(true)] {
            let (mut app, ctx, root, second) = fixture(16);
            dirty(&root, "Unsaved");
            frame(&mut app, &ctx, vec![]);
            app.request_close(Intent::Close(app.project_id.clone()));
            frame(&mut app, &ctx, vec![]);
            let source = app
                .lifecycle
                .as_ref()
                .unwrap()
                .current()
                .unwrap()
                .source
                .clone();
            let (tx, rx) = mpsc::channel();
            app.lifecycle.as_mut().unwrap().saving = Some(rx);
            if outcome == Ok(true) {
                dirty(&root, "Newer than the saved snapshot");
            }
            tx.send(SaveReply {
                source,
                result: outcome,
            })
            .unwrap();
            frame(&mut app, &ctx, vec![]);
            assert!(app.lifecycle.as_ref().unwrap().current().is_some());
            assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
            assert!(app.lifecycle.as_ref().unwrap().error.is_some());
            click(&mut app, &ctx, 2);
        }
    }
    #[test]
    fn successful_async_save_closes_its_original_tab_and_preserves_other_native_work() {
        let (mut app, ctx, root, second) = fixture(16);
        root.lock().unwrap().doc.layers[0]
            .pixels
            .set16(2, 3, [10001, 30003, 50007, 65535]);
        dirty(&root, "Save this source");
        dirty(&second, "Keep other work");
        let path = app.connection.state_dir.join("source.psd");
        root.lock().unwrap().path = Some(path.clone());
        let project = root.lock().unwrap().project_id.clone();
        let other = second.lock().unwrap().project_id.clone();
        crate::workspace::select_in(&app.workspace, &other).unwrap();
        app.switch_project(&other);
        frame(&mut app, &ctx, vec![]);
        app.request_close(Intent::Close(project));
        click(&mut app, &ctx, 0);
        assert!(app.lifecycle.as_ref().unwrap().saving.is_some());
        waiting(&mut app, &ctx);
        assert!(app.lifecycle.is_none());
        assert!(root.lock().unwrap().closed);
        assert!(!second.lock().unwrap().closed);
        assert_eq!(second.lock().unwrap().doc.layers[0].name, "Keep other work");
        assert_eq!(second.lock().unwrap().undo.len(), 1);
        assert_eq!(app.project_id, other);
        let saved = crate::psd::decode(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(saved.bit_depth, 16);
        assert_eq!(
            saved.layers[0].pixels.get16(2, 3),
            [10001, 30003, 50007, 65535]
        );
    }
    #[test]
    fn failed_actual_save_and_unapplied_tool_edits_preserve_the_project() {
        let (mut app, ctx, root, _) = fixture(16);
        dirty(&root, "Keep work");
        frame(&mut app, &ctx, vec![]);
        let doc = root.lock().unwrap().doc.clone();
        app.open_source(&doc, "rectangle");
        app.request_close(Intent::Close(app.project_id.clone()));
        assert!(app.lifecycle.is_none());
        assert!(app.source_editor.is_some());
        app.source_editor = None;
        let blocked = app.connection.state_dir.join("directory.psd");
        std::fs::create_dir(&blocked).unwrap();
        root.lock().unwrap().path = Some(blocked);
        app.request_close(Intent::Close(app.project_id.clone()));
        click(&mut app, &ctx, 0);
        assert!(app.lifecycle.as_ref().unwrap().saving.is_some());
        waiting(&mut app, &ctx);
        assert!(app.lifecycle.as_ref().unwrap().error.is_some());
        assert!(!root.lock().unwrap().closed);
        assert_eq!(root.lock().unwrap().saved_revision, 0);
        assert_eq!(root.lock().unwrap().undo.len(), 1);
        click(&mut app, &ctx, 2);
    }
    #[test]
    fn native_review_requests_show_the_original_project_and_reject_stale_requests() {
        let (mut app, ctx, root, second) = fixture(16);
        dirty(&root, "Requested close");
        let source = {
            let e = root.lock().unwrap();
            (e.project_id.clone(), e.doc.id.clone(), e.doc.revision)
        };
        crate::workspace::lifecycle::request_close(&root, &source.0, &source.1, source.2, "human")
            .unwrap();
        let other = second.lock().unwrap().project_id.clone();
        crate::workspace::select_in(&app.workspace, &other).unwrap();
        app.switch_project(&other);
        frame(&mut app, &ctx, vec![]);
        assert_eq!(
            app.lifecycle
                .as_ref()
                .unwrap()
                .current()
                .unwrap()
                .source
                .project,
            source.0
        );
        assert!(!root.lock().unwrap().closed);
        click(&mut app, &ctx, 2);
        crate::workspace::lifecycle::request_close(&root, &source.0, &source.1, source.2, "human")
            .unwrap();
        dirty(&root, "New human work");
        frame(&mut app, &ctx, vec![]);
        assert!(app.lifecycle.is_none());
        assert!(!root.lock().unwrap().closed);
        assert_eq!(root.lock().unwrap().doc.layers[0].name, "New human work");
        assert_eq!(app.project_id, other);
    }
    #[test]
    fn native_close_request_is_canceled_until_all_dirty_projects_are_reviewed() {
        let (mut app, ctx, root, second) = fixture(16);
        dirty(&root, "One");
        dirty(&second, "Two");
        frame(&mut app, &ctx, vec![]);
        let mut raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
            ..Default::default()
        };
        raw.viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
        let output = ctx.run(raw, |ctx| app.draw(ctx));
        assert!(output.viewport_output[&egui::ViewportId::ROOT]
            .commands
            .iter()
            .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
        assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
        assert!(app.lifecycle.is_some());
        click(&mut app, &ctx, 1);
        click(&mut app, &ctx, 1);
        frame(&mut app, &ctx, vec![]);
        assert!(app.exit_approved);
        assert!(root.lock().unwrap().closed && second.lock().unwrap().closed);
    }
}
#[derive(Clone)]
struct Entry {
    source: Reviewed,
    name: String,
    path: Option<PathBuf>,
    dirty: bool,
    decided: bool,
}
pub(super) struct Review {
    intent: Intent,
    entries: Vec<Entry>,
    error: Option<String>,
    saving: Option<mpsc::Receiver<SaveReply>>,
    pub(super) buttons: [Option<Rect>; 3],
}
struct SaveReply {
    source: Reviewed,
    result: Result<bool, String>,
}

fn entries(workspace: &crate::workspace::Registry, intent: &Intent) -> Result<Vec<Entry>, String> {
    let mut rows = Vec::new();
    for (project, _, shared) in crate::workspace::entries_in(workspace) {
        if matches!(intent, Intent::Close(id) if id != &project) {
            continue;
        }
        let e = shared
            .try_lock()
            .map_err(|_| "A project is still working. Try again when it finishes.")?;
        e.ensure_open()?;
        let dirty = e.doc.revision != e.saved_revision;
        rows.push(Entry {
            source: Reviewed {
                project,
                document: e.doc.id.clone(),
                revision: e.doc.revision,
                discard: false,
            },
            name: e.doc.name.clone(),
            path: e.path.clone(),
            dirty,
            decided: !dirty,
        });
    }
    if rows.is_empty() {
        return Err("The project has already closed.".into());
    }
    Ok(rows)
}

impl Review {
    fn current(&self) -> Option<&Entry> {
        self.entries.iter().find(|e| !e.decided)
    }
    fn refresh(&mut self, workspace: &crate::workspace::Registry) -> Result<(), String> {
        let mut live = entries(workspace, &self.intent)?;
        for row in &mut live {
            if let Some(old) = self.entries.iter().find(|old| {
                old.source.project == row.source.project
                    && old.source.document == row.source.document
                    && old.source.revision == row.source.revision
            }) {
                row.decided |= old.decided && (old.dirty || !row.dirty);
                row.source.discard = old.source.discard;
            }
        }
        let changed = live.iter().any(|row| {
            !self.entries.iter().any(|old| {
                old.source.project == row.source.project
                    && old.source.document == row.source.document
                    && old.source.revision == row.source.revision
            })
        });
        if changed {
            self.error = Some(
                "Work changed during review. Review the current changes before closing.".into(),
            );
        }
        self.entries = live;
        Ok(())
    }
}

impl PeerBrush {
    pub(super) fn request_close(&mut self, intent: Intent) {
        if self.lifecycle.is_some() || self.exit_approved {
            return;
        }
        let working = match &intent {
            Intent::Close(project) => self.jobs.contains(project),
            Intent::Exit => !self.jobs.is_empty() || self.tab_transfer_committing,
        };
        if working {
            self.message = "Finish or cancel the current file operation before closing.".into();
            return;
        }
        let affects_active =
            intent == Intent::Exit || matches!(&intent,Intent::Close(id) if id == &self.project_id);
        if affects_active
            && (self.geometry.is_some()
                || self.source_editor.is_some()
                || self.filter_editor.is_some()
                || self.refinement.is_some()
                || self.pending_import.is_some()
                || self.rename_edit.is_some()
                || !self.points.is_empty()
                || !self.selection_path.is_empty()
                || self.layer_drag.is_some()
                || self.eye_sweep.is_some()
                || self.tab_transfer.is_some()
                || self.parameter_gesture.as_ref().is_some_and(|edit| {
                    edit.typing
                        .as_ref()
                        .is_some_and(|ctx| controls::number_editing(ctx, edit.control))
                }))
        {
            self.message =
                "Finish applying or canceling the current tool or text edit before closing.".into();
            return;
        }
        if affects_active {
            self.finish_parameter(false);
        }
        self.lifecycle_frame = true;
        match entries(&self.workspace, &intent) {
            Ok(entries) => {
                self.lifecycle = Some(Review {
                    intent,
                    entries,
                    error: None,
                    saving: None,
                    buttons: [None; 3],
                })
            }
            Err(error) => self.message = error,
        }
    }

    fn save_for_close(&mut self, ctx: &egui::Context, save_as: bool) {
        let Some(review) = self.lifecycle.as_ref() else {
            return;
        };
        if review.saving.is_some() {
            return;
        }
        let Some(row) = review.current().cloned() else {
            return;
        };
        let shared = match crate::workspace::get_in(&self.workspace, &row.source.project) {
            Ok(shared) => shared,
            Err(error) => {
                self.lifecycle.as_mut().unwrap().error = Some(error);
                return;
            }
        };
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        let source = row.source.clone();
        let result = std::thread::Builder::new()
            .name("peerbrush-close-save".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let path = if save_as { None } else { row.path }.or_else(|| {
                        rfd::FileDialog::new()
                            .add_filter("Photoshop document", &["psd"])
                            .set_file_name(&row.name)
                            .save_file()
                    });
                    let Some(path) = path else {
                        return Ok(false);
                    };
                    server::save_reviewed_source(
                        &shared,
                        &path,
                        &source.document,
                        source.revision,
                    )?;
                    Ok(true)
                }))
                .unwrap_or_else(|_| {
                    Err("Save could not complete. Current work remains open.".into())
                });
                let _ = tx.send(SaveReply { source, result });
                repaint.request_repaint();
            });
        let review = self.lifecycle.as_mut().unwrap();
        match result {
            Ok(_) => {
                review.saving = Some(rx);
                review.error = None;
            }
            Err(error) => review.error = Some(format!("Could not start saving: {error}")),
        }
    }

    pub(super) fn lifecycle_dialog(&mut self, ctx: &egui::Context) {
        if self.lifecycle.is_none() && !self.exit_approved {
            for (project, _, shared) in crate::workspace::entries_in(&self.workspace) {
                if self.jobs.contains(&project) {
                    continue;
                }
                let request = shared.try_lock().ok().and_then(|mut e| {
                    e.native_close_review.take().map(|(document, revision)| {
                        crate::workspace::guard(&e, &document, revision)
                    })
                });
                if let Some(result) = request {
                    match result {
                        Ok(()) => self.request_close(Intent::Close(project)),
                        Err(error) => self.message = error,
                    }
                    break;
                }
            }
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.exit_approved {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.request_close(Intent::Exit);
        }
        if self.exit_approved {
            return;
        }
        let Some(mut review) = self.lifecycle.take() else {
            return;
        };
        self.lifecycle_frame = true;
        if let Some(rx) = &review.saving {
            if let Ok(reply) = rx.try_recv() {
                review.saving = None;
                match reply.result {
                    Ok(true) => {
                        let saved =
                            crate::workspace::get_in(&self.workspace, &reply.source.project)
                                .ok()
                                .and_then(|shared| {
                                    shared.try_lock().ok().map(|e| {
                                        crate::workspace::guard(
                                            &e,
                                            &reply.source.document,
                                            reply.source.revision,
                                        )
                                        .is_ok()
                                            && e.saved_revision == reply.source.revision
                                    })
                                })
                                .unwrap_or(false);
                        if saved {
                            if let Some(row) = review
                                .entries
                                .iter_mut()
                                .find(|r| r.source.project == reply.source.project)
                            {
                                row.decided = true;
                                row.source.discard = false;
                            }
                        } else {
                            review.error = Some("Work changed while saving. The project remains open; review its current changes.".into());
                        }
                    }
                    Ok(false) => {
                        review.error = Some("Save canceled. The project remains open.".into())
                    }
                    Err(error) => review.error = Some(error),
                }
            }
        }
        if review.saving.is_none() {
            let refreshed = match review.refresh(&self.workspace) {
                Ok(()) => true,
                Err(error) => {
                    review.error = Some(error);
                    false
                }
            };
            if refreshed && review.current().is_none() {
                let result = match &review.intent {
                    Intent::Exit => crate::workspace::lifecycle::finish_exit(
                        &self.workspace,
                        &review
                            .entries
                            .iter()
                            .map(|r| r.source.clone())
                            .collect::<Vec<_>>(),
                    ),
                    Intent::Close(project) => {
                        let row = &review.entries[0];
                        crate::workspace::close_in(
                            &self.workspace,
                            project,
                            &row.source.document,
                            row.source.revision,
                            row.source.discard,
                            "human",
                        )
                    }
                };
                match result {
                    Ok(()) => {
                        // Cleanup runs only after the entire final source check succeeded.
                        server::checkpoint_projects(
                            &self.workspace,
                            &self.connection.state_dir,
                            &mut Default::default(),
                        );
                        if review.intent == Intent::Exit {
                            self.exit_approved = true;
                            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        } else {
                            self.message = "Project closed".into();
                        }
                        return;
                    }
                    Err(error) => review.error = Some(error),
                }
            }
        }
        let current = review.current().cloned();
        let title = if review.intent == Intent::Exit {
            "Save project before exiting?"
        } else {
            "Save project before closing?"
        };
        let saving = review.saving.is_some();
        let mut action = 0;
        let mut save_as = false;
        review.buttons = [None; 3];
        let modal = egui::Modal::new(egui::Id::new("project lifecycle")).show(ctx, |ui| {
            ui.set_width(380.);
            ui.heading(title);
            if let Some(row) = &current {
                ui.label(RichText::new(&row.name).strong());
                ui.label(
                    row.path
                        .as_ref()
                        .map(|p| format!("Save to {}", p.display()))
                        .unwrap_or_else(|| "Choose a PSD destination when saving.".into()),
                );
                if review.intent == Intent::Exit {
                    ui.label(format!(
                        "{} project(s) still need a decision. Cancel keeps every project open.",
                        review.entries.iter().filter(|r| !r.decided).count()
                    ));
                }
            }
            if let Some(error) = &review.error {
                ui.label(RichText::new(error).color(ACCENT));
            }
            if saving {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Saving…");
                });
            }
            ui.horizontal(|ui| {
                for (index, label) in ["Save", "Don't Save", "Cancel"].into_iter().enumerate() {
                    let response = ui.add_enabled(
                        !saving && (index == 2 || current.is_some()),
                        egui::Button::new(label),
                    );
                    review.buttons[index] = Some(response.rect);
                    if response.clicked() {
                        action = index + 1;
                    }
                }
            });
            if current.as_ref().is_some_and(|r| r.path.is_some())
                && ui
                    .add_enabled(!saving, egui::Button::new("Save As…"))
                    .clicked()
            {
                save_as = true;
            }
        });
        if !saving && modal.should_close() {
            action = 3;
        }
        if action == 3 {
            self.message = "Close canceled · all projects remain open".into();
            return;
        }
        if action == 2 {
            if let Some(row) = review.entries.iter_mut().find(|r| !r.decided) {
                row.source.discard = true;
                row.decided = true;
            }
        }
        self.lifecycle = Some(review);
        if action == 1 || save_as {
            self.save_for_close(ctx, save_as);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}
