use super::*;

pub(super) struct Pending {
    pub source: Source,
    pub control: crate::loading::Control,
    pub initiating: String,
    pub index: usize,
}

pub(super) enum Source {
    Resident {
        shared: Shared,
        prepared: crate::image_open::Prepared,
        icon_sizes: Vec<[u16; 2]>,
    },
    Native(server::ImageChoice),
}
impl Source {
    pub(super) fn choice(&self) -> (Option<&'static str>, usize) {
        match self {
            Self::Resident { prepared, .. } => {
                (prepared.encoded.info.choice, prepared.encoded.info.count)
            }
            Self::Native(choice) => (Some(choice.key), choice.count),
        }
    }
    fn path(&self) -> &Path {
        match self {
            Self::Resident { prepared, .. } => &prepared.path,
            Self::Native(choice) => choice.path(),
        }
    }
    fn icon_sizes(&self) -> &[[u16; 2]] {
        match self {
            Self::Resident { icon_sizes, .. } => icon_sizes,
            Self::Native(choice) => &choice.sizes,
        }
    }
}

pub(super) fn content_choice(
    ui: &mut egui::Ui,
    choice: &str,
    count: usize,
    sizes: &[[u16; 2]],
    index: &mut usize,
) {
    let label = match choice {
        "frame" => "Frame",
        "page" => "Page",
        "variant" => "Icon variant",
        _ => "Image content",
    };
    let subject = label.to_lowercase();
    let mut number = index.saturating_add(1).min(count);
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        if count == 0 {
            ui.weak("No choices available");
            return;
        }
        if ui.add_enabled_ui(number > 1, |ui| {
            icons::small_button(ui, Icon::Left, &format!("Previous {subject}"))
        }).inner.clicked() {
            number -= 1;
        }
        let response = controls::numeric(ui, &mut number, 1..=count)
            .on_hover_text(format!("Enter a {subject} number from 1 to {count}"));
        response.widget_info(|| egui::WidgetInfo {
            label: Some(label.into()),
            ..egui::WidgetInfo::drag_value(response.enabled(), number as f64)
        });
        ui.label(format!("of {count}"));
        if ui.add_enabled_ui(number < count, |ui| {
            icons::small_button(ui, Icon::Right, &format!("Next {subject}"))
        }).inner.clicked() {
            number += 1;
        }
        if let Some(&[width, height]) = sizes.get(number - 1).filter(|_| sizes.len() == count) {
            let menu = egui::ComboBox::from_id_salt("icon size")
                .selected_text(format!("{width} × {height}"))
                .height(180.)
                .show_ui(ui, |ui| {
                    for (i, &[width, height]) in sizes.iter().enumerate() {
                        ui.selectable_value(&mut number, i + 1, format!("{} · {} × {}", i + 1, width, height));
                    }
                });
            menu.response.widget_info(|| {
                let mut info = egui::WidgetInfo::labeled(
                    egui::WidgetType::ComboBox, menu.response.enabled(), "Icon size",
                );
                let [width, height] = sizes[number - 1];
                info.current_text_value = Some(format!("{width} × {height}"));
                info
            });
        } else if count > 2 {
            let response = ui.add(egui::Slider::new(&mut number, 1..=count).show_value(false))
                .on_hover_text(format!("Scrub {subject}s"));
            response.widget_info(|| egui::WidgetInfo::slider(
                response.enabled(), number as f64, format!("{label} scrubber"),
            ));
            icons::paint_focus(ui, &response);
        }
    });
    *index = number.saturating_sub(1);
}

pub(super) fn finish(
    pending: Pending,
    workspace: &crate::workspace::Registry,
) -> Result<String, String> {
    match pending.source {
        Source::Resident { shared, prepared, .. } => {
            let mut options = json!({});
            if let Some(choice) = prepared.encoded.info.choice {
                options[choice] = json!(pending.index);
            }
            server::open_prepared_image(&shared, prepared, &pending.control, &options)?;
            crate::workspace::register_opened(workspace, shared, Some(&pending.initiating))?;
        }
        Source::Native(choice) => {
            choice.finish(
                workspace,
                &pending.control,
                &pending.initiating,
                pending.index,
            )?;
        }
    }
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
        let (choice, count) = pending.source.choice();
        let choice = choice.unwrap_or("content");
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
                        .source
                        .path()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy(),
                );
                let sizes = pending.source.icon_sizes();
                content_choice(ui, choice, count, sizes, &mut pending.index);
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
        control.bind(
            e.resident_state()
                .expect("resident fixture")
                .document
                .id
                .clone(),
            e.resident_state()
                .expect("resident fixture")
                .document
                .revision,
        );
        let prepared = crate::image_open::Prepared::read(path, &e.project_id, &control).unwrap();
        drop(e);
        Pending {
            source: Source::Resident { shared, prepared, icon_sizes: Vec::new() },
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
        let before = crate::server::state(&root).unwrap();
        app.pending_image_open = Some(pending(&app, &path));
        crate::activation::quit_signal(&app.workspace)
            .store(true, std::sync::atomic::Ordering::Release);
        frame(&mut app, &ctx, vec![]);
        assert!(app.pending_image_open.is_some());
        assert!(app.lifecycle.is_none() && !app.exit_approved);
        assert!(!root.lock().unwrap().closed);
        assert_eq!(crate::server::state(&root).unwrap(), before);
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
        assert_eq!(crate::server::state(&root).unwrap(), before);
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
        assert_eq!(
            (
                e.resident_state().expect("resident fixture").document.width,
                e.resident_state()
                    .expect("resident fixture")
                    .document
                    .height
            ),
            (12, 9)
        );
        assert_eq!(
            e.resident_state()
                .expect("resident fixture")
                .document
                .layers[0]
                .pixels
                .get(4, 4),
            [0, 255, 0, 255]
        );
        assert_eq!(crate::server::state(&root).unwrap(), before);
        drop(e);
        drop(opened);
        drop(app);
        drop(root);
        std::fs::remove_file(path).unwrap();
    }
}
