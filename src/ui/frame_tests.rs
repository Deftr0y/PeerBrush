use super::*;
use std::sync::atomic::Ordering;

#[test]
fn preview_request_retries_without_waiting_for_the_document_mutex() {
    let (mut app, ctx) = workspace_tests::fixture(16);
    let snapshot = app.doc_snapshot.clone();
    let epoch = app.preview_epoch.load(Ordering::Relaxed);
    let job = app.preview_job;
    let shared = app.shared.clone();
    let (locked, acquired) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        let _engine = shared.lock().unwrap();
        locked.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(5)).is_ok()
    });
    acquired.recv_timeout(Duration::from_secs(5)).unwrap();
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.request_preview(ctx, snapshot.view());
    });
    let _ = release.send(());
    assert!(
        holder.join().unwrap(),
        "Preview waited for the document mutex"
    );
    assert_eq!(app.preview_epoch.load(Ordering::Relaxed), epoch);
    assert_eq!(app.preview_job, job);
    let _ = ctx.run(egui::RawInput::default(), |ctx| {
        app.request_preview(ctx, snapshot.view());
    });
    assert_ne!(app.preview_job, job);
}

#[test]
fn clipboard_completions_survive_contention_and_select_only_existing_successful_targets() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace_tests::fixture(depth);
        let base = app.selected.clone();
        let mut layer = crate::engine::Layer::new("Pasted result", "paint", 32, 24);
        layer.pixels = crate::raster::Raster::new_depth(32, 24, depth);
        let pasted = layer.id.clone();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document.layers.push(layer);
        let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let history = app.shared.lock().unwrap().history_lengths();
        let (sender, replies) = std::sync::mpsc::channel();
        app.clipboard.replies = replies;
        sender
            .send(crate::clipboard::Reply {
                document: source.id.clone(),
                result: Ok("Clipboard completed".into()),
                selected: None,
                selected_layers: vec!["removed-result".into(), pasted.clone()],
            })
            .unwrap();
        let shared = app.shared.clone();
        let guard = shared.lock().unwrap();
        app.poll_clipboard(&ctx);
        assert_eq!(app.selected, base);
        drop(guard);
        app.poll_clipboard(&ctx);
        assert_eq!(app.selected, pasted);
        assert_eq!(app.selection_layers, [pasted.clone()].into_iter().collect());
        assert!(app.layer_clipboard);
        assert_eq!(app.message.as_ref(), "Clipboard completed");
        sender
            .send(crate::clipboard::Reply {
                document: source.id.clone(),
                result: Err("Clipboard refused".into()),
                selected: Some(base.clone()),
                selected_layers: vec![],
            })
            .unwrap();
        app.poll_clipboard(&ctx);
        assert_eq!(app.selected, pasted);
        assert_eq!(app.message.as_ref(), "Clipboard refused");
        sender
            .send(crate::clipboard::Reply {
                document: crate::engine::id(),
                result: Ok("Stale completion".into()),
                selected: Some(base.clone()),
                selected_layers: vec![],
            })
            .unwrap();
        sender
            .send(crate::clipboard::Reply {
                document: source.id.clone(),
                result: Ok("Removed target".into()),
                selected: Some("removed-result".into()),
                selected_layers: vec![],
            })
            .unwrap();
        app.poll_clipboard(&ctx);
        assert_eq!(app.selected, pasted);
        assert_eq!(app.message.as_ref(), "Removed target");
        let engine = shared.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
            serde_json::to_value(source).unwrap()
        );
        assert_eq!(engine.history_lengths(), history);
    }
}

#[test]
fn panel_requests_wait_for_source_locks_and_preserve_existing_filter_drafts() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace_tests::fixture(depth);
        let source = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
        let history = app.shared.lock().unwrap().history_lengths();
        let root = app.workspace_root.clone();
        root.lock().unwrap().capture_panel = Some("filters".into());
        {
            let held = root.lock().unwrap();
            app.poll_capture_panel(&ctx);
            assert_eq!(held.capture_panel.as_deref(), Some("filters"));
            assert!(app.filter_editor.is_none());
        }
        let workspace = app.workspace.clone();
        {
            let _held = workspace.lock().unwrap();
            app.poll_capture_panel(&ctx);
            assert_eq!(
                root.lock().unwrap().capture_panel.as_deref(),
                Some("filters")
            );
            assert!(app.filter_editor.is_none());
        }
        let other =
            crate::workspace::register_in(&workspace, crate::engine::Engine::new(), None).unwrap();
        let other_id = other.lock().unwrap().project_id.clone();
        crate::workspace::select_in(&workspace, &other_id).unwrap();
        app.poll_capture_panel(&ctx);
        assert_eq!(
            root.lock().unwrap().capture_panel.as_deref(),
            Some("filters")
        );
        assert!(app.filter_editor.is_none());
        crate::workspace::select_in(&workspace, &app.project_id).unwrap();
        app.poll_capture_panel(&ctx);
        assert!(root.lock().unwrap().capture_panel.is_none());
        let editor = app.filter_editor.as_mut().unwrap();
        assert_eq!(editor.document, source.id);
        editor.weight = 0.37;
        editor.settings = json!({"radius":17.});
        let command = editor.command();
        root.lock().unwrap().capture_panel = Some("filters".into());
        app.poll_capture_panel(&ctx);
        assert_eq!(app.filter_editor.as_ref().unwrap().command(), command);
        root.lock().unwrap().capture_panel = Some("document_sources".into());
        app.poll_capture_panel(&ctx);
        assert_eq!(app.read_only_info.as_deref(), Some(source.id.as_str()));
        let engine = app.shared.lock().unwrap();
        assert_eq!(
            serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(),
            serde_json::to_value(source).unwrap()
        );
        assert_eq!(engine.history_lengths(), history);
    }
}

#[test]
fn frame_panels_and_eraser_shortcut_accept_native_snapshots_without_resident_conversion() {
    for depth in [8, 16] {
        let (mut app, ctx) = workspace_tests::fixture(depth);
        let mut doc = Document::new_depth(19, 13, depth).unwrap();
        doc.layers[0].name = "Native frame layer".into();
        app.shared.lock().unwrap().resident_state_mut().expect("resident fixture").document = doc.clone();
        app.select_content(&doc.layers[0].id);
        app.tool = Tool::None;
        let pool = crate::large_document::ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let native = crate::tree::presentation_tests::native(&doc, &pool);
        let key = native.key();
        let original = serde_json::to_value(native.metadata()).unwrap();
        let saved = serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap();
        let history = app.shared.lock().unwrap().history_lengths();
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300., 1000.))),
                ..Default::default()
            },
            |ctx| {
                let frame = DocumentFrame {
                    document: native.clone().into(),
                    ..DocumentFrame::capture(&app.shared.lock().unwrap())
                };
                app.draw_document(ctx, frame, 0, None);
            },
        );
        assert!(matches!(app.doc_snapshot, DocumentSnapshot::Native(_)));
        assert!(app.thumb_native);
        assert_eq!(app.doc_snapshot.view().native().unwrap().key(), key);
        assert!(app.layer_rects.contains_key(&doc.layers[0].id));
        assert!(app.opacity_rect.is_some());
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == "Native frame layer")));
        let _ = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Key {
                    key: egui::Key::E,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers {
                        ctrl: true,
                        command: true,
                        ..Default::default()
                    },
                }],
                ..Default::default()
            },
            |ctx| app.document_shortcuts(ctx, &native),
        );
        assert!(app.tool == Tool::Eraser);
        assert_eq!(serde_json::to_value(native.metadata()).unwrap(), original);
        assert_eq!(
            serde_json::to_value(&app.shared.lock().unwrap().resident_state().expect("resident fixture").document).unwrap(),
            saved
        );
        assert_eq!(app.shared.lock().unwrap().history_lengths(), history);
        app.layer_thumbnails.stop();
        app.mask_thumbnails.stop();
    }
}

#[test]
fn imported_closed_folders_start_collapsed_and_keep_manual_expansion() {
    for native in [false, true] {
        let (mut app, ctx) = workspace_tests::fixture(16);
        let mut doc = Document::new_depth(19, 13, 16).unwrap();
        let mut folder = crate::engine::Layer::new("Imported folder", "group", 19, 13);
        folder.psd_metadata.push(crate::psd::LayerMetadata {
            key: "lsct:closed".into(),
            data: 2u32.to_be_bytes().to_vec(),
        });
        doc.layers[0].parent = Some(folder.id.clone());
        doc.layers.insert(0, folder);
        doc.ensure_depth();
        let reopened = crate::psd::decode(&crate::psd::encode(&doc).unwrap()).unwrap();
        let folder = reopened.layers[0].id.clone();
        let child = reopened.layers[1].id.clone();
        let pool = crate::large_document::ResidentBudget::new(32 * 1024 * 1024).unwrap();
        let snapshot: DocumentSnapshot = if native {
            crate::tree::presentation_tests::native(&reopened, &pool).into()
        } else {
            reopened.clone().into()
        };
        app.shared.lock().unwrap().resident_state_mut().unwrap().document = reopened;
        app.select_content(&folder);
        let draw = |app: &mut PeerBrush| {
            let _ = ctx.run(egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1300., 1000.))),
                ..Default::default()
            }, |ctx| {
                let mut frame = DocumentFrame::capture(&app.shared.lock().unwrap());
                frame.document = snapshot.clone();
                app.draw_document(ctx, frame, 0, None);
            });
        };
        let before = app.shared.lock().unwrap().history_lengths();
        draw(&mut app);
        assert!(app.collapsed.contains(&folder));
        assert!(!app.layer_rects.contains_key(&child));
        app.collapsed.remove(&folder);
        draw(&mut app);
        assert!(app.layer_rects.contains_key(&child));
        assert!(snapshot.view().find_layer(&folder).unwrap().photoshop_folder_is_closed());
        assert_eq!(app.shared.lock().unwrap().history_lengths(), before);
    }
}

#[test]
fn native_protection_reports_unavailable_inspection_and_keeps_original_warnings() {
    let (mut app, ctx) = workspace_tests::fixture(16);
    let mut doc = Document::new_depth(9, 7, 16).unwrap();
    doc.read_only = true;
    doc.warnings = vec!["Unsupported Photoshop feature: preserved-example".into()];
    let pool = crate::large_document::ResidentBudget::new(16 * 1024 * 1024).unwrap();
    let native = crate::tree::presentation_tests::native(&doc, &pool);
    let original = serde_json::to_value(native.metadata()).unwrap();
    for patterns in [false, true] {
        app.read_only_info = (!patterns).then(|| doc.id.clone());
        app.read_only_patterns = patterns.then(|| doc.id.clone());
        let output = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1100., 1000.))),
                ..Default::default()
            },
            |ctx| {
                if patterns {
                    app.patterns_window(ctx, &native);
                } else {
                    app.protection_window(ctx, &native);
                }
            },
        );
        assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text.contains("inspection is unavailable"))));
        if !patterns {
            assert!(output.shapes.iter().any(|shape| matches!(&shape.shape, egui::Shape::Text(text) if text.galley.job.text == doc.warnings[0])));
        }
        assert_eq!(serde_json::to_value(native.metadata()).unwrap(), original);
        assert!(native.metadata().read_only());
    }
}
