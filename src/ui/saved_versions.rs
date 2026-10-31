use super::*;

pub(super) struct Browser {
    project: String,
    document: String,
    original: PathBuf,
    entries: Vec<crate::file_safety::PreviousVersion>,
    error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
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
    #[test]
    fn dated_chooser_is_view_only_and_pointer_opens_an_independent_unsaved_copy() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.recovery.open = false;
            let dir =
                std::env::temp_dir().join(format!("pb-saved-browser-{}", crate::engine::id()));
            std::fs::create_dir(&dir).unwrap();
            app.connection.state_dir = dir.clone();
            let path = dir.join("Artwork.psd");
            {
                let mut e = app.shared.lock().unwrap();
                e.doc = Document::new_depth(9, 7, depth).unwrap();
                e.doc.layers[0]
                    .pixels
                    .set16(2, 3, [10001, 20003, 40007, 65535]);
                app.selected = e.doc.layers[0].id.clone();
            }
            let prior = app.shared.lock().unwrap().doc.export_png().unwrap();
            server::save(&app.shared, &path).unwrap();
            {
                let mut e = app.shared.lock().unwrap();
                e.doc.layers[0]
                    .pixels
                    .set16(2, 3, [54321, 23459, 45671, 65535]);
                e.doc.revision += 1;
            }
            server::save(&app.shared, &path).unwrap();
            let saved = std::fs::read(&path).unwrap();
            frame(&mut app, &ctx, vec![]);
            let current = app.shared.clone();
            let png = current.lock().unwrap().doc.export_png().unwrap();
            let project = app.project_id.clone();
            app.recover_previous_save();
            assert_eq!(app.saved_versions.as_ref().unwrap().entries.len(), 1);
            let mut position = None;
            for _ in 0..3 {
                let output = frame(&mut app, &ctx, vec![]);
                position = output.shapes.iter().find_map(|shape| {
                    if let egui::Shape::Text(text) = &shape.shape {
                        (text.galley.text() == "Open copy")
                            .then(|| text.pos + text.galley.size() * 0.5)
                    } else {
                        None
                    }
                });
            }
            assert_eq!(current.lock().unwrap().doc.export_png().unwrap(), png);
            assert_eq!(app.project_id, project);
            let pos = position.expect("visible Open copy button");
            for pressed in [true, false] {
                frame(
                    &mut app,
                    &ctx,
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
            for _ in 0..200 {
                frame(&mut app, &ctx, vec![]);
                if app.project_id != project {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_ne!(app.project_id, project);
            let e = app.shared.lock().unwrap();
            assert!(e.path.is_none() && e.file_version.is_none());
            assert_ne!(e.doc.revision, e.saved_revision);
            assert_eq!(e.doc.bit_depth, depth);
            assert_eq!(e.doc.export_png().unwrap(), prior);
            assert!(e.doc.name.contains("Artwork.psd"));
            drop(e);
            assert_eq!(current.lock().unwrap().doc.export_png().unwrap(), png);
            assert_eq!(std::fs::read(&path).unwrap(), saved);
            // Release native test caches before removing this test's private files.
            drop(app);
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
impl PeerBrush {
    pub(super) fn show_previous_saves(&mut self) {
        let (document, path) = {
            let e = self.shared.lock().unwrap();
            (e.doc.id.clone(), e.path.clone())
        };
        let Some(original) = path else {
            self.message = "This project has no saved destination yet".into();
            return;
        };
        let (entries, error) = match crate::file_safety::previous_versions(&original) {
            Ok(entries) => (entries, None),
            Err(error) => (Vec::new(), Some(error)),
        };
        self.saved_versions = Some(Browser {
            project: self.project_id.clone(),
            document,
            original,
            entries,
            error,
        });
    }
    pub(super) fn saved_versions_window(&mut self, ctx: &egui::Context) {
        let Some(browser) = self.saved_versions.take() else {
            return;
        };
        if browser.project != self.project_id
            || browser.document != self.shared.lock().unwrap().doc.id
        {
            return;
        }
        let mut open = true;
        let mut restore = None;
        egui::Window::new("Previous saves").id(egui::Id::new("previous saves")).open(&mut open).resizable(false).default_width(460.).show(ctx, |ui| {
            dialogs::body(ui, 460.);
            ui.label(RichText::new(browser.original.file_name().unwrap_or_default().to_string_lossy()).strong());
            ui.label("Choose a version to inspect in a new unsaved project. Your current project and saved file stay available.");
            if let Some(error) = &browser.error { ui.label(RichText::new(error).color(ACCENT)); }
            if browser.entries.is_empty() { ui.label("No previous Photoshop saves are available for this destination."); }
            for entry in &browser.entries {
                ui.horizontal(|ui| {
                    ui.label(format!("Before save at {} · {:.1} MiB", recovery::timestamp(entry.timestamp_ms), entry.bytes as f64 / 1048576.));
                    if ui.add_enabled(!self.busy, egui::Button::new("Open copy")).clicked() { restore = Some(entry.path.clone()); }
                });
            }
        });
        if let Some(path) = restore {
            self.open_named_job(
                Some(path),
                true,
                Some(
                    browser
                        .original
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned(),
                ),
            );
        } else if open {
            self.saved_versions = Some(browser);
        }
    }
}
