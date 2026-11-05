use super::*;

#[cfg(target_os = "macos")]
mod macos;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    version: u32,
    answered: bool,
}

pub(crate) fn validate_file(path: &Path) -> Result<(), String> {
    if let Some(bytes) = crate::tool_settings::read_owned(path)? {
        let value: Preference = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        if value.version != 1 || !value.answered {
            return Err("Unsupported default-editor preference".into());
        }
    }
    Ok(())
}

pub(super) struct Welcome {
    path: PathBuf,
    original: Option<Vec<u8>>,
    open: bool,
    error: Option<String>,
    icon: Option<TextureHandle>,
    pending: Option<mpsc::Receiver<Result<(), String>>>,
    completed: usize,
}

impl Welcome {
    pub(super) fn load(preferences: &Path) -> Self {
        let path = preferences.join("associations.json");
        let loaded = crate::tool_settings::read_owned(&path);
        let error = loaded
            .as_ref()
            .err()
            .cloned()
            .or_else(|| validate_file(&path).err());
        let original = loaded.ok().flatten();
        Self {
            open: original.is_none() && error.is_none(),
            path,
            original,
            error,
            icon: None,
            pending: None,
            completed: 0,
        }
    }

    pub(super) fn offer(&mut self) {
        self.open = true;
        self.error = None;
    }

    fn dismiss(&mut self) -> Result<(), String> {
        let _lease = crate::file_safety::Lease::acquire(&self.path)?;
        let bytes = serde_json::to_vec(&Preference {
            version: 1,
            answered: true,
        })
        .map_err(|e| e.to_string())?;
        server::atomic_write_checked(&self.path, &bytes, |_| {
            if crate::tool_settings::read_owned(&self.path)? == self.original {
                Ok(())
            } else {
                Err("Default-editor preferences changed on disk; restart before saving".into())
            }
        })?;
        self.original = Some(bytes);
        self.open = false;
        Ok(())
    }

    pub(super) fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if let Some(receiver) = &self.pending {
            while let Ok(result) = receiver.try_recv() {
                self.completed += 1;
                if let Err(error) = result {
                    self.error = Some(error);
                }
            }
            if self.completed == 2 {
                self.pending = None;
                if self.error.is_none() {
                    self.error = self.dismiss().err();
                }
            }
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        if !self.open {
            return;
        }
        if self.icon.is_none() {
            if let Ok(image) =
                image::load_from_memory(include_bytes!("../../../assets/peerbrush-logo.png"))
            {
                let image = image.to_rgba8();
                self.icon = Some(ctx.load_texture(
                    "welcome_peerbrush",
                    egui::ColorImage::from_rgba_unmultiplied(
                        [image.width() as usize, image.height() as usize],
                        image.as_raw(),
                    ),
                    Default::default(),
                ));
            }
        }
        let response = egui::Modal::new(egui::Id::new("first_launch_default_editor")).show(ctx, |ui| {
            dialogs::body(ui, 440.);
            ui.add_space(10.);
            ui.vertical_centered(|ui| {
                if let Some(icon) = &self.icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(Vec2::splat(64.)));
                }
                ui.add_space(12.);
                ui.label(RichText::new("WELCOME TO PEERBRUSH").size(11.).color(ACCENT).strong());
                ui.add_space(8.);
                ui.label(RichText::new("Your files. Your editor.").size(27.).strong());
                ui.add_space(10.);
                ui.label("Would you like PSD and PSB files to open in PeerBrush when you double-click them?");
                ui.add_space(18.);
                ui.horizontal(|ui| {
                    let total = 240.;
                    ui.add_space(((ui.available_width() - total) / 2.).max(0.));
                    for (name, detail) in [("PSD", "Photoshop document"), ("PSB", "Large document")] {
                        ui.vertical(|ui| {
                            ui.set_width(112.);
                            ui.label(RichText::new(name).size(24.).strong().color(ACCENT));
                            ui.label(RichText::new(detail).size(11.).color(MUTED));
                        });
                    }
                });
                ui.add_space(18.);
                ui.label(RichText::new("Your files stay exactly where they are. You can change this choice later.").small().color(MUTED));
            });
            ui.add_space(14.);
            #[cfg(windows)]
            ui.label("Windows will open Default apps. Choose PeerBrush for .psd and .psb there.");
            #[cfg(target_os = "macos")]
            if !macos::installed() {
                ui.colored_label(ACCENT, "First drag PeerBrush into Applications, then open it from there.");
            }
            if let Some(error) = &self.error { ui.colored_label(ACCENT, error); }
            if self.pending.is_some() { ui.spinner(); ui.label("Waiting for macOS to confirm your choice…"); }
            ui.add_space(12.);
            ui.horizontal(|ui| {
                if ui.add_enabled(self.pending.is_none(), egui::Button::new("Not now").min_size(Vec2::new(100., 36.))).clicked() {
                    self.error = self.dismiss().err();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let enabled = self.pending.is_none() && self.can_apply();
                    if ui.add_enabled(enabled, dialogs::primary("Use PeerBrush").min_size(Vec2::new(160., 36.))).clicked() {
                        self.error = None;
                        self.error = self.apply(ctx).err();
                    }
                });
            });
            ui.add_space(6.);
        });
        if response.should_close() && self.pending.is_none() {
            self.error = self.dismiss().err();
        }
    }

    fn can_apply(&self) -> bool {
        #[cfg(target_os = "macos")]
        return macos::installed();
        #[cfg(not(target_os = "macos"))]
        true
    }

    fn apply(&mut self, _ctx: &egui::Context) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        {
            self.pending = Some(macos::request(_ctx)?);
            self.completed = 0;
            Ok(())
        }
        #[cfg(windows)]
        {
            open_windows_defaults(WINDOWS_DEFAULTS_URI)?;
            self.dismiss()
        }
        #[cfg(target_os = "linux")]
        {
            let status = std::process::Command::new("xdg-mime")
                .args([
                    "default",
                    "com.peerbrush.PeerBrush.desktop",
                    "image/vnd.adobe.photoshop",
                ])
                .status()
                .map_err(|_| "Could not open the desktop's file-association service")?;
            if !status.success() {
                return Err("Your desktop could not change the default. Use Open With in your file manager.".into());
            }
            self.dismiss()
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        Err("Choose PeerBrush using your file manager's Open With settings.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "peerbrush-consent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&path).unwrap();
        path
    }
    #[test]
    fn first_launch_does_not_apply_defaults_and_decline_is_remembered() {
        let root = directory();
        let mut welcome = Welcome::load(&root);
        assert!(welcome.open && welcome.pending.is_none());
        assert!(!root.join("associations.json").exists());
        welcome.dismiss().unwrap();
        assert!(!welcome.open);
        let mut reopened = Welcome::load(&root);
        assert!(!reopened.open && reopened.pending.is_none());
        reopened.offer();
        assert!(reopened.open);
        reopened.dismiss().unwrap();
        validate_file(&root.join("associations.json")).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn concurrent_or_unknown_preferences_are_preserved() {
        let root = directory();
        let mut welcome = Welcome::load(&root);
        let path = root.join("associations.json");
        let foreign = b"{\"version\":42,\"answered\":true}";
        std::fs::write(&path, foreign).unwrap();
        assert!(welcome.dismiss().is_err());
        assert_eq!(std::fs::read(&path).unwrap(), foreign);
        assert!(!Welcome::load(&root).open);
        std::fs::remove_dir_all(root).unwrap();
    }
}
