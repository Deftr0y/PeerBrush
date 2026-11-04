//! Running application metadata, independent of document information and updates.
use super::*;

#[derive(serde::Deserialize)]
struct Metadata {
    version: String,
    commit: String,
    tree: String,
}

pub(super) struct About {
    pub open: bool,
    metadata: Option<Metadata>,
}

fn version_status(version: &str) -> &'static str {
    match semver::Version::parse(version) {
        Ok(version) if version.pre.as_str().split('.').any(|part| part == "dev") => {
            "Development build"
        }
        Ok(version) if !version.pre.is_empty() => "Prerelease build",
        Ok(_) => "",
        Err(_) => "Version status unknown",
    }
}

impl About {
    pub fn load() -> Self {
        Self {
            open: false,
            metadata: serde_json::from_str(crate::build_info::json()).ok(),
        }
    }
    pub fn menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("About PeerBrush…").clicked() {
            self.open = true;
            ui.close_menu();
        }
    }
    pub fn dismiss_escape(&mut self, ctx: &egui::Context) {
        let layer = egui::LayerId::new(egui::Order::Middle, egui::Id::new("application_info"));
        if self.open
            && ctx.top_layer_id() == Some(layer)
            && ctx.memory(|memory| !memory.any_popup_open() && memory.top_modal_layer().is_none())
            && ctx.input_mut(|input| input.consume_key(input.modifiers, egui::Key::Escape))
        {
            self.open = false;
        }
    }
    pub fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new("About PeerBrush")
            .id(egui::Id::new("application_info"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.)
            .show(ctx, |ui| {
                ui.heading("PeerBrush");
                if let Some(metadata) = &self.metadata {
                    ui.label(format!("Running version {}", metadata.version));
                    let status = version_status(&metadata.version);
                    if !status.is_empty() {
                        ui.label(status);
                    }
                    if metadata.tree == "dirty" {
                        ui.label("Local changes · dirty build");
                    }
                    if metadata.tree == "unknown" || metadata.commit == "unknown" {
                        ui.label("Source revision unknown");
                    } else {
                        ui.label("Source revision");
                        ui.add(
                            egui::Label::new(RichText::new(&metadata.commit).monospace())
                                .selectable(true),
                        );
                    }
                } else {
                    ui.label("Build metadata unavailable");
                }
                ui.label(format!(
                    "{} / {}",
                    std::env::consts::OS,
                    std::env::consts::ARCH
                ));
                ui.label("GNU GPL v3");
                close = ui.button("Close").clicked();
            });
        self.open = open && !close;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(output: &egui::FullOutput, expected: &str) -> Option<Pos2> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text) if text.galley.job.text == expected => {
                Some(text.pos + text.galley.size() * 0.5)
            }
            _ => None,
        })
    }
    fn click(point: Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(point),
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos: point,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    }
    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
            events,
            ..Default::default()
        }
    }

    #[test]
    fn actual_menu_running_metadata_close_escape_and_capture_preserve_native_sources() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.recovery.open = false;
            {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(32, 24, depth).unwrap();
                app.selected = engine.doc.layers[0].id.clone();
                for point in [[8, 8], [16, 16]] {
                    engine.edit("human", &[json!({"op":"paint","layer":app.selected,"points":[point],"radius":3,"color":[233,84,32,255]})], None, None, "Human artwork").unwrap();
                }
                engine.undo("human").unwrap();
            }
            let (before, pixels, undo, redo) = {
                let engine = app.shared.lock().unwrap();
                (
                    serde_json::to_value(&engine.doc).unwrap(),
                    engine.doc.export_png().unwrap(),
                    engine.undo.len(),
                    engine.redo.len(),
                )
            };
            let frame = |app: &mut PeerBrush, events| ctx.run(input(events), |ctx| app.draw(ctx));
            frame(&mut app, vec![]);
            let output = frame(&mut app, vec![]);
            frame(
                &mut app,
                click(label(&output, "File").expect("Actual File menu")),
            );
            let output = frame(&mut app, vec![]);
            frame(
                &mut app,
                click(label(&output, "About PeerBrush…").expect("Actual About entry")),
            );
            assert!(app.about.open);
            frame(&mut app, vec![]);
            let output = frame(&mut app, vec![]);
            let metadata: serde_json::Value =
                serde_json::from_str(crate::build_info::json()).unwrap();
            assert!(label(
                &output,
                &format!("Running version {}", metadata["version"].as_str().unwrap())
            )
            .is_some());
            assert!(label(&output, "GNU GPL v3").is_some());
            frame(
                &mut app,
                click(label(&output, "Close").expect("Actual Close button")),
            );
            assert!(!app.about.open);
            // Exercise the same settled capture-panel route used by native QA.
            app.workspace_root.lock().unwrap().capture_panel = Some("application_info".into());
            frame(&mut app, vec![]);
            assert!(app.about.open);
            app.live_gesture = Some("unfinished human gesture".into());
            frame(
                &mut app,
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
            assert!(!app.about.open);
            assert_eq!(
                app.live_gesture.as_deref(),
                Some("unfinished human gesture")
            );
            let engine = app.shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!(engine.doc.export_png().unwrap(), pixels);
            assert_eq!((engine.undo.len(), engine.redo.len()), (undo, redo));
            assert!(!engine.closed);
        }
    }

    #[test]
    fn displayed_build_statuses_do_not_claim_a_published_or_verified_package() {
        for (version, commit, tree, expected) in [
            (
                "0.30.0-dev.1",
                "0123456789abcdef0123456789abcdef01234567",
                "clean",
                "Development build",
            ),
            (
                "0.30.0-rc.1",
                "0123456789abcdef0123456789abcdef01234567",
                "clean",
                "Prerelease build",
            ),
            (
                "0.30.0",
                "0123456789abcdef0123456789abcdef01234567",
                "dirty",
                "Local changes · dirty build",
            ),
            ("0.30.0", "unknown", "unknown", "Source revision unknown"),
        ] {
            let ctx = egui::Context::default();
            let mut about = About {
                open: true,
                metadata: Some(Metadata {
                    version: version.into(),
                    commit: commit.into(),
                    tree: tree.into(),
                }),
            };
            let mut output = ctx.run(input(vec![]), |ctx| about.show(ctx));
            for _ in 0..2 {
                output = ctx.run(input(vec![]), |ctx| about.show(ctx));
            }
            assert!(label(&output, &format!("Running version {version}")).is_some());
            assert!(label(&output, expected).is_some(), "{version}/{tree}");
            if commit != "unknown" {
                assert!(label(&output, commit).is_some());
            }
        }
    }
}
