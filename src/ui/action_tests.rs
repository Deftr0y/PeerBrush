use super::*;

#[test]
fn layer_panel_creation_selects_receipt_ids_and_rejects_stale_or_pending_sources() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace_tests::fixture(depth);
        let base = app.selected.clone();
        let initial = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.create_panel_layer(&initial, Some(("hsl", "Hue / saturation")));
        let first = app.selected.clone();
        assert_ne!(first, base);
        app.select_content(&base);
        let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let history = app.shared.lock().unwrap().history_lengths();
        app.create_panel_layer(&before, Some(("hsl", "Hue / saturation")));
        assert_ne!(app.selected, first);
        assert_ne!(app.selected, base);
        {
            let mut engine = app.shared.lock().unwrap();
            let selected = engine
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|layer| layer.id == app.selected)
                .unwrap();
            assert_eq!(selected.kind, "adjustment");
            assert_eq!(selected.name, "Hue / saturation");
            assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
            assert_eq!(
                engine
                    .resident_state().expect("resident fixture").document
                    .layers
                    .iter()
                    .find(|layer| layer.id == base)
                    .unwrap()
                    .pixels,
                initial.layers[0].pixels
            );
            engine.undo("human").unwrap();
            assert_eq!(
                engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                before.export_png().unwrap()
            );
        }
        let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        app.create_panel_layer(&current, None);
        let paint = app.selected.clone();
        assert_eq!(
            app.shared
                .lock()
                .unwrap()
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|layer| layer.id == paint)
                .unwrap()
                .kind,
            "paint"
        );
        let selected = app.selected.clone();
        let saved = serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
        app.create_panel_layer(&before, None);
        assert_eq!(app.selected, selected);
        assert_eq!(
            serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
            saved
        );
        let current = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let shared = app.shared.clone();
        let guard = shared.lock().unwrap();
        app.submit_edit_job(
            edit_job::Request {
                project: app.project_id.clone(),
                document: current.id.clone(),
                revision: current.revision,
                label: "Pending".into(),
                selection_edit: false,
                origin: edit_job::Origin::Action,
            },
            edit_job::Commands::resident(vec![
                json!({"op":"layer.update","layer":paint,"name":"Pending"}),
            ]),
            &ctx,
        )
        .unwrap();
        app.proposal_review = Some("preserved-proposal".into());
        app.create_panel_layer(&current, None);
        app.create_panel_layer(&current, Some(("hsl", "Hue / saturation")));
        assert_eq!(app.selected, selected);
        assert_eq!(app.proposal_review.as_deref(), Some("preserved-proposal"));
        assert_eq!(serde_json::to_value(&guard.resident_state().expect("resident fixture").document).unwrap(), saved);
        app.cancel_pending_edit();
        drop(guard);
        text_tests::wait_edit(&mut app);
    }
}

#[test]
fn pending_action_preserves_rename_and_blocks_a_second_action_until_terminal_result() {
    for cancel in [false, true] {
        let (mut app, ctx) = tests::fixture();
        let doc = Document::new_depth(4, 4, 16).unwrap();
        let id = doc.layers[0].id.clone();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
        app.select_content(&id);
        app.begin_rename(&doc, &id);
        app.rename_edit.as_mut().unwrap().text = "Preserved draft".into();
        let shared = app.shared.clone();
        // Hold publication so neither scheduling nor machine speed can turn
        // this into an already-completed worker test.
        let guard = shared.lock().unwrap();
        app.submit_edit_job(
            edit_job::Request {
                project: app.project_id.clone(),
                document: doc.id.clone(),
                revision: doc.revision,
                label: "First action".into(),
                selection_edit: false,
                origin: edit_job::Origin::Action,
            },
            edit_job::Commands::resident(vec![json!({
                "op":"layer.add", "name":"First result"
            })]),
            &ctx,
        )
        .unwrap();
        assert!(app.edit_worker.pending());
        assert!(app.message.starts_with("Applying"));
        assert_eq!(app.selected, id);
        app.edit(
            vec![json!({"op":"layer.delete", "layer":id})],
            "Second action",
        );
        app.history_action(false);
        app.history_action(true);
        app.proposal_review = Some("preserved-review".into());
        app.duplicate_layers(&doc);
        app.duplicate_selection(&doc);
        app.create_folder(&doc);
        app.add_selected_masks(&doc);
        assert_eq!(app.proposal_review.as_deref(), Some("preserved-review"));
        assert!(!app.mask);
        app.finish_rename();
        assert_eq!(app.rename_edit.as_ref().unwrap().text, "Preserved draft");
        app.begin_rename(&doc, &id);
        assert_eq!(app.rename_edit.as_ref().unwrap().text, "Preserved draft");
        assert_eq!(guard.resident_state().expect("resident fixture").document.revision, doc.revision);
        assert!(guard.resident_state().expect("resident fixture").undo.is_empty());
        if cancel {
            app.cancel_pending_edit();
        }
        drop(guard);
        text_tests::wait_edit(&mut app);
        let engine = shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), if cancel { 1 } else { 2 });
        assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 16);
        if cancel {
            assert_eq!(engine.resident_state().expect("resident fixture").document.revision, doc.revision);
            assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
            assert!(app.message.contains("cancelled"));
            assert_eq!(app.selected, id);
        } else {
            assert_ne!(app.selected, id);
            let created = engine
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|layer| layer.id == app.selected)
                .unwrap();
            assert_eq!(created.name, "First result");
            assert_eq!(created.pixels.depth, 16);
            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 1);
            assert_eq!(app.message.as_ref(), "First action");
        }
    }
}

#[test]
fn queued_history_waits_for_publication_and_can_be_cancelled_without_moving_the_cursor() {
    for cancel in [false, true] {
        let (mut app, ctx) = tests::fixture();
        let doc = Document::new_depth(4, 4, 16).unwrap();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
        app.edit(
            vec![json!({"op":"layer.update", "layer":doc.layers[0].id, "name":"Edited"})],
            "Rename",
        );
        let shared = app.shared.clone();
        let guard = shared.lock().unwrap();
        app.submit_edit_job(
            edit_job::Request {
                project: app.project_id.clone(),
                document: doc.id.clone(),
                revision: 1,
                label: "Undo".into(),
                selection_edit: false,
                origin: edit_job::Origin::History(crate::engine::native::Direction::Undo),
            },
            edit_job::Commands::resident(vec![]),
            &ctx,
        )
        .unwrap();
        assert_eq!(guard.history_lengths(), [1, 0]);
        assert_eq!(guard.resident_state().expect("resident fixture").document.layers[0].name, "Edited");
        if cancel {
            app.cancel_pending_edit();
        }
        drop(guard);
        text_tests::wait_edit(&mut app);
        let engine = shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 16);
        assert_eq!(
            engine.history_lengths(),
            if cancel { [1, 0] } else { [0, 1] }
        );
        assert_eq!(engine.resident_state().expect("resident fixture").document.revision, if cancel { 1 } else { 2 });
        assert_eq!(
            engine.resident_state().expect("resident fixture").document.layers[0].name,
            if cancel {
                "Edited"
            } else {
                &doc.layers[0].name
            }
        );
    }
}

#[test]
fn terminal_selection_failure_survives_the_frame_that_polls_it() {
    let (mut app, ctx) = tests::fixture();
    let doc = Document::new(4, 4).unwrap();
    app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
    let (sent, received) = mpsc::sync_channel(1);
    app.edit_worker
        .submit(
            edit_job::Request {
                project: app.project_id.clone(),
                document: doc.id.clone(),
                revision: doc.revision,
                label: "Select".into(),
                selection_edit: true,
                origin: edit_job::Origin::Action,
            },
            edit_job::Commands::resident(vec![]),
            |_, _| Err("Selection source changed; current work preserved".into()),
            move || sent.send(()).unwrap(),
        )
        .unwrap();
    // The repaint callback runs after the terminal reply enters the channel.
    received.recv_timeout(Duration::from_secs(5)).unwrap();
    let _ = ctx.run(
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
            ..Default::default()
        },
        |ctx| app.draw(ctx),
    );
    assert!(!app.edit_worker.pending());
    assert!(app.selection_commit_failed);
    let engine = app.shared.lock().unwrap();
    assert_eq!(engine.resident_state().expect("resident fixture").document.revision, doc.revision);
    assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
}

#[test]
fn inline_rename_rejects_changed_revision_or_document_without_overwriting_human_work() {
    for replace in [false, true] {
        let (mut app, _) = tests::fixture();
        let doc = Document::new(4, 4).unwrap();
        let id = doc.layers[0].id.clone();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
        app.begin_rename(&doc, &id);
        app.rename_edit.as_mut().unwrap().text = "Stale draft".into();
        let shared = app.shared.clone();
        {
            let mut engine = shared.lock().unwrap();
            if replace {
                let mut replacement = Document::new(4, 4).unwrap();
                replacement.layers[0].id = id.clone();
                replacement.layers[0].name = "Replacement".into();
                engine.resident_state_mut().expect("resident fixture").document = replacement;
            } else {
                engine
                    .edit(
                        "human",
                        &[json!({
                            "op":"layer.update", "layer":id, "name":"New human name"
                        })],
                        Some(doc.revision),
                        None,
                        "Human rename",
                    )
                    .unwrap();
            }
        }
        let (name, revision, history) = {
            let engine = shared.lock().unwrap();
            (
                engine.resident_state().expect("resident fixture").document.layers[0].name.clone(),
                engine.resident_state().expect("resident fixture").document.revision,
                engine.resident_state().expect("resident fixture").undo.len(),
            )
        };
        app.finish_rename();
        let engine = shared.lock().unwrap();
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].name, name);
        assert_eq!(engine.resident_state().expect("resident fixture").document.revision, revision);
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history);
        assert!(app.message.contains("source changed"));
    }
}

#[test]
fn duplicate_actions_select_only_completed_roots_and_preserve_native_words() {
    for (queued, cancel, reselect) in [
        (false, false, false),
        (true, false, false),
        (true, true, false),
        (true, false, true),
    ] {
        let (mut app, ctx) = tests::fixture();
        let mut doc = Document::new_depth(4, 4, 16).unwrap();
        let mut folder = crate::engine::Layer::new("Folder", "group", 4, 4);
        folder.pixels = crate::raster::Raster::new_depth(4, 4, 16);
        doc.layers[0].name = "Child".into();
        doc.layers[0].parent = Some(folder.id.clone());
        doc.layers[0]
            .pixels
            .set16(1, 1, [12347, 23459, 34571, 45679]);
        let mut outside = crate::engine::Layer::new("Outside", "paint", 4, 4);
        outside.pixels = doc.layers[0].pixels.clone();
        doc.layers.push(folder);
        doc.layers.push(outside);
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
        app.select_content(&doc.layers[1].id);
        app.selection_layers = doc.layers.iter().map(|layer| layer.id.clone()).collect();
        let selected_before = app.selection_layers.clone();
        let active_before = app.selected.clone();
        app.layer_clipboard = false;
        if queued {
            let shared = app.shared.clone();
            let guard = shared.lock().unwrap();
            app.pending_duplicate_repeat = Some(app.duplicate_start((&doc).into()));
            app.submit_edit_job(
                edit_job::Request {
                    project: app.project_id.clone(),
                    document: doc.id.clone(),
                    revision: doc.revision,
                    label: "Duplicate layers".into(),
                    selection_edit: false,
                    origin: edit_job::Origin::Duplicate,
                },
                edit_job::Commands::resident(vec![
                    json!({"op":"layer.duplicate", "layers":app.selected_layer_ids(&doc)}),
                ]),
                &ctx,
            )
            .unwrap();
            assert_eq!(app.selected, active_before);
            assert_eq!(app.selection_layers, selected_before);
            assert!(!app.layer_clipboard);
            if cancel {
                app.cancel_pending_edit();
            }
            if reselect {
                app.select_content(&doc.layers[2].id);
            }
            drop(guard);
            text_tests::wait_edit(&mut app);
        } else {
            app.duplicate_layers(&doc);
        }
        let engine = app.shared.lock().unwrap();
        if cancel {
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 3);
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(app.selected, active_before);
            assert_eq!(app.selection_layers, selected_before);
            assert!(!app.layer_clipboard);
        } else {
            assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 6);
            assert_eq!(engine.history_lengths(), [1, 0]);
            let created: HashSet<_> = engine
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .filter(|layer| !selected_before.contains(&layer.id))
                .map(|layer| layer.id.clone())
                .collect();
            let roots = crate::tree::roots(&engine.resident_state().expect("resident fixture").document, &created);
            assert_eq!(roots.len(), 2);
            if reselect {
                let selected = doc.layers[2].id.clone();
                assert_eq!(app.selected, selected);
                assert_eq!(app.selection_layers, HashSet::from([selected]));
                assert!(!app.layer_clipboard);
                assert!(app.duplicate_repeat.is_none());
            } else {
                assert_eq!(app.selected, roots[0]);
                assert_eq!(app.selection_layers, roots.into_iter().collect());
                assert!(app.layer_clipboard);
            }
            assert!(!app.mask);
            for layer in engine
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .filter(|layer| layer.kind == "paint")
            {
                assert_eq!(layer.pixels.depth, 16);
                assert_eq!(layer.pixels.rgba16(), doc.layers[0].pixels.rgba16());
            }
        }
    }
}

#[test]
fn selection_duplication_uses_guarded_dispatch_and_selects_the_native_crop() {
    let (mut app, _) = tests::fixture();
    let mut doc = Document::new_depth(4, 4, 16).unwrap();
    doc.layers[0]
        .pixels
        .set16(1, 1, [12347, 23459, 34571, 45679]);
    doc.selection = Some([1, 1, 3, 3]);
    app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
    app.select_content(&doc.layers[0].id);
    app.duplicate_selection(&doc);
    let engine = app.shared.lock().unwrap();
    assert_eq!(engine.history_lengths(), [1, 0]);
    assert_eq!(engine.resident_state().expect("resident fixture").document.layers.len(), 2);
    assert_eq!(engine.resident_state().expect("resident fixture").document.selection, doc.selection);
    let copied = engine
        .resident_state().expect("resident fixture").document
        .layers
        .iter()
        .find(|layer| layer.id == app.selected)
        .unwrap();
    assert_ne!(copied.id, doc.layers[0].id);
    assert_eq!([copied.x, copied.y], [1, 1]);
    assert_eq!([copied.pixels.width, copied.pixels.height], [2, 2]);
    assert_eq!(copied.pixels.depth, 16);
    assert_eq!(copied.pixels.get16(0, 0), doc.layers[0].pixels.get16(1, 1));
    assert_eq!(
        app.selection_layers,
        [copied.id.clone()].into_iter().collect()
    );
    assert!(app.layer_clipboard);
}

#[test]
fn duplication_refusals_preserve_document_selection_and_clipboard_mode() {
    for selection in [false, true] {
        for refusal in ["document", "revision", "reservation"] {
            let (mut app, _) = tests::fixture();
            let mut doc = Document::new_depth(4, 4, 16).unwrap();
            doc.layers[0]
                .pixels
                .set16(1, 1, [12347, 23459, 34571, 45679]);
            doc.selection = Some([1, 1, 3, 3]);
            let id = doc.layers[0].id.clone();
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
            app.select_content(&id);
            app.layer_clipboard = false;
            let before = {
                let mut engine = app.shared.lock().unwrap();
                match refusal {
                    "document" => engine.resident_state_mut().expect("resident fixture").document.id = crate::engine::id(),
                    "revision" => engine.resident_state_mut().expect("resident fixture").document.revision += 1,
                    _ => {
                        engine
                            .reserve(
                                "agent",
                                "Reserved source",
                                vec![crate::engine::Scope::layer(&id)],
                            )
                            .unwrap();
                    }
                }
                serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap()
            };
            if selection {
                app.duplicate_selection(&doc);
            } else {
                app.duplicate_layers(&doc);
            }
            let engine = app.shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(app.selected, id);
            assert_eq!(app.selection_layers, [id.clone()].into_iter().collect());
            assert!(!app.layer_clipboard);
            assert!(app.message.contains(if refusal == "reservation" {
                "Reserved"
            } else {
                "changed"
            }));
        }
    }
}

#[test]
fn queued_group_and_mask_actions_change_controls_only_after_success() {
    for origin in [edit_job::Origin::Group, edit_job::Origin::Masks] {
        for outcome in ["commit", "cancel", "reserved"] {
            let (mut app, ctx) = tests::fixture();
            let mut doc = Document::new_depth(4, 4, 16).unwrap();
            doc.layers[0]
                .pixels
                .set16(1, 1, [12347, 23459, 34571, 45679]);
            let id = doc.layers[0].id.clone();
            app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
            app.select_content(&id);
            app.layer_clipboard = false;
            app.mask_step = Some("preserved-draft-step".into());
            let shared = app.shared.clone();
            if outcome == "reserved" {
                shared
                    .lock()
                    .unwrap()
                    .reserve(
                        "agent",
                        "Protected source",
                        vec![crate::engine::Scope::layer(&id)],
                    )
                    .unwrap();
            }
            let guard = shared.lock().unwrap();
            let command = if origin == edit_job::Origin::Group {
                json!({"op":"group.create_selected", "layers":[id], "name":"Folder"})
            } else {
                json!({"op":"mask.add", "layer":id, "value":255})
            };
            app.submit_edit_job(
                edit_job::Request {
                    project: app.project_id.clone(),
                    document: doc.id.clone(),
                    revision: doc.revision,
                    label: "Layer controls".into(),
                    selection_edit: false,
                    origin,
                },
                edit_job::Commands::resident(vec![command]),
                &ctx,
            )
            .unwrap();
            assert_eq!(app.selected, id);
            assert!(!app.mask);
            assert!(!app.layer_clipboard);
            assert_eq!(app.mask_step.as_deref(), Some("preserved-draft-step"));
            if outcome == "cancel" {
                app.cancel_pending_edit();
            }
            drop(guard);
            text_tests::wait_edit(&mut app);
            let engine = shared.lock().unwrap();
            let original = engine
                .resident_state().expect("resident fixture").document
                .layers
                .iter()
                .find(|layer| layer.id == id)
                .unwrap();
            assert_eq!(original.pixels.rgba16(), doc.layers[0].pixels.rgba16());
            if outcome != "commit" {
                assert_eq!(engine.history_lengths(), [0, 0]);
                assert_eq!(
                    serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
                    serde_json::to_value(&doc).unwrap()
                );
                assert_eq!(app.selected, id);
                assert!(!app.mask);
                assert!(!app.layer_clipboard);
                assert_eq!(app.mask_step.as_deref(), Some("preserved-draft-step"));
            } else {
                assert_eq!(engine.history_lengths(), [1, 0]);
                assert_eq!(app.mask_step, None);
                if origin == edit_job::Origin::Group {
                    assert_ne!(app.selected, id);
                    assert_eq!(original.parent.as_deref(), Some(app.selected.as_str()));
                    assert_eq!(
                        app.selection_layers,
                        [app.selected.clone()].into_iter().collect()
                    );
                    assert!(!app.collapsed.contains(&app.selected));
                    assert!(app.layer_clipboard);
                    assert!(!app.mask);
                } else {
                    assert_eq!(app.selected, id);
                    assert!(app.mask);
                    assert!(original
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .iter()
                        .all(|step| step.pixels.depth == 16));
                }
            }
        }
    }
}
