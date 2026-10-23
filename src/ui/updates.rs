use super::*;
use crate::updates::{self, Channel, Owner, Progress, Report};
use std::time::Instant;

#[cfg(any(windows, test))]
mod store;

enum Event {
    Progress(Progress),
    Complete(Result<Report, String>),
}
pub(super) struct Panel {
    pub open: bool,
    store: updates::Store,
    owner: Owner,
    receiver: Option<mpsc::Receiver<Event>>,
    progress: Option<Progress>,
    result: Option<Result<Report, String>>,
    message: String,
    next_auto: Instant,
    last_notification: Option<String>,
    defer_current: bool,
    #[cfg(target_os = "macos")]
    native_updater: crate::sparkle::macos::Application,
    #[cfg(any(windows, test))]
    store_check: store::Check,
}
impl Panel {
    pub fn load(directory: &Path) -> Self {
        Self {
            open: false,
            store: updates::Store::load(directory),
            owner: Owner::current(),
            receiver: None,
            progress: None,
            result: None,
            message: String::new(),
            next_auto: Instant::now() + Duration::from_secs(2),
            last_notification: None,
            defer_current: false,
            #[cfg(target_os = "macos")]
            native_updater: crate::sparkle::macos::Application::load(),
            #[cfg(any(windows, test))]
            store_check: store::Check::default(),
        }
    }
    #[cfg(windows)]
    pub(super) fn native_window(&mut self, frame: &eframe::Frame) {
        self.store_check.window(frame);
    }
    fn start(&mut self, ctx: &egui::Context, manual: bool) {
        if self.receiver.is_some() {
            return;
        }
        let target = match updates::Target::current() {
            Ok(target) => target,
            Err(error) => {
                self.result = Some(Err(error));
                return;
            }
        };
        self.next_auto = Instant::now() + Duration::from_secs(updates::CHECK_INTERVAL);
        let mut preferences = self.store.preferences.clone();
        preferences.last_attempt = Some(updates::now());
        if let Err(error) = self.store.save(preferences) {
            self.message = error;
            if !manual {
                return;
            }
        }
        let channel = self.store.preferences.channel;
        let (sender, receiver) = mpsc::channel();
        let repaint = ctx.clone();
        match std::thread::Builder::new()
            .name("peerbrush-update-check".into())
            .spawn(move || {
                let result = updates::check(channel, target, |progress| {
                    let _ = sender.send(Event::Progress(progress));
                    repaint.request_repaint();
                });
                let _ = sender.send(Event::Complete(result));
                repaint.request_repaint();
            }) {
            Ok(_) => {
                self.defer_current = false;
                self.receiver = Some(receiver);
                self.progress = Some(Progress {
                    stage: "Starting secure release check",
                    bytes: 0,
                });
                self.result = None;
            }
            Err(_) => {
                self.result = Some(Err(
                    "Could not start the release check; try again later".into()
                ))
            }
        }
    }
    fn tick(&mut self, ctx: &egui::Context, allow_notification: bool) {
        #[cfg(target_os = "macos")]
        self.native_updater.tick(ctx);
        #[cfg(any(windows, test))]
        {
            self.store_check.tick(Instant::now());
            if self.store_check.pending() {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        let events: Vec<_> = self
            .receiver
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default();
        for event in events {
            match event {
                Event::Progress(progress) => self.progress = Some(progress),
                Event::Complete(result) => {
                    self.result = Some(result);
                    self.receiver = None;
                    self.progress = None;
                }
            }
        }
        if !self.defer_current {
            if let Some(Ok(report)) = &self.result {
                if let Some(version) = &report.release_version {
                    if self.last_notification.as_ref() != Some(version) {
                        if allow_notification {
                            self.open = true;
                            self.last_notification = Some(version.clone());
                        } else {
                            ctx.request_repaint_after(Duration::from_secs(1));
                        }
                    }
                }
            }
        }
        if self.receiver.is_none() && Instant::now() >= self.next_auto {
            self.next_auto = Instant::now() + Duration::from_secs(updates::CHECK_INTERVAL);
            if self.store.due(updates::now()) {
                self.start(ctx, false);
            }
        }
        if self.store.preferences.automatic_checks {
            ctx.request_repaint_after(self.next_auto.saturating_duration_since(Instant::now()));
        }
    }
    fn show(&mut self, ctx: &egui::Context, restart_blocker: Option<String>) {
        if !self.open {
            return;
        }
        let mut open = true;
        let mut later = false;
        egui::Window::new("PeerBrush updates").id(egui::Id::new("peerbrush_updates"))
            .open(&mut open).resizable(true).default_width(480.).show(ctx, |ui| {
                ui.label(format!("Installed version {}", env!("CARGO_PKG_VERSION")));
                ui.label(self.owner.description());
                #[cfg(target_os = "macos")]
                {
                    let message = self.native_updater.message();
                    if !message.is_empty() { ui.label(message); }
                }
                if self.owner != Owner::Direct {
                    ui.label("Public release notes do not establish your Store or package provider's update availability.");
                }
                #[cfg(any(windows, test))]
                if self.owner == Owner::MicrosoftStore {
                    self.store_check.show(ui);
                }
                #[cfg(target_os = "macos")]
                if self.native_updater.configured() {
                    use crate::sparkle::check::Phase;
                    ui.label("Signed macOS stable release information");
                    let busy=self.native_updater.view().busy;
                    if ui.add_enabled(!busy,egui::Button::new("Check app updates")).clicked() {
                        if let Err(error)=self.native_updater.check(ctx) { self.message=error; }
                    }
                    let phase=self.native_updater.view().phase;
                    match phase {
                        Phase::Verifying | Phase::Checking => {
                            ui.horizontal(|ui| { ui.spinner(); ui.label(if phase==Phase::Verifying { "Verifying this app for secure update checking…" } else { "Checking signed release information…" }); });
                            if ui.button("Cancel app check").clicked() { self.native_updater.defer(); }
                        }
                        Phase::Found => {
                            let state=self.native_updater.view();
                            if let Some(version)=&state.version { ui.label(format!("New app version {version}")); }
                            if !state.notes.is_empty() { egui::ScrollArea::vertical().id_salt("sparkle_notes").max_height(180.).show(ui,|ui| { ui.label(&state.notes); }); } else { ui.label("Plain-text release notes are not embedded in this update."); }
                            ui.label("Package download is unavailable in this build.");
                        }
                        Phase::NoUpdate => { ui.label("No compatible signed app update was found."); }
                        Phase::Cancelled => { ui.label("App update check cancelled."); }
                        Phase::Failed => { ui.label("App update checking could not complete. Try again later."); }
                        Phase::Quarantined => {}
                        Phase::Idle => {}
                    }
                    let state=self.native_updater.view();
                    if !state.notice.is_empty() { ui.label(state.notice); }
                }
                ui.label("Official GitHub release notes and portable package metadata");
                let mut preferences = self.store.preferences.clone();
                ui.add_enabled_ui(self.store.error.is_none() && self.receiver.is_none(), |ui| {
                    ui.checkbox(&mut preferences.automatic_checks, "Check official releases daily");
                    ui.horizontal(|ui| {
                        ui.label("Channel");
                        egui::ComboBox::from_id_salt("update_channel")
                            .selected_text(match preferences.channel { Channel::Stable => "Stable", Channel::Preview => "Preview" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut preferences.channel, Channel::Stable, "Stable");
                                ui.selectable_value(&mut preferences.channel, Channel::Preview, "Preview, including prereleases");
                            });
                    });
                });
                if preferences != self.store.preferences {
                    let changed_channel = preferences.channel != self.store.preferences.channel;
                    match self.store.save(preferences) {
                        Ok(()) => {
                            self.message.clear();
                            if changed_channel { self.result = None; }
                            self.next_auto = Instant::now() + Duration::from_secs(2);
                        }
                        Err(error) => self.message = error,
                    }
                }
                if let Some(error) = &self.store.error { ui.label(format!("Preferences protected: {error}")); }
                if !self.message.is_empty() { ui.label(&self.message); }
                if ui.add_enabled(self.receiver.is_none(), egui::Button::new("Check now")).clicked() { self.start(ctx, true); }
                if let Some(progress) = &self.progress {
                    ui.horizontal(|ui| { ui.spinner(); ui.label(format!("{} · {} KiB", progress.stage, progress.bytes / 1024)); });
                }
                if let Some(result) = &self.result {
                    match result {
                        Err(error) => { ui.label(error); }
                        Ok(report) => {
                            if let Some(version) = &report.release_version {
                                ui.label(format!("Published version {version}"));
                                if report.assets.is_empty() {
                                    ui.label(format!("No published {} package is available for this version.", report.target.label()));
                                } else {
                                    for asset in &report.assets {
                                        ui.label(format!("{} · {:.1} MiB", asset.name, asset.bytes as f64 / (1024. * 1024.)));
                                    }
                                    ui.label("Published asset metadata only. Package signatures have not been verified by this check.");
                                }
                                if let Some(url) = &report.release_url { ui.hyperlink_to("Read the official release", url); }
                                if !report.notes.is_empty() {
                                    egui::ScrollArea::vertical().max_height(200.).show(ui, |ui| { ui.label(&report.notes); });
                                }
                            } else { ui.label("No newer published release was found in this channel."); }
                        }
                    }
                }
                ui.label("Automatic installation and restart are not available in this development build.");
                if let Some(reason) = &restart_blocker { ui.label(format!("Restart protection: {reason}")); }
                if ui.button("Later").clicked() { later = true; }
            });
        self.open = open && !later;
        if !self.open {
            self.defer_current = true;
            #[cfg(target_os = "macos")]
            self.native_updater.defer();
            #[cfg(any(windows, test))]
            self.store_check.cancel();
        }
    }
}

impl PeerBrush {
    #[cfg(target_os = "macos")]
    pub(super) fn native_update_quit(&mut self) -> bool {
        if !self.updates.native_updater.pending_installation() {
            return false;
        }
        let blocked = self.update_interaction_active();
        // A pending updater may never enter the ordinary Don't Save exit path.
        self.message = self
            .updates
            .native_updater
            .request_installation(
                &self.workspace,
                blocked,
                crate::sparkle::macos::ContinuationKind::InstallOnQuit,
            )
            .err()
            .unwrap_or_else(|| {
                "Automatic updates are not available in this development build.".into()
            });
        true
    }

    pub(super) fn updates_tick(&mut self, ctx: &egui::Context) {
        let idle = !self.update_interaction_active()
            && !ctx.wants_keyboard_input()
            && !ctx.input(|i| i.pointer.any_down() || !i.events.is_empty());
        self.updates.tick(ctx, idle);
    }
    pub(super) fn update_interaction_active(&self) -> bool {
        self.busy
            || self.connecting
            || !self.jobs.is_empty()
            || self.load_control.is_some()
            || self.lifecycle.is_some()
            || self.save_loss_review.is_some()
            || self.pending_import.is_some()
            || self.pending_image_open.is_some()
            || self.show_new
            || self.parameter_gesture.is_some()
            || self.live_gesture.is_some()
            || !self.points.is_empty()
            || !self.selection_path.is_empty()
            || self.gizmo_handle != 0
            || self.layer_drag.is_some()
            || self.eye_sweep.is_some()
            || self.tab_transfer.is_some()
            || self.tab_transfer_committing
            || self.free_transform.is_some()
            || self.geometry.is_some()
            || self.source_editor.is_some()
            || self.filter_editor.is_some()
            || self.refinement.is_some()
            || self.rename_edit.is_some()
            || self.color_editor.is_some()
            || self.project_settings.is_some()
            || self.task_undo_review.is_some()
            || self.proposal_review.is_some()
            || self.export_editor.is_some()
    }
    pub(super) fn update_restart_blocker(&self) -> Option<String> {
        if self.update_interaction_active() {
            Some(
                "Finish or cancel the current tool, edit or file operation before restarting."
                    .into(),
            )
        } else {
            crate::workspace::lifecycle::update_readiness(&self.workspace).err()
        }
    }
    pub(super) fn updates_dialog(&mut self, ctx: &egui::Context) {
        if !self.updates.open {
            return;
        }
        let blocker = self.update_restart_blocker();
        self.updates.show(ctx, blocker);
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

    #[test]
    fn actual_menu_preference_channel_and_later_clicks_persist_without_changing_artwork() {
        for (depth, owner) in [(8, Owner::Direct), (16, Owner::WindowsAppInstaller)] {
            let directory =
                std::env::temp_dir().join(format!("pb-update-controls-{}", crate::engine::id()));
            std::fs::create_dir(&directory).unwrap();
            let (mut app, ctx) = super::super::tests::fixture();
            app.recovery.open = false;
            app.updates = Panel::load(&directory);
            app.updates.owner = owner;
            // A recent manual attempt prevents this input test from making a network request.
            app.updates
                .store
                .save(updates::Preferences {
                    last_attempt: Some(updates::now()),
                    ..Default::default()
                })
                .unwrap();
            {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(32, 24, depth).unwrap();
                app.selected = engine.doc.layers[0].id.clone();
                for point in [[8, 8], [16, 16]] {
                    engine.edit("human", &[json!({"op":"paint","layer":app.selected,"points":[point],"radius":3,"color":[233,84,32,255]})], None, None, "Existing stroke").unwrap();
                }
                engine.undo("human").unwrap();
            }
            let before = serde_json::to_value(&app.shared.lock().unwrap().doc).unwrap();
            let frame = |app: &mut PeerBrush, events| {
                ctx.run(
                    egui::RawInput {
                        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                        events,
                        ..Default::default()
                    },
                    |ctx| app.draw(ctx),
                )
            };
            frame(&mut app, vec![]);
            let output = frame(&mut app, vec![]);
            frame(
                &mut app,
                click(label(&output, "File").expect("Actual File menu")),
            );
            let output = frame(&mut app, vec![]);
            frame(
                &mut app,
                click(label(&output, "Updates…").expect("Actual Updates entry")),
            );
            assert!(app.updates.open);
            frame(&mut app, vec![]);
            let output = frame(&mut app, vec![]);
            assert!(label(&output, owner.description()).is_some());
            assert_eq!(
                label(&output, "Public release notes do not establish your Store or package provider's update availability.").is_some(),
                owner != Owner::Direct,
            );
            assert!(label(
                &output,
                "Automatic installation and restart are not available in this development build."
            )
            .is_some());
            frame(
                &mut app,
                click(
                    label(&output, "Check official releases daily")
                        .expect("Actual preferences checkbox"),
                ),
            );
            assert!(app.updates.store.preferences.automatic_checks);
            let output = frame(&mut app, vec![]);
            frame(
                &mut app,
                click(label(&output, "Stable").expect("Actual channel selector")),
            );
            let output = frame(&mut app, vec![]);
            frame(
                &mut app,
                click(
                    label(&output, "Preview, including prereleases")
                        .expect("Actual Preview option"),
                ),
            );
            assert_eq!(app.updates.store.preferences.channel, Channel::Preview);
            let (sender, receiver) = mpsc::channel();
            app.updates.receiver = Some(receiver);
            let output = frame(&mut app, vec![]);
            frame(
                &mut app,
                click(label(&output, "Later").expect("Actual Later button")),
            );
            assert!(!app.updates.open);
            sender.send(Event::Complete(Ok(newer_report()))).unwrap();
            app.updates.tick(&ctx, true);
            assert!(!app.updates.open, "Later must defer an in-flight result");
            assert!(app.updates.receiver.is_none());
            let reloaded = updates::Store::load(&directory);
            assert!(reloaded.preferences.automatic_checks);
            assert_eq!(reloaded.preferences.channel, Channel::Preview);
            let engine = app.shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!((engine.undo.len(), engine.redo.len()), (1, 1));
            assert!(!engine.closed);
            drop(engine);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn a_failed_check_replaces_old_success_without_closing_or_editing_projects() {
        let directory = std::env::temp_dir().join(format!("pb-updates-ui-{}", crate::engine::id()));
        std::fs::create_dir(&directory).unwrap();
        let mut panel = Panel::load(&directory);
        let (sender, receiver) = mpsc::channel();
        panel.receiver = Some(receiver);
        panel.result = Some(Ok(Report {
            current_version: "0.30.0".into(),
            channel: Channel::Stable,
            target: updates::Target::WindowsX64,
            release_version: None,
            release_url: None,
            notes: String::new(),
            assets: vec![],
            checked_at: 1,
        }));
        sender
            .send(Event::Complete(Err("Offline · retry later".into())))
            .unwrap();
        panel.tick(&egui::Context::default(), true);
        assert!(panel.receiver.is_none());
        assert!(matches!(panel.result, Some(Err(_))));
        assert!(!panel.open);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn newer_report() -> Report {
        Report {
            current_version: env!("CARGO_PKG_VERSION").into(),
            channel: Channel::Stable,
            target: updates::Target::current().unwrap(),
            release_version: Some("999.0.0".into()),
            release_url: None,
            notes: "Synthetic published notes".into(),
            assets: vec![],
            checked_at: 1,
        }
    }

    #[test]
    fn background_notice_waits_for_native_input_and_gesture_without_requiring_saved_artwork() {
        for depth in [8, 16] {
            let directory =
                std::env::temp_dir().join(format!("pb-update-defer-{}", crate::engine::id()));
            std::fs::create_dir(&directory).unwrap();
            let (mut app, ctx) = super::super::tests::fixture();
            app.updates = Panel::load(&directory);
            {
                let mut engine = app.shared.lock().unwrap();
                engine.doc = Document::new_depth(16, 16, depth).unwrap();
                let layer = engine.doc.layers[0].id.clone();
                engine.edit("human", &[json!({"op":"paint","layer":layer,"points":[[8,8]],"radius":3,"color":[233,84,32,255]})], None, None, "Unsaved human stroke").unwrap();
            }
            let (before, history) = {
                let engine = app.shared.lock().unwrap();
                (
                    serde_json::to_value(&engine.doc).unwrap(),
                    engine.undo.len(),
                )
            };
            assert!(crate::workspace::lifecycle::update_readiness(&app.workspace).is_err());
            let (sender, receiver) = mpsc::channel();
            app.updates.receiver = Some(receiver);
            sender.send(Event::Complete(Ok(newer_report()))).unwrap();
            let _ = ctx.run(
                egui::RawInput {
                    events: vec![egui::Event::Text("typing".into())],
                    ..Default::default()
                },
                |ctx| app.updates_tick(ctx),
            );
            assert!(!app.updates.open);
            app.live_gesture = Some("unfinished native human stroke".into());
            let _ = ctx.run(Default::default(), |ctx| app.updates_tick(ctx));
            assert!(!app.updates.open);
            // Explicit native menu opening stays immediate while a gesture exists.
            app.updates.open = true;
            let _ = ctx.run(Default::default(), |ctx| app.updates_tick(ctx));
            assert!(app.updates.open);
            app.updates.open = false;
            app.live_gesture = None;
            app.show_new = true;
            let _ = ctx.run(Default::default(), |ctx| app.updates_tick(ctx));
            assert!(!app.updates.open);
            app.show_new = false;
            let _ = ctx.run(Default::default(), |ctx| app.updates_tick(ctx));
            assert!(app.updates.open);
            let engine = app.shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!(engine.undo.len(), history);
            assert!(!engine.closed);
            drop(engine);
            std::fs::remove_dir_all(directory).unwrap();
        }
    }

    #[test]
    fn update_dialog_and_live_gesture_blocker_preserve_native_artwork_and_history() {
        for depth in [8, 16] {
            let (mut app, ctx) = super::super::tests::fixture();
            app.shared.lock().unwrap().doc = Document::new_depth(16, 16, depth).unwrap();
            let (before, revision, history) = {
                let engine = app.shared.lock().unwrap();
                (
                    engine.doc.export_png().unwrap(),
                    engine.doc.revision,
                    engine.undo.len(),
                )
            };
            assert!(app.update_restart_blocker().is_none());
            app.show_new = true;
            assert!(app.update_restart_blocker().is_some());
            app.show_new = false;
            app.connecting = true;
            assert!(app.update_restart_blocker().is_some());
            app.connecting = false;
            app.live_gesture = Some("unfinished human stroke".into());
            assert!(app.update_restart_blocker().is_some());
            app.updates.open = true;
            let _ = ctx.run(
                egui::RawInput {
                    screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                    ..Default::default()
                },
                |ctx| app.updates_dialog(ctx),
            );
            let engine = app.shared.lock().unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), before);
            assert_eq!(engine.doc.revision, revision);
            assert_eq!(engine.undo.len(), history);
            assert!(!engine.closed);
        }
    }
}
