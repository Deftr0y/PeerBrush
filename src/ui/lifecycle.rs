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
                availability: Default::default(),
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
    #[cfg(target_os = "macos")]
    #[test]
    fn late_native_start_preserves_an_already_reviewed_human_exit_and_all_projects() {
        for depth in [8, 16] {
            let (mut app, ctx, root, second) = fixture(depth);
            for shared in [&root, &second] {
                let mut engine = shared.lock().unwrap();
                if depth == 16 {
                    engine.doc.layers[0]
                        .pixels
                        .set16(0, 0, [1001, 3003, 5007, 0]);
                } else {
                    engine.doc.layers[0].pixels.set(0, 0, [17, 93, 207, 0]);
                }
            }
            dirty(&root, "Keep reviewed first artwork");
            dirty(&second, "Keep reviewed second artwork");
            let before: Vec<_> = [&root, &second]
                .into_iter()
                .map(|shared| {
                    let engine = shared.lock().unwrap();
                    (
                        serde_json::to_value(&engine.doc).unwrap(),
                        engine.undo.len(),
                        engine.redo.len(),
                    )
                })
                .collect();
            app.request_close(Intent::Exit);
            let review = app.lifecycle.as_mut().unwrap();
            for row in &mut review.entries {
                row.decided = true;
                row.source.discard = true;
            }
            assert!(review.current().is_none());
            // Model a native startup arriving after human decisions but before
            // the final close. This hook changes only the unavailable Session.
            app.native_check_starting_for_test();
            let mut output = frame(&mut app, &ctx, vec![]);
            for _ in 0..3 {
                output = frame(&mut app, &ctx, vec![]);
            }
            assert!(!app.exit_approved);
            let review = app
                .lifecycle
                .as_ref()
                .expect("The human review must remain open");
            assert!(review.current().is_none());
            assert!(review
                .entries
                .iter()
                .all(|row| row.decided && row.source.discard));
            assert!(review
                .error
                .as_ref()
                .unwrap()
                .contains("Quitting is blocked"));
            assert!(
                review.buttons[2].is_some(),
                "Blocked review must render its real Cancel control"
            );
            let text = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("Quitting is blocked"));
            click(&mut app, &ctx, 2);
            assert!(
                app.lifecycle.is_none(),
                "Real Cancel must return to the workspace"
            );
            assert!(!app.exit_approved);
            assert!(app.message.contains("Close canceled"));
            for (shared, (document, undo, redo)) in [&root, &second].into_iter().zip(before) {
                let engine = shared.lock().unwrap();
                assert!(!engine.closed);
                assert_eq!(serde_json::to_value(&engine.doc).unwrap(), document);
                assert_eq!((engine.undo.len(), engine.redo.len()), (undo, redo));
            }
        }
    }
    #[test]
    fn shared_save_cancel_status_and_reply_keep_the_native_close_review_open() {
        for depth in [8, 16] {
            let (mut app, ctx, root, second) = fixture(depth);
            dirty(&root, "Keep canceled save work");
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
            let before = serde_json::to_value(&root.lock().unwrap().doc).unwrap();
            let other = serde_json::to_value(&second.lock().unwrap().doc).unwrap();
            let control = crate::saving::Control::default();
            control
                .begin(source.document.clone(), source.revision)
                .unwrap();
            control
                .progress("Writing current composite", 2, 16)
                .unwrap();
            root.lock().unwrap().saving = Some(control.clone());
            let (tx, rx) = mpsc::channel();
            app.lifecycle.as_mut().unwrap().saving = Some(rx);
            let mut output = frame(&mut app, &ctx, vec![]);
            for _ in 0..3 {
                output = frame(&mut app, &ctx, vec![]);
            }
            let text = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) => Some(text.galley.text()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("Writing current composite"));
            assert!(text.contains("Cancel saving"));
            let cancel = output
                .shapes
                .iter()
                .find_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text() == "Cancel saving" => {
                        Some(text.pos + text.galley.size() * 0.5)
                    }
                    _ => None,
                })
                .unwrap();
            for pressed in [true, false] {
                frame(
                    &mut app,
                    &ctx,
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
            tx.send(SaveReply {
                source,
                result: result.map(|_| true),
            })
            .unwrap();
            frame(&mut app, &ctx, vec![]);
            assert!(app.lifecycle.as_ref().unwrap().current().is_some());
            assert!(app
                .lifecycle
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("canceled"));
            let e = root.lock().unwrap();
            assert!(!e.closed);
            assert_ne!(e.doc.revision, e.saved_revision);
            assert_eq!(e.undo.len(), 1);
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            drop(e);
            assert!(!second.lock().unwrap().closed);
            assert_eq!(
                serde_json::to_value(&second.lock().unwrap().doc).unwrap(),
                other
            );
            click(&mut app, &ctx, 2);
        }
    }

    #[test]
    fn clean_conflicting_project_reviews_native_cancel_failed_save_and_discard() {
        for depth in [8, 16] {
            let (mut app, ctx, root, second) = fixture(depth);
            let path = app.connection.state_dir.join("conflicting.psd");
            server::save(&root, &path).unwrap();
            let before = root.lock().unwrap().doc.export_png().unwrap();
            // Simulate another editor replacing the source: duplicate PeerBrush opens are blocked.
            let mut newer = root.lock().unwrap().doc.clone();
            newer.layers[0].name = "Newer saved source".into();
            server::atomic_write(&path, &crate::psd::encode(&newer).unwrap()).unwrap();
            let saved = std::fs::read(&path).unwrap();
            frame(&mut app, &ctx, vec![]);
            let project = root.lock().unwrap().project_id.clone();
            app.request_close(Intent::Close(project.clone()));
            frame(&mut app, &ctx, vec![]);
            assert!(
                app.lifecycle
                    .as_ref()
                    .unwrap()
                    .current()
                    .unwrap()
                    .conflicted
            );
            click(&mut app, &ctx, 2);
            assert!(app.lifecycle.is_none());
            assert!(!root.lock().unwrap().closed);
            app.request_close(Intent::Close(project));
            click(&mut app, &ctx, 0);
            waiting(&mut app, &ctx);
            assert!(app
                .lifecycle
                .as_ref()
                .unwrap()
                .error
                .as_ref()
                .unwrap()
                .contains("changed"));
            assert!(!root.lock().unwrap().closed);
            assert_eq!(std::fs::read(&path).unwrap(), saved);
            assert_eq!(root.lock().unwrap().doc.export_png().unwrap(), before);
            click(&mut app, &ctx, 1);
            frame(&mut app, &ctx, vec![]);
            assert!(root.lock().unwrap().closed);
            assert!(!second.lock().unwrap().closed);
            assert_eq!(std::fs::read(&path).unwrap(), saved);
        }
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
    fn clean_native_close_is_not_canceled_after_final_source_validation() {
        for depth in [8, 16] {
            let (mut app, ctx, root, second) = fixture(depth);
            for _ in 0..3 {
                frame(&mut app, &ctx, vec![]);
            }
            let mut input = egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                ..Default::default()
            };
            input
                .viewports
                .get_mut(&egui::ViewportId::ROOT)
                .unwrap()
                .events
                .push(egui::ViewportEvent::Close);
            let output = ctx.run(input, |ctx| app.draw(ctx));
            assert!(app.exit_approved);
            assert!(root.lock().unwrap().closed && second.lock().unwrap().closed);
            // eframe cancels the original native close if this appears anywhere
            // in the frame, even when a Close command is also present.
            assert!(!output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::CancelClose));
        }
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
    #[test]
    fn queued_native_quit_preserves_cancel_save_and_late_write_guards() {
        use std::sync::atomic::Ordering;
        for depth in [8, 16] {
            let (mut app, ctx, root, second) = fixture(depth);
            dirty(&root, "First native Quit edit");
            dirty(&second, "Second native Quit edit");
            let before = [
                root.lock().unwrap().doc.clone(),
                second.lock().unwrap().doc.clone(),
            ];
            let quit = crate::activation::quit_signal(&app.workspace);
            quit.store(true, Ordering::Release);
            frame(&mut app, &ctx, vec![]);
            assert!(matches!(
                app.lifecycle.as_ref().unwrap().intent,
                Intent::Exit
            ));
            click(&mut app, &ctx, 2);
            assert!(app.lifecycle.is_none() && !app.exit_approved);
            for (shared, expected) in [&root, &second].into_iter().zip(&before) {
                let engine = shared.lock().unwrap();
                assert!(!engine.closed);
                assert_eq!(
                    serde_json::to_value(&engine.doc).unwrap(),
                    serde_json::to_value(expected).unwrap()
                );
                assert_eq!(engine.undo.len(), 1);
            }
            server::save(&root, &app.connection.state_dir.join("first.psd")).unwrap();
            server::save(&second, &app.connection.state_dir.join("second.psb")).unwrap();
            dirty(&root, "Save this exact source");
            dirty(&second, "Keep review open");
            quit.store(true, Ordering::Release);
            frame(&mut app, &ctx, vec![]);
            click(&mut app, &ctx, 0);
            waiting(&mut app, &ctx);
            let operations = second.lock().unwrap().file_operations.clone();
            operations.fetch_add(1, Ordering::AcqRel);
            click(&mut app, &ctx, 1);
            frame(&mut app, &ctx, vec![]);
            assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
            assert!(app.lifecycle.as_ref().unwrap().error.is_some());
            operations.fetch_sub(1, Ordering::AcqRel);
            dirty(&root, "Late human edit must survive");
            let late = root.lock().unwrap().doc.clone();
            frame(&mut app, &ctx, vec![]);
            click(&mut app, &ctx, 2);
            assert_eq!(
                serde_json::to_value(&root.lock().unwrap().doc).unwrap(),
                serde_json::to_value(&late).unwrap()
            );
            assert!(!app.exit_approved);
            assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
            server::save(&root, &app.connection.state_dir.join("first.psd")).unwrap();
            server::save(&second, &app.connection.state_dir.join("second.psb")).unwrap();
            quit.store(true, Ordering::Release);
            let output = frame(&mut app, &ctx, vec![]);
            assert!(app.exit_approved);
            assert!(root.lock().unwrap().closed && second.lock().unwrap().closed);
            assert!(output.viewport_output[&egui::ViewportId::ROOT]
                .commands
                .contains(&egui::ViewportCommand::Close));
        }
    }

    #[test]
    fn queued_native_quit_keeps_existing_color_preferences_and_export_drafts() {
        use std::sync::atomic::Ordering;
        for depth in [8, 16] {
            for draft in ["color", "preferences", "export", "proposal", "live gesture"] {
                let (mut app, ctx, root, second) = fixture(depth);
                let before = root.lock().unwrap().doc.clone();
                match draft {
                    "color" => {
                        app.open_color(super::super::color::Target::Foreground, [17, 41, 73, 255])
                    }
                    "preferences" => app.project_settings = Some((96, 80, depth)),
                    "export" => app.open_exports(&before),
                    "proposal" => {
                        let proposal = crate::collaboration::propose(&root, "synthetic-review", &json!({"document_id":before.id,"expected_revision":before.revision,"commands":[{"op":"layer.update","layer":before.layers[0].id,"name":"Proposed change"}]})).unwrap();
                        app.proposal_review = Some(proposal["id"].as_str().unwrap().into());
                    }
                    "live gesture" => app.live_gesture = Some(crate::engine::id()),
                    _ => unreachable!(),
                }
                crate::activation::quit_signal(&app.workspace).store(true, Ordering::Release);
                frame(&mut app, &ctx, vec![]);
                assert!(app.lifecycle.is_none() && !app.exit_approved, "{draft}");
                assert!(!root.lock().unwrap().closed && !second.lock().unwrap().closed);
                assert_eq!(
                    serde_json::to_value(&root.lock().unwrap().doc).unwrap(),
                    serde_json::to_value(&before).unwrap()
                );
                assert!(match draft {
                    "color" => app.color_editor.is_some(),
                    "preferences" => app.project_settings == Some((96, 80, depth)),
                    "export" => app.export_editor.is_some(),
                    "proposal" => app.proposal_review.is_some(),
                    "live gesture" => app.live_gesture.is_some(),
                    _ => false,
                });
            }
        }
    }
}
#[derive(Clone)]
struct Entry {
    source: Reviewed,
    name: String,
    path: Option<PathBuf>,
    dirty: bool,
    conflicted: bool,
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
        let conflicted = crate::file_safety::source_conflict(e.path.as_deref(), e.file_version);
        let dirty = e.doc.revision != e.saved_revision || conflicted;
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
            conflicted,
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
                if old.conflicted != row.conflicted {
                    row.decided = false;
                }
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
        #[cfg(target_os = "macos")]
        if intent == Intent::Exit && self.native_update_quit() {
            return;
        }
        if self.lifecycle.is_some() || self.save_loss_review.is_some() || self.exit_approved {
            return;
        }
        #[cfg(any(windows, test))]
        if intent == Intent::Exit {
            self.updates.defer_windows_for_close();
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
            && (self.free_transform.is_some()
                || self.geometry.is_some()
                || self.source_editor.is_some()
                || self.filter_editor.is_some()
                || self.refinement.is_some()
                || self.pending_import.is_some()
                || self.pending_image_open.is_some()
                || self.rename_edit.is_some()
                || self.color_editor.is_some()
                || self.proposal_review.is_some()
                || self.task_undo_review.is_some()
                || self.live_gesture.is_some()
                || self.pen.in_gesture()
                || self.project_settings.is_some()
                || self.export_editor.is_some()
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
        self.save_for_close_with_acceptance(ctx, save_as, None);
    }
    pub(super) fn cancel_close_save_loss(&mut self) {
        if let Some(review) = self.lifecycle.as_mut() {
            review.error = Some("Save canceled · the project remains open".into());
        }
    }
    pub(super) fn save_for_close_with_acceptance(
        &mut self,
        ctx: &egui::Context,
        save_as: bool,
        accepted: Option<(&str, u64)>,
    ) {
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
        let report = {
            let e = shared.lock().unwrap();
            crate::workspace::guard(&e, &row.source.document, row.source.revision)
                .and_then(|_| crate::compatibility::report(&e.doc))
        };
        let report = match report {
            Ok(report) => report,
            Err(error) => {
                self.lifecycle.as_mut().unwrap().error = Some(error);
                return;
            }
        };
        if !report.save_allowed {
            self.lifecycle.as_mut().unwrap().error =
                Some("Protected source · create an explicit compatible copy before saving".into());
            return;
        }
        if accepted
            .is_some_and(|source| source != (row.source.document.as_str(), row.source.revision))
        {
            self.lifecycle.as_mut().unwrap().error =
                Some("Work changed after save-loss review · review the current changes".into());
            return;
        }
        if report.has_editability_losses && accepted.is_none() {
            self.save_loss_review = Some(super::compatibility::SaveReview::new(
                row.source.project.clone(),
                row.name.clone(),
                report,
                super::compatibility::SaveIntent::Close { save_as },
            ));
            return;
        }
        let accepted = accepted.is_some();
        let (tx, rx) = mpsc::channel();
        let repaint = ctx.clone();
        let source = row.source.clone();
        let result = std::thread::Builder::new()
            .name("peerbrush-close-save".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let overwrite = save_as || row.path.is_none();
                    let path = if save_as { None } else { row.path }.or_else(|| {
                        rfd::FileDialog::new()
                            .add_filter("PSD document", &["psd"])
                            .add_filter("PSB document", &["psb"])
                            .set_file_name(&row.name)
                            .save_file()
                    });
                    let Some(path) = path else {
                        return Ok(false);
                    };
                    if accepted {
                        server::save_accepted_source(
                            &shared,
                            &path,
                            &source.document,
                            source.revision,
                            overwrite,
                        )?;
                    } else {
                        server::save_confirmed_source(
                            &shared,
                            &path,
                            &source.document,
                            source.revision,
                            overwrite,
                        )?;
                    }
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
        let native_quit = crate::activation::take_quit(&self.workspace);
        if (ctx.input(|i| i.viewport().close_requested()) || native_quit) && !self.exit_approved {
            #[cfg(target_os = "macos")]
            if self.native_update_quit() {
                ctx.send_viewport_cmd(if self.exit_approved {
                    egui::ViewportCommand::Close
                } else {
                    egui::ViewportCommand::CancelClose
                });
                return;
            }
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
                #[cfg(target_os = "macos")]
                let blocked = review.intent == Intent::Exit && self.native_update_quit();
                #[cfg(not(target_os = "macos"))]
                let blocked = false;
                if self.exit_approved {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
                let result = if blocked {
                    Err(self.message.clone())
                } else {
                    match &review.intent {
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
        let save_control = current.as_ref().and_then(|row| {
            crate::workspace::get_in(&self.workspace, &row.source.project)
                .ok()
                .and_then(|shared| shared.try_lock().ok().and_then(|e| e.saving.clone()))
                .filter(|control| !control.status().finished)
        });
        let mut action = 0;
        let mut save_as = false;
        review.buttons = [None; 3];
        let modal = egui::Modal::new(egui::Id::new("project lifecycle")).show(ctx, |ui| {
            dialogs::body(ui, 400.);
            dialogs::heading(ui, title, current.as_ref().is_some_and(|row| row.conflicted));
            if let Some(row) = &current {
                ui.label(RichText::new(&row.name).strong());
                if row.conflicted { ui.label(RichText::new("The saved file changed outside this project. Save As preserves both versions. Don't Save closes only this project's in-memory copy.").color(ACCENT)); }
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
                if let Some(control) = &save_control {
                    super::saving::progress(ui, control);
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Saving…");
                    });
                }
            }
            ui.horizontal(|ui| {
                for (index, label) in ["Save", "Don't Save", "Cancel"].into_iter().enumerate() {
                    let response = ui.add_enabled(
                        !saving && (index == 2 || current.is_some()),
                        if index == 0 { dialogs::primary(label) } else { egui::Button::new(label).min_size(Vec2::new(0., 28.)) },
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
