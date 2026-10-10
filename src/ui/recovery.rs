//! Startup and File recovery chooser; source files are never save destinations.
use super::*;

pub(super) struct Browser {
    pub open: bool,
    entries: Vec<crate::recovery::Snapshot>,
    legacy: Vec<(String, PathBuf)>,
    selected: Option<String>,
    discard: Option<String>,
    error: Option<String>,
}
impl Browser {
    pub fn new(dir: &Path) -> Self {
        let entries = crate::recovery::catalog(dir);
        let legacy = crate::recovery::legacy(dir);
        Self {
            open: !entries.is_empty() || !legacy.is_empty(),
            selected: entries.first().map(|e| e.snapshot.clone()),
            entries,
            legacy,
            discard: None,
            error: None,
        }
    }
}
fn timestamp(ms: u64) -> String {
    // Gregorian civil date from Unix days; no locale-dependent ambiguity.
    let seconds = ms / 1000;
    let z = (seconds / 86400) as i64 + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        seconds % 86400 / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}
impl PeerBrush {
    fn recover_version(&mut self, snapshot: String) {
        let workspace = self.workspace.clone();
        let dir = self.connection.state_dir.clone();
        let initiating = self.project_id.clone();
        let control = crate::loading::Control::default();
        self.load_control = Some(control.clone());
        self.job(move || {
            crate::recovery::restore(&workspace, &dir, &snapshot, &control, Some(&initiating))?;
            Ok("Recovered into a new unsaved tab · choose Save As · recovery retained".into())
        });
    }
    pub(super) fn recovery_failure(&mut self, ctx: &egui::Context) {
        let failure = self.workspace.lock().unwrap().recovery_error.clone();
        if let Some(error) = &failure {
            egui::TopBottomPanel::bottom("autosave failure").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(ACCENT, error);
                    if ui.button("Review recoveries").clicked() {
                        self.recovery.open = true;
                    }
                });
            });
        }
    }
    pub(super) fn recovery_window(&mut self, ctx: &egui::Context) {
        let failure = self.workspace.lock().unwrap().recovery_error.clone();
        if !self.recovery.open {
            return;
        }
        let mut open = true;
        let mut later = false;
        let mut restore = None;
        let mut legacy = None;
        let mut discard = None;
        egui::Window::new("Recover autosaved work").open(&mut open).default_width(590.).show(ctx, |ui| {
            ui.label("Restore a complete version into a new tab. Original files stay intact.");
            ui.label(RichText::new("Restored work is unsaved. Choose a destination with Save As; recovery versions remain until discarded.").color(MUTED));
            if let Some(error) = &failure { ui.colored_label(ACCENT, error); }
            if let Some(error) = &self.recovery.error { ui.colored_label(ACCENT, error); }
            if self.message.to_lowercase().contains("recover") { ui.label(&self.message); }
            egui::ScrollArea::vertical().max_height(340.).show(ui, |ui| {
                for entry in &self.recovery.entries {
                    let selected = self.recovery.selected.as_deref() == Some(&entry.snapshot);
                    if ui.selectable_label(selected, format!("{} · {} · {}-bit · revision {}", entry.name, timestamp(entry.timestamp_ms), entry.bit_depth, entry.revision)).clicked() {
                        self.recovery.selected = Some(entry.snapshot.clone());
                        self.recovery.discard = None;
                    }
                    if selected {
                        ui.label(RichText::new(entry.original_path.as_ref().map(|p|p.display().to_string()).unwrap_or_else(||"Untitled · no original destination".into())).size(11.).color(MUTED));
                        ui.label(RichText::new(format!("Document {}", entry.document_id)).size(11.).color(MUTED));
                    }
                }
                for (name, path) in &self.recovery.legacy {
                    if ui.add_enabled(!self.busy, egui::Button::new(format!("Restore {name} · legacy autosave"))).clicked() { legacy = Some(path.clone()); }
                }
            });
            if self.recovery.entries.is_empty() && self.recovery.legacy.is_empty() { ui.label("No complete recovery versions are available."); }
            ui.horizontal(|ui| {
                if ui.add_enabled(!self.busy && self.recovery.selected.is_some(),egui::Button::new("Restore selected version")).clicked() { restore = self.recovery.selected.clone(); }
                if ui.add_enabled(!self.busy && self.recovery.selected.is_some(),egui::Button::new("Discard selected…")).clicked() { self.recovery.discard = self.recovery.selected.clone(); }
                if ui.button("Refresh").clicked() { self.recovery = Browser::new(&self.connection.state_dir); }
                if ui.button("Later").clicked() { later = true; }
            });
            if let Some(snapshot) = self.recovery.discard.clone() {
                ui.colored_label(ACCENT,"Discard this recovery version permanently? Other versions and original files stay intact.");
                ui.horizontal(|ui| {
                    if ui.button("Discard this version").clicked() { discard = Some(snapshot.clone()); }
                    if ui.button("Keep it").clicked() { self.recovery.discard = None; }
                });
            }
        });
        self.recovery.open = open && !later;
        if let Some(snapshot) = discard {
            match crate::recovery::discard(&self.connection.state_dir, &snapshot) {
                Ok(()) => {
                    self.recovery = Browser::new(&self.connection.state_dir);
                    self.recovery.open = true;
                    self.message = "Recovery version discarded".into();
                }
                Err(error) => self.recovery.error = Some(error),
            }
        }
        if let Some(snapshot) = restore {
            self.recover_version(snapshot);
        }
        if let Some(path) = legacy {
            self.recovery.open = false;
            self.open_job(Some(path), true);
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
    fn text_position(shape: &egui::Shape, needle: &str) -> Option<Pos2> {
        match shape {
            egui::Shape::Text(text) if text.galley.text() == needle => {
                Some(text.pos + text.galley.size() / 2.)
            }
            egui::Shape::Vec(shapes) => shapes.iter().find_map(|s| text_position(s, needle)),
            _ => None,
        }
    }
    #[test]
    fn startup_chooser_restores_through_a_real_click_and_surfaces_autosave_failure() {
        let dir = std::env::temp_dir().join(format!("pb-recovery-ui-{}", crate::engine::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut old = crate::engine::Engine::new();
        old.doc = Document::new_depth(32, 24, 16).unwrap();
        let layer = old.doc.layers[0].id.clone();
        old.edit(
            "human",
            &[json!({"op":"paint.fill","layer":layer,"color":[180,70,20,255]})],
            None,
            None,
            "Artwork",
        )
        .unwrap();
        let expected = old.doc.export_png().unwrap();
        let old = Arc::new(std::sync::Mutex::new(old));
        server::checkpoint_projects(
            &crate::workspace::attach(&old),
            &dir,
            &mut Default::default(),
        );
        let mut blank = crate::engine::Engine::new();
        blank.doc = Document::new(32, 24).unwrap();
        let blank = Arc::new(std::sync::Mutex::new(blank));
        let blank_id = blank.lock().unwrap().doc.id.clone();
        let ctx = egui::Context::default();
        let mut app = PeerBrush::init(
            &ctx,
            blank.clone(),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: dir,
                instance_lock: None,
            },
        );
        assert!(app.recovery.open);
        assert_eq!(app.recovery.entries.len(), 1);
        for _ in 0..3 {
            frame(&mut app, &ctx, vec![]);
        }
        let output = frame(&mut app, &ctx, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| text_position(&s.shape, "Restore selected version"))
            .expect("Startup restore control must be visible");
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
        for _ in 0..100 {
            frame(&mut app, &ctx, vec![]);
            if !app.busy {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!app.busy);
        assert_eq!(
            crate::workspace::entries_in(&app.workspace).len(),
            2,
            "{}",
            app.message
        );
        assert_eq!(blank.lock().unwrap().doc.id, blank_id);
        let active = crate::workspace::active(&blank).unwrap();
        let e = active.lock().unwrap();
        assert_eq!(e.doc.bit_depth, 16);
        assert_eq!(e.doc.export_png().unwrap(), expected);
        assert!(e.path.is_none());
        assert_ne!(e.saved_revision, e.doc.revision);
        drop(e);
        app.workspace.lock().unwrap().recovery_error =
            Some("Autosave failed · synthetic disk failure".into());
        let output = frame(&mut app, &ctx, vec![]);
        assert!(output.shapes.iter().any(|s| text_position(
            &s.shape,
            "Autosave failed · synthetic disk failure"
        )
        .is_some()));
    }
    #[test]
    fn recovery_timestamps_are_explicit_utc() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(timestamp(1791581400000), "2026-10-09 21:30:00 UTC");
    }
}
