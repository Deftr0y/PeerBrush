//! OS-owned file associations. Guidance never changes defaults or document state.
use super::*;

const WINDOWS_DEFAULTS_URI: &str = "ms-settings:defaultapps";

#[derive(Default)]
pub(super) struct Choice {
    open: bool,
    registration: Option<Result<bool, String>>,
    error: Option<String>,
}

impl Choice {
    pub(super) fn menu(&mut self, ui: &mut egui::Ui) {
        let response = ui.button("PSD/PSB default editor…");
        if response.clicked() {
            self.open = true;
            self.error = None;
            #[cfg(windows)]
            {
                self.registration = Some(crate::packaged::package_family().map(|p| p.is_some()));
            }
            ui.close_menu();
        }
    }

    pub(super) fn show(&mut self, ctx: &egui::Context) {
        self.show_for(ctx, std::env::consts::OS, open_windows_defaults);
    }

    fn show_for(
        &mut self,
        ctx: &egui::Context,
        os: &str,
        mut launch: impl FnMut(&str) -> Result<(), String>,
    ) {
        if !self.open {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.open = false;
            return;
        }
        let mut open = true;
        let mut close = false;
        egui::Window::new("PSD/PSB default editor")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(430.)
            .show(ctx, |ui| {
                ui.label("Choose the app used when you open PSD and PSB files from your file manager.");
                ui.add_space(4.);
                match os {
                    "windows" => {
                        match self.registration.as_ref() {
                            Some(Ok(true)) => { ui.label("This copy has Windows package identity. Its installed package supplies Open with handlers; choose your defaults in Windows Settings."); }
                            Some(Ok(false)) => { ui.label("This is an unpackaged copy. Portable copies do not register themselves in Default apps. If PeerBrush is missing, install a registered build or use Explorer's Open with → Choose another app to select peerbrush.exe."); }
                            _ => { ui.label("Registration status is unavailable. If PeerBrush is missing from Default apps, verify its installation or use Explorer's Open with → Choose another app."); }
                        }
                        ui.label("In Settings → Apps → Default apps, search for .psd, choose PeerBrush, then repeat for .psb. Windows may ask you to confirm each choice.");
                    }
                    "macos" => {
                        ui.label("Keep PeerBrush.app in Applications. In Finder, select a PSD file and choose File → Get Info. Under Open with, choose PeerBrush and click Change All. Repeat with a PSB file.");
                        ui.label("If PeerBrush is missing, choose Other… and locate PeerBrush.app. Open with alone can open one file without changing the default for its type.");
                    }
                    "linux" => {
                        ui.label("In Ubuntu 24.04's Files desktop, right-click a PSD file → Open With…. Choose PeerBrush, enable Always use for this file type, then Open. Repeat for PSB. Older desktops may use Properties → Open With → Set as default; labels vary by file manager.");
                        ui.label("If PeerBrush is absent, its desktop/file-type registration needs installation. A portable executable does not register itself. File → Open PSD/PSB remains available inside PeerBrush.");
                    }
                    _ => { ui.label("Use your file manager's Open with or file-type preferences to select PeerBrush for PSD and PSB separately."); }
                }
                ui.add_space(4.);
                ui.label(RichText::new("Your operating system applies only the choices you confirm.").small().color(MUTED));
                if let Some(error) = &self.error {
                    ui.colored_label(ACCENT, error);
                }
                ui.horizontal(|ui| {
                    if os == "windows" {
                        let response = ui.button("Open Windows Default apps");
                        if response.clicked() {
                            self.error = launch(WINDOWS_DEFAULTS_URI).err();
                        }
                    }
                    let response = ui.button("Close");
                    close = response.clicked();
                });
            });
        self.open = open && !close;
    }
}

fn open_windows_defaults(uri: &str) -> Result<(), String> {
    if uri != WINDOWS_DEFAULTS_URI {
        return Err("Only Windows Default apps settings can be opened here.".into());
    }
    #[cfg(windows)]
    {
        #[link(name = "shell32")]
        extern "system" {
            fn ShellExecuteW(
                window: *mut std::ffi::c_void,
                operation: *const u16,
                file: *const u16,
                parameters: *const u16,
                directory: *const u16,
                show: i32,
            ) -> isize;
        }
        let operation: Vec<u16> = "open\0".encode_utf16().collect();
        let target: Vec<u16> = uri.encode_utf16().chain(Some(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        if result > 32 {
            return Ok(());
        }
    }
    Err("Windows Settings could not be opened. Use Start → Settings → Apps → Default apps.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        choice: &mut Choice,
        ctx: &egui::Context,
        os: &str,
        events: Vec<egui::Event>,
        calls: &mut Vec<String>,
        fail: bool,
    ) -> egui::FullOutput {
        ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(980., 640.))),
                events,
                ..Default::default()
            },
            |ctx| {
                choice.show_for(ctx, os, |uri| {
                    calls.push(uri.into());
                    if fail {
                        Err("Synthetic Settings launch failure".into())
                    } else {
                        Ok(())
                    }
                })
            },
        )
    }

    fn label(output: &egui::FullOutput, text: &str) -> Option<Pos2> {
        output.shapes.iter().find_map(|shape| match &shape.shape {
            egui::Shape::Text(text_shape) if text_shape.galley.job.text == text => {
                Some(text_shape.pos + text_shape.galley.size() * 0.5)
            }
            _ => None,
        })
    }

    fn visible_text_contains(output: &egui::FullOutput, text: &str) -> bool {
        output.shapes.iter().any(|shape| {
            matches!(&shape.shape, egui::Shape::Text(t) if t.galley.job.text.contains(text))
        })
    }

    fn escape() -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }]
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

    #[test]
    fn actual_windows_control_requests_only_the_trusted_uri_and_reports_failure() {
        for registered in [
            Ok(true),
            Ok(false),
            Err("synthetic identity failure".into()),
        ] {
            let guidance = match &registered {
                Ok(true) => "package identity",
                Ok(false) => "Portable copies do not register themselves",
                Err(_) => "Registration status is unavailable",
            };
            let ctx = egui::Context::default();
            let mut choice = Choice {
                open: true,
                registration: Some(registered),
                ..Default::default()
            };
            let mut calls = vec![];
            frame(&mut choice, &ctx, "windows", vec![], &mut calls, false);
            let output = frame(&mut choice, &ctx, "windows", vec![], &mut calls, false);
            assert!(calls.is_empty());
            assert!(visible_text_contains(&output, guidance));
            let point = label(&output, "Open Windows Default apps").unwrap();
            frame(&mut choice, &ctx, "windows", click(point), &mut calls, true);
            assert_eq!(calls, [WINDOWS_DEFAULTS_URI]);
            assert!(choice.open && choice.error.is_some());
            let output = frame(&mut choice, &ctx, "windows", vec![], &mut calls, false);
            assert!(label(&output, "Synthetic Settings launch failure").is_some());
            let point = label(&output, "Open Windows Default apps").unwrap();
            frame(
                &mut choice,
                &ctx,
                "windows",
                click(point),
                &mut calls,
                false,
            );
            assert_eq!(calls, [WINDOWS_DEFAULTS_URI, WINDOWS_DEFAULTS_URI]);
            assert!(choice.error.is_none());
            let output = frame(&mut choice, &ctx, "windows", vec![], &mut calls, false);
            let close = label(&output, "Close").unwrap();
            frame(
                &mut choice,
                &ctx,
                "windows",
                click(close),
                &mut calls,
                false,
            );
            assert!(!choice.open);
        }
        assert!(open_windows_defaults("https://example.invalid").is_err());
    }

    #[test]
    fn other_platforms_show_guidance_without_requesting_windows_settings() {
        for os in ["macos", "linux"] {
            let ctx = egui::Context::default();
            let mut choice = Choice {
                open: true,
                ..Default::default()
            };
            let mut calls = vec![];
            frame(&mut choice, &ctx, os, vec![], &mut calls, false);
            let output = frame(&mut choice, &ctx, os, vec![], &mut calls, false);
            assert!(label(&output, "Open Windows Default apps").is_none());
            assert!(visible_text_contains(
                &output,
                if os == "macos" {
                    "Change All"
                } else {
                    "Always use for this file type"
                }
            ));
            frame(&mut choice, &ctx, os, escape(), &mut calls, false);
            assert!(!choice.open && calls.is_empty());
        }
    }

    #[test]
    fn actual_menu_entry_and_dialog_leave_native_documents_and_history_untouched() {
        for (depth, size) in [(8, Vec2::new(980., 640.)), (16, Vec2::new(1360., 900.))] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.recovery.open = false;
            {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(32, 24, depth).unwrap();
                app.selected = engine.doc.layers[0].id.clone();
                for point in [[8, 8], [16, 16]] {
                    engine.edit("human", &[json!({"op":"paint","layer":app.selected,"points":[point],"radius":3,"color":[233,84,32,255]})], None, None, "Existing stroke").unwrap();
                }
                engine.undo("human").unwrap();
                assert_eq!((engine.undo.len(), engine.redo.len()), (1, 1));
            }
            let before = serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap();
            let draw = |app: &mut PeerBrush, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            draw(&mut app, vec![]);
            let output = draw(&mut app, vec![]);
            let file = label(&output, "File").expect("Actual File menu is visible");
            draw(&mut app, click(file));
            let output = draw(&mut app, vec![]);
            let point =
                label(&output, "PSD/PSB default editor…").expect("Actual File entry is visible");
            draw(&mut app, click(point));
            assert!(app.associations.open);
            let output = draw(&mut app, vec![]);
            assert!(label(&output, "PSD/PSB default editor").is_some());
            draw(&mut app, escape());
            assert!(!app.associations.open);
            let engine = app.shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!((engine.undo.len(), engine.redo.len()), (1, 1));
        }
    }
}
