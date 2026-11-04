use super::*;

pub(super) fn progress(ui: &mut egui::Ui, control: &crate::saving::Control) -> egui::Response {
    let status = control.status();
    ui.label(&status.stage);
    if status.total > 0 {
        ui.add(
            egui::ProgressBar::new(status.completed as f32 / status.total as f32).show_percentage(),
        );
    } else {
        ui.spinner();
    }
    let response = ui.add_enabled(status.can_cancel, egui::Button::new("Cancel saving"));
    if response.clicked() {
        control.cancel();
    }
    if status.cancel_requested {
        ui.label("Cancel requested; waiting for the current preparation or file operation.");
    } else if !status.can_cancel {
        ui.label("Finishing save; cancellation is no longer available.");
    }
    response
}

impl PeerBrush {
    pub(super) fn save_progress_windows(&self, ctx: &egui::Context) {
        // Close-save uses the same controls inside its modal so Cancel stays reachable.
        if self.lifecycle.is_some() {
            return;
        }
        for (_, name, shared) in crate::workspace::entries_in(&self.workspace) {
            let control = shared
                .try_lock()
                .ok()
                .and_then(|e| e.saving.clone())
                .filter(|control| !control.status().finished);
            let Some(control) = control else { continue };
            let status = control.status();
            egui::Window::new("Saving Photoshop document")
                .id(egui::Id::new(("working save", &status.run)))
                .collapsible(false)
                .resizable(false)
                .default_width(350.)
                .show(ctx, |ui| {
                    ui.label(name);
                    progress(ui, &control);
                });
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        ctx: &egui::Context,
        control: &crate::saving::Control,
        events: Vec<egui::Event>,
    ) -> (egui::FullOutput, egui::Response) {
        let mut response = None;
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(640., 480.))),
                events,
                ..Default::default()
            },
            |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    response = Some(progress(ui, control));
                });
            },
        );
        (output, response.unwrap())
    }

    #[test]
    fn actual_save_controls_show_rows_cancel_and_disable_late_cancellation() {
        for finalizing in [false, true] {
            let ctx = egui::Context::default();
            let control = crate::saving::Control::default();
            control.begin("native-source".into(), 3).unwrap();
            control
                .progress("Writing standard layer channels", 3, 12)
                .unwrap();
            if finalizing {
                control.commit().unwrap();
            }
            let mut output = frame(&ctx, &control, vec![]);
            for _ in 0..3 {
                output = frame(&ctx, &control, vec![]);
            }
            let text = output
                .0
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains(if finalizing {
                "Finishing save"
            } else {
                "Writing standard layer channels"
            }));
            assert!(text.contains("Cancel saving"));
            assert_eq!(output.1.enabled(), !finalizing);
            let pos = output.1.rect.center();
            for pressed in [true, false] {
                frame(
                    &ctx,
                    &control,
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
            assert_eq!(control.status().cancel_requested, !finalizing);
            assert_eq!(control.check().is_ok(), finalizing);
        }
    }
    #[test]
    fn ordinary_save_window_cancels_exact_shared_run_and_keeps_native_unsaved_work() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            let (before, saved, history, document, revision) = {
                let mut e = app.shared.lock().unwrap();
                e.doc = Document::new_depth(8, 4, depth).unwrap();
                e.doc.layers[0].pixels.set16(2, 1, [12347, 23459, 34571, 0]);
                e.doc.revision = 1;
                (
                    serde_json::to_value(&e.doc).unwrap(),
                    e.saved_revision,
                    e.undo.len(),
                    e.doc.id.clone(),
                    e.doc.revision,
                )
            };
            let control = crate::saving::Control::default();
            control.begin(document, revision).unwrap();
            control
                .progress("Writing standard layer channels", 2, 20)
                .unwrap();
            app.shared.lock().unwrap().saving = Some(control.clone());
            app.busy = true;
            app.jobs.insert(app.project_id.clone());
            let draw = |app: &mut PeerBrush, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            let mut output = draw(&mut app, vec![]);
            for _ in 0..3 {
                output = draw(&mut app, vec![]);
            }
            let cancel = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "Cancel saving" => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .expect("The ordinary Save window must contain a reachable cancel control");
            for pressed in [true, false] {
                draw(
                    &mut app,
                    vec![
                        egui::Event::PointerMoved(cancel),
                        egui::Event::PointerButton {
                            pos: cancel,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        },
                    ],
                );
            }
            assert!(control.status().cancel_requested);
            let result = control.check();
            control.finish(&result);
            app.job_tx
                .send(JobReply {
                    project: app.project_id.clone(),
                    result: result.map(|_| String::new()),
                })
                .unwrap();
            draw(&mut app, vec![]);
            assert!(!app.busy);
            assert!(!app.jobs.contains(&app.project_id));
            assert!(app.message.contains("canceled"));
            let e = app.shared.lock().unwrap();
            assert!(!e.closed);
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert_eq!(e.saved_revision, saved);
            assert_eq!(e.undo.len(), history);
            assert_ne!(e.doc.revision, e.saved_revision);
            assert!(e.path.is_none());
        }
    }
}
