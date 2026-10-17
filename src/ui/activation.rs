use super::*;

impl PeerBrush {
    pub(super) fn open_activated_file(&mut self, ctx: &egui::Context) {
        // Keep requests while a dialog, file job, close review or gesture is active.
        // In particular, drain image choices before starting the next queued file.
        if self.busy
            || !self.jobs.is_empty()
            || self.load_control.is_some()
            || self.pending_image_open.is_some()
            || self.pending_import.is_some()
            || self.layer_drag.is_some()
            || self.eye_sweep.is_some()
            || self.tab_transfer.is_some()
            || self.tab_transfer_committing
            || self
                .export_editor
                .as_ref()
                .is_some_and(|editor| editor.project == self.project_id)
            || self.color_editor.is_some()
            || self.rename_edit.is_some()
            || self.geometry.is_some()
            || self.source_editor.is_some()
            || self.filter_editor.is_some()
            || self.refinement.is_some()
            || self.task_undo_review.is_some()
            || self.proposal_review.is_some()
            || self.project_settings.is_some()
            || !self.selection_path.is_empty()
            || self.open_file_warning.is_some()
            || self.saved_versions.is_some()
            || self.recovery.open
            || self.save_loss_review.is_some()
            || self.lifecycle.is_some()
            || self.lifecycle_frame
            || self.exit_approved
            || self.parameter_gesture.is_some()
            || self.live_gesture.is_some()
            || !self.points.is_empty()
            || ctx.input(|i| i.pointer.any_down())
        {
            return;
        }
        if let Some(path) = crate::activation::next(&self.workspace) {
            // The existing opener handles PSD/PSB protection, native depth, file
            // ownership, image-content choices and the workspace's project limit.
            self.open_job(Some(path), false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        activation,
        engine::{Document, Engine},
        workspace,
    };

    fn frame(app: &mut PeerBrush, ctx: &egui::Context) {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        );
    }

    fn settle(app: &mut PeerBrush, ctx: &egui::Context) {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            frame(app, ctx);
            if app.jobs.is_empty() && app.load_control.is_none() {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "file activation did not finish"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn queued_files_wait_for_recovery_review_to_close() {
        let (mut app, ctx) = super::super::tests::fixture();
        let root = app.workspace_root.clone();
        let path = std::env::temp_dir().join(format!("after-recovery-{}.psd", crate::engine::id()));
        let before = root.lock().unwrap().state();
        app.recovery.open = true;
        activation::enqueue(&root, vec![path.clone()]).unwrap();
        app.open_activated_file(&ctx);
        assert!(app.jobs.is_empty());
        assert_eq!(activation::next(&app.workspace), Some(path.clone()));
        activation::enqueue(&root, vec![path]).unwrap();
        app.recovery.open = false;
        settle(&mut app, &ctx);
        assert!(activation::next(&app.workspace).is_none());
        assert_eq!(root.lock().unwrap().state(), before);
    }

    #[test]
    fn automatic_activation_preserves_unfinished_rename_and_selection() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let root = app.workspace_root.clone();
        let path = std::env::temp_dir().join(format!("after-edit-{}.psd", crate::engine::id()));
        let before = root.lock().unwrap().state();
        let doc = root.lock().unwrap().doc.clone();
        app.rename_edit = Some(RenameEdit {
            document: doc.id,
            layer: doc.layers[0].id.clone(),
            original: doc.layers[0].name.clone(),
            text: "Uncommitted rename".into(),
            revision: doc.revision,
            focus: false,
        });
        activation::enqueue(&root, vec![path.clone()]).unwrap();
        app.open_activated_file(&ctx);
        assert_eq!(app.rename_edit.as_ref().unwrap().text, "Uncommitted rename");
        assert_eq!(activation::next(&app.workspace), Some(path.clone()));
        activation::enqueue(&root, vec![path.clone()]).unwrap();
        app.rename_edit = None;
        app.selection_path = vec![[1.0, 2.0], [3.0, 4.0]];
        app.open_activated_file(&ctx);
        assert_eq!(app.selection_path.len(), 2);
        assert_eq!(activation::next(&app.workspace), Some(path.clone()));
        activation::enqueue(&root, vec![path]).unwrap();
        app.selection_path.clear();
        settle(&mut app, &ctx);
        assert!(activation::next(&app.workspace).is_none());
        assert_eq!(root.lock().unwrap().state(), before);
    }

    #[test]
    fn queued_files_wait_for_document_bound_color_editor() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let root = app.workspace_root.clone();
        let path = std::env::temp_dir().join(format!("after-color-{}.psd", crate::engine::id()));
        let layer = root.lock().unwrap().doc.layers[0].id.clone();
        app.open_color(color::Target::Layer(layer), [20, 30, 40, 255]);
        activation::enqueue(&root, vec![path.clone()]).unwrap();
        app.open_activated_file(&ctx);
        assert!(app.color_editor.is_some());
        assert_eq!(activation::next(&app.workspace), Some(path.clone()));
        activation::enqueue(&root, vec![path]).unwrap();
        app.color_editor = None;
        settle(&mut app, &ctx);
        assert!(activation::next(&app.workspace).is_none());
    }

    #[test]
    fn native_activation_opens_new_tab_preserves_dirty_work_and_reuses_duplicate() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let root = app.workspace_root.clone();
        {
            let mut e = root.lock().unwrap();
            e.doc = Document::new(12, 9).unwrap();
            let layer = e.doc.layers[0].id.clone();
            e.edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"name":"Unsaved work"})],
                None,
                None,
                "Rename",
            )
            .unwrap();
        }
        let before = root.lock().unwrap().state();
        let history = root.lock().unwrap().undo.len();
        let path =
            std::env::temp_dir().join(format!("作品 with spaces-{}.psd", crate::engine::id()));
        let mut doc = Document::new_depth(8, 6, 16).unwrap();
        doc.layers[0]
            .pixels
            .set16(2, 3, [60001, 12347, 34569, 65535]);
        std::fs::write(&path, crate::psd::encode(&doc).unwrap()).unwrap();
        server::dispatch(&root, "native_open", &json!({"paths":[path]})).unwrap();
        settle(&mut app, &ctx);
        assert_eq!(workspace::handles(&root).len(), 2);
        let opened = workspace::handles(&root)
            .into_iter()
            .find(|p| !Arc::ptr_eq(p, &root))
            .unwrap();
        {
            let e = opened.lock().unwrap();
            assert_eq!(e.doc.bit_depth, 16);
            assert_eq!(
                e.doc.layers[0].pixels.get16(2, 3),
                [60001, 12347, 34569, 65535]
            );
        }
        assert_eq!(root.lock().unwrap().state(), before);
        assert_eq!(root.lock().unwrap().undo.len(), history);
        activation::enqueue(&root, vec![path.clone()]).unwrap();
        settle(&mut app, &ctx);
        assert_eq!(workspace::handles(&root).len(), 2);
        assert_eq!(root.lock().unwrap().state(), before);
        assert_eq!(
            workspace::active_id(&root),
            opened.lock().unwrap().project_id
        );
        drop(opened);
        drop(app);
        drop(root);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn activation_waits_for_file_jobs_and_invalid_file_preserves_projects() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let root = app.workspace_root.clone();
        let before = root.lock().unwrap().state();
        let path = std::env::temp_dir().join(format!("missing-{}.psd", crate::engine::id()));
        activation::enqueue(&root, vec![path.clone()]).unwrap();
        app.jobs.insert("another project".into());
        frame(&mut app, &ctx);
        assert_eq!(activation::next(&app.workspace), Some(path.clone()));
        activation::enqueue(&root, vec![path]).unwrap();
        app.jobs.clear();
        settle(&mut app, &ctx);
        assert_eq!(workspace::handles(&root).len(), 1);
        assert_eq!(root.lock().unwrap().state(), before);
        assert_ne!(app.message, "Ready");
        // No accidental replacement even when another project exists.
        workspace::register(&root, Engine::new(), None).unwrap();
        assert_eq!(root.lock().unwrap().state(), before);
    }

    #[test]
    fn queued_images_wait_for_explicit_content_choice_and_continue_after_cancel() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let root = app.workspace_root.clone();
        let before = root.lock().unwrap().state();
        let directory =
            std::env::temp_dir().join(format!("pb-image-activation-{}", crate::engine::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let animated = directory.join("two frames.gif");
        let still = directory.join("next image.png");
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder
            .encode_frames(
                [[255, 0, 0, 255], [0, 255, 0, 255]]
                    .into_iter()
                    .map(|color| {
                        image::Frame::new(image::RgbaImage::from_pixel(4, 3, image::Rgba(color)))
                    }),
            )
            .unwrap();
        drop(encoder);
        std::fs::write(&animated, bytes).unwrap();
        std::fs::write(
            &still,
            crate::raster::png(3, 2, &[20, 40, 60, 255].repeat(6)).unwrap(),
        )
        .unwrap();
        activation::enqueue(&root, vec![animated, still.clone()]).unwrap();
        settle(&mut app, &ctx);
        assert!(app.pending_image_open.is_some());
        for _ in 0..3 {
            frame(&mut app, &ctx);
        }
        assert_eq!(
            workspace::handles(&root).len(),
            1,
            "no implicit frame selection or later-file opening"
        );
        assert_eq!(activation::next(&app.workspace), Some(still.clone()));
        activation::enqueue(&root, vec![still]).unwrap();
        app.pending_image_open.as_ref().unwrap().control.cancel();
        settle(&mut app, &ctx);
        assert!(app.pending_image_open.is_none());
        assert_eq!(workspace::handles(&root).len(), 2);
        assert_eq!(root.lock().unwrap().state(), before);
        drop(app);
        drop(root);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
