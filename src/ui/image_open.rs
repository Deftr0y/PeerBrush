use super::*;

pub(super) struct Pending {
    pub shared: Shared,
    pub prepared: crate::image_open::Prepared,
    pub control: crate::loading::Control,
    pub initiating: String,
    pub index: usize,
}

pub(super) fn finish(
    pending: Pending,
    workspace: &crate::workspace::Registry,
) -> Result<String, String> {
    let mut options = json!({});
    if let Some(choice) = pending.prepared.encoded.info.choice {
        options[choice] = json!(pending.index);
    }
    server::open_prepared_image(
        &pending.shared,
        pending.prepared,
        &pending.control,
        &options,
    )?;
    let engine = pending.shared.lock().unwrap().clone();
    crate::workspace::register_in(workspace, engine, Some(&pending.initiating))?;
    Ok("Image opened — save as PSD or PSB".into())
}

impl PeerBrush {
    pub(super) fn image_open_dialog(&mut self, ctx: &egui::Context) {
        while let Ok(pending) = self.image_open_rx.try_recv() {
            self.pending_image_open = Some(pending);
        }
        if self.pending_image_open.as_ref().is_some_and(|p| {
            p.control.check().is_err()
                || crate::workspace::get_in(&self.workspace, &p.initiating).is_err()
        }) {
            self.pending_image_open = None;
            self.message = "Open canceled · current projects preserved".into();
        }
        let Some(pending) = &mut self.pending_image_open else {
            return;
        };
        let info = &pending.prepared.encoded.info;
        let choice = info.choice.unwrap_or("content");
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        egui::Window::new("Open image content")
            .collapsible(false)
            .resizable(false)
            .open(&mut open)
            .show(ctx, |ui| {
                dialogs::body(ui, 420.);
                ui.label(
                    pending
                        .prepared
                        .path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                );
                ui.label("Choose the content to open in a new project.");
                egui::ComboBox::from_id_salt("open image content")
                    .selected_text(format!("{choice} {} of {}", pending.index + 1, info.count))
                    .show_ui(ui, |ui| {
                        for index in 0..info.count {
                            ui.selectable_value(
                                &mut pending.index,
                                index,
                                format!("{choice} {}", index + 1),
                            );
                        }
                    });
                ui.horizontal(|ui| {
                    apply = ui
                        .add_enabled(!self.busy, dialogs::primary("Open"))
                        .clicked();
                    cancel = ui.button("Cancel").clicked();
                });
            });
        if cancel || !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.pending_image_open = None;
            self.message = "Open canceled · current projects preserved".into();
        } else if apply {
            let pending = self.pending_image_open.take().unwrap();
            let workspace = self.workspace.clone();
            self.load_control = Some(pending.control.clone());
            self.job(move || finish(pending, &workspace));
        }
    }
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
    fn pending(app: &PeerBrush, path: &Path) -> Pending {
        let shared = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
        let control = crate::loading::Control::default();
        let e = shared.lock().unwrap();
        control.bind(e.doc.id.clone(), e.doc.revision);
        let prepared = crate::image_open::Prepared::read(path, &e.project_id, &control).unwrap();
        drop(e);
        Pending {
            shared,
            prepared,
            control,
            initiating: app.project_id.clone(),
            index: 1,
        }
    }
    #[test]
    fn native_choice_cancel_and_open_preserve_existing_work_and_file_changes_are_rejected() {
        let (mut app, ctx) = super::super::tests::fixture();
        app.recovery.open = false;
        let path = std::env::temp_dir().join(format!("pb-open-choice-{}.gif", crate::engine::id()));
        let mut bytes = Vec::new();
        let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
        encoder
            .encode_frames(
                [[255, 0, 0, 255], [0, 255, 0, 255]].into_iter().map(|c| {
                    image::Frame::new(image::RgbaImage::from_pixel(12, 9, image::Rgba(c)))
                }),
            )
            .unwrap();
        drop(encoder);
        std::fs::write(&path, &bytes).unwrap();
        let root = app.shared.clone();
        let before = root.lock().unwrap().state();
        app.pending_image_open = Some(pending(&app, &path));
        frame(&mut app, &ctx, vec![]);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            }],
        );
        assert!(app.pending_image_open.is_none());
        assert_eq!(crate::workspace::handles(&root).len(), 1);
        assert_eq!(root.lock().unwrap().state(), before);
        let changed = pending(&app, &path);
        std::fs::write(&path, b"changed source").unwrap();
        assert!(finish(changed, &app.workspace).is_err());
        assert_eq!(crate::workspace::handles(&root).len(), 1);
        std::fs::write(&path, &bytes).unwrap();
        app.pending_image_open = Some(pending(&app, &path));
        let mut output = frame(&mut app, &ctx, vec![]);
        for _ in 0..3 {
            output = frame(&mut app, &ctx, vec![]);
        }
        let pos = output
            .shapes
            .iter()
            .find_map(|s| match &s.shape {
                egui::Shape::Text(t) if t.galley.job.text == "Open" => {
                    Some(t.pos + t.galley.size() * 0.5)
                }
                _ => None,
            })
            .expect("Visible Open choice button");
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
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while crate::workspace::handles(&root).len() < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            frame(&mut app, &ctx, vec![]);
        }
        assert_eq!(crate::workspace::handles(&root).len(), 2);
        let opened = crate::workspace::handles(&root)
            .into_iter()
            .find(|s| !Arc::ptr_eq(s, &root))
            .unwrap();
        let e = opened.lock().unwrap();
        assert_eq!((e.doc.width, e.doc.height), (12, 9));
        assert_eq!(e.doc.layers[0].pixels.get(4, 4), [0, 255, 0, 255]);
        assert_eq!(root.lock().unwrap().state(), before);
        drop(e);
        drop(opened);
        drop(app);
        drop(root);
        std::fs::remove_file(path).unwrap();
    }
}
