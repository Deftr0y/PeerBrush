//! Startup and File recovery chooser; source files are never save destinations.
use super::*;

pub(super) struct Browser {
    pub open: bool,
    entries: Vec<crate::recovery::Snapshot>,
    unpaired: Vec<crate::recovery::Unpaired>,
    legacy: Vec<(String, PathBuf)>,
    selected: Option<String>,
    discard: Option<String>,
    error: Option<String>,
    record_error: Option<String>,
    native: crate::recovery::NativeCatalog,
    pending: Option<Pending>,
    pub(super) record_job: Option<String>,
}
struct Pending {
    result: mpsc::Receiver<Result<crate::recovery::Available, String>>,
    control: crate::loading::Control,
    open_when_ready: bool,
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.control.cancel();
    }
}
impl Browser {
    pub fn new(workspace: &crate::workspace::Registry, dir: &Path, ctx: &egui::Context) -> Self {
        let mut browser = Self {
            open: false,
            selected: None,
            entries: Vec::new(),
            unpaired: Vec::new(),
            legacy: Vec::new(),
            discard: None,
            error: None,
            record_error: None,
            native: Default::default(),
            pending: None,
            record_job: None,
        };
        browser.refresh(workspace, dir, ctx, false);
        if let Some(pending) = &mut browser.pending {
            pending.open_when_ready = true;
        }
        browser
    }
    pub fn refresh(
        &mut self,
        workspace: &crate::workspace::Registry,
        dir: &Path,
        ctx: &egui::Context,
        show: bool,
    ) {
        if show {
            self.open = true;
        }
        if self.record_job.is_some() {
            return;
        }
        if let Some(pending) = &mut self.pending {
            pending.open_when_ready |= show;
            return;
        }
        let workspace = workspace.clone();
        let dir = dir.to_owned();
        let repaint = ctx.clone();
        let control = crate::loading::Control::default();
        let checked = control.clone();
        let (tx, result) = mpsc::sync_channel(1);
        match std::thread::Builder::new()
            .name("peerbrush-recovery-catalog".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    crate::recovery::catalog_in(&workspace, &dir, &|| checked.check())
                }))
                .unwrap_or_else(|_| {
                    Err("Recovery inspection could not complete; existing files retained".into())
                });
                let _ = tx.send(result);
                repaint.request_repaint();
            }) {
            Ok(_) => {
                self.pending = Some(Pending {
                    result,
                    control,
                    open_when_ready: show,
                })
            }
            Err(_) => {
                self.error =
                    Some("Could not start recovery inspection; existing files retained".into());
                self.open |= show;
            }
        }
    }
    pub(super) fn finish_record_job(
        &mut self,
        error: Option<String>,
        workspace: &crate::workspace::Registry,
        dir: &Path,
        ctx: &egui::Context,
    ) {
        self.record_job = None;
        self.record_error = error;
        // A failure can follow a filesystem change, such as a failed directory sync.
        self.refresh(workspace, dir, ctx, self.open);
    }
    fn poll(&mut self) {
        let Some(pending) = &self.pending else { return };
        let result = match pending.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                Err("Recovery inspection was interrupted; existing files retained".into())
            }
        };
        let show = pending.open_when_ready;
        self.pending = None;
        match result {
            Ok(available) => {
                self.entries = available.versions;
                self.unpaired = available.unpaired;
                self.legacy = available.legacy;
                self.native = available.native;
                self.error = available.resident_error.map(str::to_owned);
                self.discard = None;
                let valid = |id: &str| {
                    self.entries.iter().any(|entry| entry.snapshot == id)
                        || self.unpaired.iter().any(|entry| entry.snapshot == id)
                        || self
                            .native
                            .entries()
                            .iter()
                            .any(|entry| entry.snapshot == id)
                };
                if !self.selected.as_deref().is_some_and(valid) {
                    let native = self
                        .native
                        .entries()
                        .iter()
                        .find(|entry| entry.error.is_none() && entry.read_only == Some(false));
                    let legacy = self.entries.first();
                    self.selected = match (legacy, native) {
                        (Some(legacy), Some(native))
                            if native.timestamp_ms > Some(legacy.timestamp_ms) =>
                        {
                            Some(native.snapshot.clone())
                        }
                        (Some(legacy), _) => Some(legacy.snapshot.clone()),
                        (_, Some(native)) => Some(native.snapshot.clone()),
                        _ => self.unpaired.first().map(|entry| entry.snapshot.clone())
                            .or_else(|| self.native.entries().first().map(|entry| entry.snapshot.clone())),
                    };
                }
                if show
                    && (!self.entries.is_empty()
                        || !self.legacy.is_empty()
                        || !self.unpaired.is_empty()
                        || !self.native.entries().is_empty()
                        || self.error.is_some()
                        || self.native.error().is_some())
                {
                    self.open = true;
                }
            }
            Err(error) => {
                self.error = Some(error);
                self.open |= show;
            }
        }
    }
}
pub(super) fn timestamp(ms: u64) -> String {
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
                        self.recovery.refresh(
                            &self.workspace,
                            &self.connection.state_dir,
                            ctx,
                            true,
                        );
                    }
                });
            });
        }
    }
    pub(super) fn recovery_window(&mut self, ctx: &egui::Context) {
        self.recovery.poll();
        let failure = self.workspace.lock().unwrap().recovery_error.clone();
        if !self.recovery.open {
            return;
        }
        let mut open = true;
        let mut later = false;
        let mut restore = None;
        let mut legacy = None;
        let mut discard = None;
        let mut repair = None;
        let mut refresh = false;
        let inspecting = self.recovery.pending.is_some();
        let working = self.busy || inspecting || self.recovery.record_job.is_some();
        egui::Window::new("Recover autosaved work").open(&mut open).default_width(590.).show(ctx, |ui| {
            dialogs::body(ui, 590.);
            ui.label("Restore a complete version into a new tab. Original files stay intact.");
            ui.label(RichText::new("Restored work is unsaved. Choose a destination with Save As; recovery versions remain until discarded.").color(MUTED));
            if let Some(error) = &failure { ui.colored_label(ACCENT, error); }
            if let Some(error) = &self.recovery.record_error { ui.colored_label(ACCENT, error); }
            if let Some(error) = &self.recovery.error { ui.colored_label(ACCENT, error); }
            if let Some(error) = self.recovery.native.error() { ui.colored_label(ACCENT, error); }
            if inspecting { ui.horizontal(|ui| { ui.spinner(); ui.label("Checking recoveries…"); }); }
            if self.recovery.record_job.is_some() { ui.horizontal(|ui| { ui.spinner(); ui.label("Updating recovery records…"); }); }
            if self.recovery.record_error.as_deref() != Some(self.message.as_str())
                && self.message.to_lowercase().contains("recover")
            {
                ui.label(self.message.as_str());
            }
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
                for entry in self.recovery.native.entries() {
                    let selected = self.recovery.selected.as_deref() == Some(&entry.snapshot);
                    let date = entry.timestamp_ms.map(timestamp).unwrap_or_else(|| "Unknown date".into());
                    let precision = entry.bit_depth.map(|depth| format!("{depth}-bit")).unwrap_or_else(|| "Unknown precision".into());
                    if ui.selectable_label(selected, format!("{} · {date} · {precision} · native recovery", entry.display_name)).clicked() {
                        self.recovery.selected = Some(entry.snapshot.clone());
                        self.recovery.discard = None;
                    }
                    if selected {
                        if let Some(id) = &entry.document_id { ui.label(RichText::new(format!("Document {id}")).size(11.).color(MUTED)); }
                        if let Some(error) = &entry.error { ui.colored_label(ACCENT, error); }
                        if entry.read_only == Some(true) { ui.label("This protected checkpoint cannot be restored as editable work. Its files are retained."); }
                    }
                }
                for entry in &self.recovery.unpaired {
                    let selected = self.recovery.selected.as_deref() == Some(&entry.snapshot);
                    if ui.selectable_label(selected, format!("Unpaired autosave · {} · {} KiB", timestamp(entry.timestamp_ms), entry.bytes.div_ceil(1024))).clicked() {
                        self.recovery.selected = Some(entry.snapshot.clone());
                        self.recovery.discard = None;
                    }
                    if selected {
                        ui.label("No recovery record was written. Restore checks the PSD and keeps the original file.");
                        ui.label("Rebuilding the record adds this version to normal autosave rotation.");
                        if ui.add_enabled(!working, egui::Button::new("Rebuild recovery record")).clicked() {
                            repair = Some(entry.snapshot.clone());
                        }
                    }
                }
                for (name, path) in &self.recovery.legacy {
                    if ui.add_enabled(!working, egui::Button::new(format!("Restore {name} · legacy autosave"))).clicked() { legacy = Some(path.clone()); }
                }
            });
            if !inspecting && self.recovery.error.is_none() && self.recovery.native.error().is_none() && self.recovery.entries.is_empty() && self.recovery.unpaired.is_empty() && self.recovery.legacy.is_empty() && self.recovery.native.entries().is_empty() { ui.label("No complete recovery versions are available."); }
            ui.horizontal(|ui| {
                let native_selected = self.recovery.native.entries().iter().find(|entry| Some(entry.snapshot.as_str()) == self.recovery.selected.as_deref());
                let unpaired = self.recovery.unpaired.iter().any(|entry| Some(entry.snapshot.as_str()) == self.recovery.selected.as_deref());
                let can_restore = self.recovery.selected.is_some() && native_selected.is_none_or(|entry| entry.error.is_none() && entry.read_only == Some(false));
                let can_discard = self.recovery.selected.is_some() && !unpaired && native_selected.is_none_or(|entry| crate::recovery::NATIVE_DISCARD_AVAILABLE && entry.error.is_none());
                if ui.add_enabled(!working && can_restore,dialogs::primary("Restore selected version")).clicked() { restore = self.recovery.selected.clone(); }
                if ui.add_enabled(!working && can_discard,egui::Button::new("Discard selected…")).on_disabled_hover_text(if unpaired { "Unpaired autosaves are retained for manual review." } else if native_selected.is_some() && !crate::recovery::NATIVE_DISCARD_AVAILABLE { "Native checkpoint discard is unavailable on this platform; files are retained." } else if native_selected.is_some_and(|entry| entry.error.is_some()) { "This checkpoint cannot be validated; its files are retained." } else { "Choose a recovery version and finish active work." }).clicked() { self.recovery.discard = self.recovery.selected.clone(); }
                if ui.add_enabled(!working, egui::Button::new("Refresh")).clicked() { refresh = true; }
                if ui.button("Later").clicked() { later = true; }
            });
            if let Some(snapshot) = self.recovery.discard.clone() {
                ui.colored_label(ACCENT,"Discard this recovery version permanently? Other versions and original files stay intact.");
                ui.horizontal(|ui| {
                    if ui.add_enabled(!working, egui::Button::new("Discard this version")).clicked() { discard = Some(snapshot.clone()); }
                    if ui.add_enabled(!working, egui::Button::new("Keep it")).clicked() { self.recovery.discard = None; }
                });
            }
        });
        self.recovery.open = open && !later;
        if !self.recovery.open {
            if let Some(pending) = &mut self.recovery.pending {
                pending.open_when_ready = false;
            }
        }
        if refresh {
            self.recovery
                .refresh(&self.workspace, &self.connection.state_dir, ctx, true);
        }
        if let Some(snapshot) = discard {
            self.recovery.discard = None;
            self.recovery.record_job = Some(self.project_id.clone());
            let workspace = self.workspace.clone();
            let directory = self.connection.state_dir.clone();
            self.job(move || {
                crate::recovery::discard_in(&workspace, &directory, &snapshot)
                    .map_err(|e| format!("Recovery discard could not complete: {e}"))?;
                Ok("Recovery version discarded".into())
            });
        }
        if let Some(snapshot) = repair {
            self.recovery.record_job = Some(self.project_id.clone());
            let workspace = self.workspace.clone();
            let directory = self.connection.state_dir.clone();
            self.job(move || {
                crate::recovery::repair_unpaired(&workspace, &directory, &snapshot, &Default::default())
                    .map_err(|e| format!("Recovery record could not be rebuilt: {e}"))?;
                Ok("Recovery record rebuilt".into())
            });
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
    fn click(app: &mut PeerBrush, ctx: &egui::Context, label: &str) {
        let output = frame(app, ctx, vec![]);
        let pos = output
            .shapes
            .iter()
            .find_map(|s| text_position(&s.shape, label))
            .unwrap_or_else(|| panic!("Missing recovery control: {label}"));
        for pressed in [true, false] {
            frame(
                app,
                ctx,
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
    }
    #[test]
    fn startup_chooser_restores_through_a_real_click_and_surfaces_autosave_failure() {
        let dir = std::env::temp_dir().join(format!("pb-recovery-ui-{}", crate::engine::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut old = crate::engine::Engine::new();
        old.resident_state_mut().expect("resident fixture").document =
            Document::new_depth(32, 24, 16).unwrap();
        let layer = old
            .resident_state()
            .expect("resident fixture")
            .document
            .layers[0]
            .id
            .clone();
        old.edit(
            "human",
            &[json!({"op":"paint.fill","layer":layer,"color":[180,70,20,255]})],
            None,
            None,
            "Artwork",
        )
        .unwrap();
        let expected = old
            .resident_state()
            .expect("resident fixture")
            .document
            .export_png()
            .unwrap();
        let old = Arc::new(std::sync::Mutex::new(old));
        server::checkpoint_projects(
            &crate::workspace::attach(&old),
            &dir,
            &mut Default::default(),
        );
        let mut blank = crate::engine::Engine::new();
        blank
            .resident_state_mut()
            .expect("resident fixture")
            .document = Document::new(32, 24).unwrap();
        let blank = Arc::new(std::sync::Mutex::new(blank));
        let blank_id = blank
            .lock()
            .unwrap()
            .resident_state()
            .expect("resident fixture")
            .document
            .id
            .clone();
        let ctx = egui::Context::default();
        let mut app = PeerBrush::init(
            &ctx,
            blank.clone(),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: dir,
                instance_lock: None,
                availability: Default::default(),
            },
        );
        for _ in 0..200 {
            frame(&mut app, &ctx, vec![]);
            if app.recovery.pending.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.recovery.pending.is_none());
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
        assert_eq!(
            blank
                .lock()
                .unwrap()
                .resident_state()
                .expect("resident fixture")
                .document
                .id,
            blank_id
        );
        let active = crate::workspace::active(&blank).unwrap();
        let e = active.lock().unwrap();
        assert_eq!(
            e.resident_state()
                .expect("resident fixture")
                .document
                .bit_depth,
            16
        );
        assert_eq!(
            e.resident_state()
                .expect("resident fixture")
                .document
                .export_png()
                .unwrap(),
            expected
        );
        assert!(e.path.is_none());
        assert_ne!(
            e.saved_revision,
            e.resident_state()
                .expect("resident fixture")
                .document
                .revision
        );
        drop(e);
        app.workspace.lock().unwrap().recovery_error =
            Some("Autosave failed · synthetic disk failure".into());
        let output = frame(&mut app, &ctx, vec![]);
        assert!(output.shapes.iter().any(|s| text_position(
            &s.shape,
            "Autosave failed · synthetic disk failure"
        )
        .is_some()));
        app.recovery.open = true;
        app.recovery.record_job = Some(app.project_id.clone());
        click(&mut app, &ctx, "Discard selected…");
        assert!(
            app.recovery.discard.is_none(),
            "A pending discard must disable duplicate requests"
        );
        app.recovery.record_job = None;
        click(&mut app, &ctx, "Discard selected…");
        assert!(app.recovery.discard.is_some());
        assert_eq!(crate::recovery::catalog(&app.connection.state_dir).len(), 1);
        click(&mut app, &ctx, "Discard this version");
        assert!(app.recovery.record_job.is_some());
        click(&mut app, &ctx, "Later");
        for _ in 0..200 {
            frame(&mut app, &ctx, vec![]);
            if app.recovery.record_job.is_none() && app.recovery.pending.is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(app.recovery.record_job.is_none());
        assert!(app.recovery.pending.is_none());
        assert!(
            !app.recovery.open,
            "A completed background discard must respect Later"
        );
        assert!(app.recovery.entries.is_empty(), "{}", app.message);
        assert!(crate::recovery::catalog(&app.connection.state_dir).is_empty());
        assert_eq!(
            active
                .lock()
                .unwrap()
                .resident_state()
                .unwrap()
                .document
                .export_png()
                .unwrap(),
            expected
        );
    }
    #[test]
    fn recovery_timestamps_are_explicit_utc() {
        assert_eq!(timestamp(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(timestamp(1791581400000), "2026-10-09 21:30:00 UTC");
    }
    #[test]
    fn native_checkpoint_is_discovered_and_restored_through_the_real_chooser() {
        for depth in [8, 16] {
            let fixture = crate::engine::native::worker::tests::Fixture::new();
            let mut document = Document::new_depth(4, 2, depth).unwrap();
            document.layers[0]
                .pixels
                .set16(2, 1, [12347, 23459, 34571, 65535]);
            let expected = document.layers[0].pixels.get16(2, 1);
            let shared = Arc::new(std::sync::Mutex::new(crate::engine::Engine::from_document(
                document,
            )));
            let mut connection =
                server::start(shared.clone(), fixture.path().join("state")).unwrap();
            connection
                .instance_lock
                .as_mut()
                .unwrap()
                .stop_recovery_for_test();
            connection.configure_native_sources(None, true).unwrap();
            let runtime = crate::workspace::native_dispatch::runtime_for(&shared).unwrap();
            let cache = crate::engine::scene::LegacyTileCache::new(&runtime.resident(), 8).unwrap();
            let captured = shared.lock().unwrap().capture_scene(None).unwrap();
            let staged = captured.stage(&runtime, &cache, &|| Ok(())).unwrap();
            let native = crate::engine::native::NativeDocument::from_pending(
                staged.scene.into_provisional_pending().unwrap(),
            )
            .unwrap();
            {
                let mut engine = shared.lock().unwrap();
                engine.authority =
                    crate::engine::authority::DocumentAuthority::from_native(native, 8).unwrap();
                engine.saved_revision = u64::MAX;
            }
            let scene = shared
                .lock()
                .unwrap()
                .capture_scene(None)
                .unwrap()
                .into_native()
                .unwrap();
            let session = crate::workspace::native_dispatch::begin_scene(&shared, &scene).unwrap();
            let id = uuid::Uuid::new_v4();
            crate::recovery::backed::Capture::from_native(
                &scene,
                &session,
                crate::recovery::backed::Limits {
                    metadata: crate::psd::canonical36::Limits {
                        bytes: 65536,
                        nodes: 8192,
                        depth: 64,
                    },
                    refs: 1024,
                    stores: 16,
                },
                &|| Ok(()),
            )
            .unwrap()
            .publish(*id.as_bytes(), &session, &|| Ok(()))
            .unwrap();
            let ctx = egui::Context::default();
            let mut app = PeerBrush::init(&ctx, shared.clone(), connection);
            for _ in 0..200 {
                frame(&mut app, &ctx, vec![]);
                if app.recovery.pending.is_none() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(app.recovery.pending.is_none());
            assert!(app.recovery.open);
            assert_eq!(app.recovery.native.entries().len(), 1);
            assert_eq!(
                app.recovery.selected.as_deref(),
                Some(format!("native:{id}").as_str())
            );
            let output = frame(&mut app, &ctx, vec![]);
            let pos = output
                .shapes
                .iter()
                .find_map(|s| text_position(&s.shape, "Restore selected version"))
                .unwrap();
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
                if crate::workspace::entries_in(&app.workspace).len() == 2 && !app.busy {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert_eq!(
                crate::workspace::entries_in(&app.workspace).len(),
                2,
                "{}",
                app.message
            );
            let active = crate::workspace::active(&shared).unwrap();
            let engine = active.lock().unwrap();
            assert!(engine.resident_state().is_err());
            assert!(engine.path.is_none());
            assert_eq!(engine.saved_revision, u64::MAX);
            let recovered = engine.capture_scene(None).unwrap().into_native().unwrap();
            assert_ne!(
                recovered.identity().document_id,
                scene.identity().document_id
            );
            drop(engine);
            let source = recovered.rasters().iter().next().unwrap().1;
            let region = crate::raster::backed::Region::new([0, 0, 4, 2]).unwrap();
            assert_eq!(
                source
                    .prepare_region(region, &mut || Ok(()))
                    .unwrap()
                    .pixel16(2, 1)
                    .unwrap(),
                expected
            );
            assert_eq!(
                shared.lock().unwrap().native_document().unwrap().key(),
                scene.key()
            );
            assert!(session.open_recovery_marker(*id.as_bytes()).is_ok());
            // A late inspection failure must not reopen a chooser dismissed by human input.
            let (tx, result) = mpsc::sync_channel(1);
            app.recovery.pending = Some(Pending {
                result,
                control: Default::default(),
                open_when_ready: true,
            });
            app.recovery.open = true;
            let output = frame(&mut app, &ctx, vec![]);
            let pos = output
                .shapes
                .iter()
                .find_map(|s| text_position(&s.shape, "Later"))
                .unwrap();
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
            assert!(!app.recovery.open);
            tx.send(Err("Inspection interrupted; files retained".into()))
                .unwrap();
            frame(&mut app, &ctx, vec![]);
            assert!(!app.recovery.open);
        }
    }
}
