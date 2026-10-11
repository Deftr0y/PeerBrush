//! Source-bound compatibility inspection and explicit loss review for PSD/PSB saves.
use super::*;
use crate::compatibility::{Representation, StandardLayout};

pub(super) enum SaveIntent {
    Save { save_as: bool },
    Close { save_as: bool },
}
pub(super) struct SaveReview {
    project: String,
    name: String,
    report: crate::compatibility::SaveReport,
    intent: SaveIntent,
    pub(super) buttons: [Option<Rect>; 2],
}
impl SaveReview {
    pub(super) fn new(
        project: String,
        name: String,
        report: crate::compatibility::SaveReport,
        intent: SaveIntent,
    ) -> Self {
        Self {
            project,
            name,
            report,
            intent,
            buttons: [None; 2],
        }
    }
}

impl PeerBrush {
    pub(super) fn save_loss_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut review) = self.save_loss_review.take() else {
            return;
        };
        self.lifecycle_frame = true;
        if ctx.input(|input| input.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        let shared = crate::workspace::get_in(&self.workspace, &review.project);
        let validity = shared
            .and_then(|shared| {
                let result = match shared.try_lock() {
                    Ok(engine) => crate::workspace::guard(
                        &engine,
                        &review.report.document_id,
                        review.report.revision,
                    )
                    .map(|_| true),
                    Err(_) => Ok(false),
                };
                result
            })
            .and_then(|ready| {
                if matches!(review.intent, SaveIntent::Save { .. })
                    && self.project_id != review.project
                {
                    Err("Active project changed".into())
                } else {
                    Ok(ready)
                }
            });
        let ready = match validity {
            Ok(ready) => ready,
            Err(_) => {
                self.message =
                    "Document changed · review its current PSD/PSB output before saving".into();
                self.cancel_close_save_loss();
                return;
            }
        } && !self.jobs.contains(&review.project);
        let mut accepted = false;
        let mut cancelled = false;
        review.buttons = [None; 2];
        let modal = egui::Modal::new(egui::Id::new("PSD save loss review")).show(ctx, |ui| {
            ui.set_max_width(540.);
            ui.heading("Review PSD / PSB output");
            ui.label(RichText::new(&review.name).strong());
            ui.label(format!("{} bit · {} standard layers/folders · current merged image", review.report.bit_depth, review.report.standard_layer_count));
            ui.label("The saved appearance and native channel depth are preserved. Some settings require PeerBrush sources to stay editable; other editors receive the standard rendered result.");
            egui::ScrollArea::vertical().id_salt("save loss details").max_height(330.).show(ui, |ui| {
                for loss in &review.report.losses {
                    if let Some(layer) = loss.layer_id.as_ref().and_then(|id| review.report.layers.iter().find(|layer| &layer.id == id)) {
                        ui.label(RichText::new(&layer.name).strong());
                    }
                    ui.label(RichText::new(loss.message).color(ACCENT));
                    ui.add_space(4.);
                }
            });
            if !ready { ui.label("Waiting for the current document operation…"); }
            ui.horizontal(|ui| {
                let save = ui.add_enabled(ready, egui::Button::new("Save with these changes"));
                review.buttons[0] = Some(save.rect);
                accepted = save.clicked();
                let cancel = ui.button("Cancel");
                review.buttons[1] = Some(cancel.rect);
                cancelled = cancel.clicked();
            });
        });
        if cancelled || modal.should_close() {
            self.message = "Save canceled · current work remains open".into();
            self.cancel_close_save_loss();
        } else if accepted {
            match review.intent {
                SaveIntent::Save { save_as } => self.save_frozen(
                    save_as,
                    review.report.document_id,
                    review.report.revision,
                    true,
                ),
                SaveIntent::Close { save_as } => self.save_for_close_with_acceptance(
                    ctx,
                    save_as,
                    Some((&review.report.document_id, review.report.revision)),
                ),
            }
        } else {
            self.save_loss_review = Some(review);
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
    pub(super) fn open_save_report(&mut self) {
        let result = crate::compatibility::report(&self.shared.lock().unwrap().doc);
        match result {
            Ok(report) => self.save_report = Some(report),
            Err(error) => self.message = error,
        }
    }
    pub(super) fn compatibility_window(&mut self, ctx: &egui::Context, doc: &Document) {
        let Some(report) = self.save_report.as_ref() else {
            return;
        };
        if report.document_id != doc.id || report.revision != doc.revision {
            self.save_report = None;
            self.message =
                "Document changed · reopen PSD compatibility to review current sources".into();
            return;
        }
        let mut open = true;
        egui::Window::new("PSD / PSB compatibility")
            .id(egui::Id::new("save compatibility"))
            .open(&mut open).default_width(560.).resizable(true)
            .show(ctx, |ui| {
                ui.label(format!("{} bit · revision {}", report.bit_depth, report.revision));
                ui.label(match report.standard_layout {
                    StandardLayout::Protected => "This source is protected. Create an explicit compatible copy before saving.",
                    StandardLayout::LayerTree => "Standard output contains separate raster layers and supported folders.",
                    StandardLayout::CompositeOnly => "Standard output contains one baked composite layer. The original tree requires PeerBrush sources.",
                    StandardLayout::CompositeAndHiddenSources => "Standard output contains a visible filtered composite and hidden original layer roots.",
                });
                if report.save_allowed {
                    ui.label(format!("{} standard layers/folders · current merged composite · native channel depth", report.standard_layer_count));
                    ui.label("Standard rasters and masks preserve the saved result. Supplementary PeerBrush sources retain additional editable definitions; receiving applications may ignore them.");
                    ui.label(if report.output_profile == "embedded_srgb" { "Embedded sRGB profile · no profile conversion during saving" } else { "Untagged sRGB working space · no profile conversion during saving" });
                }
                egui::ScrollArea::vertical().id_salt("compatibility details").max_height(400.).show(ui, |ui| {
                    for reason in &report.protected_reasons { ui.label(RichText::new(reason).color(ACCENT)); }
                    if report.losses.is_empty() && report.save_allowed {
                        ui.label("No baking or supplementary-source limitation was found in this document's standard layer plan.");
                    }
                    for loss in &report.losses {
                        if let Some(layer) = loss.layer_id.as_ref().and_then(|id| report.layers.iter().find(|layer| &layer.id == id)) {
                            ui.label(RichText::new(&layer.name).strong());
                        }
                        ui.label(RichText::new(loss.message).color(ACCENT));
                        ui.add_space(4.);
                    }
                    ui.separator();
                    ui.label(RichText::new("Layer representation").strong());
                    for layer in &report.layers {
                        let representation = match layer.standard {
                            Representation::Raster => "editable raster",
                            Representation::Folder => "folder",
                            Representation::BakedFolderRaster => "folder baked to raster",
                            Representation::BakedIntoFolder => "included in a baked folder",
                            Representation::CompositeOnly => "included in the composite",
                            Representation::ProtectedPreview => "protected preview",
                        };
                        ui.label(format!("{} · {representation}{}", layer.name, if layer.standard_mask { " · raster mask" } else { "" }));
                        if matches!(layer.standard, Representation::Raster | Representation::BakedFolderRaster) {
                            if let Some([left, top, right, bottom]) = layer.standard_bounds { ui.label(format!("Standard bounds: {left}, {top} → {right}, {bottom}")); }
                        }
                        if !layer.peerbrush_sources.is_empty() {
                            let definitions = layer.peerbrush_sources.iter().map(|source| match *source {
                                "effect_stack" => "editable effects", "mask_stack" => "editable mask steps",
                                "editable_text_vector_source" => "editable text/vector definitions",
                                "transform_originals" => "original pixels and transform matrices",
                                "folder_hierarchy" => "original folder and children", _ => "original layer",
                            }).collect::<Vec<_>>().join(", ");
                            ui.label(format!("PeerBrush sources retain: {definitions}"));
                        }
                    }
                    if report.details_truncated { ui.label("Long protected-source reasons were shortened for this report."); }
                });
                ui.label("This report describes the codec's output. Receiving-application round trips and full Photoshop compatibility are not guaranteed.");
            });
        if !open {
            self.save_report = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    fn fixture(depth: u16) -> (PeerBrush, egui::Context, Shared, PathBuf) {
        let ctx = egui::Context::default();
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(16, 12, depth).unwrap();
        engine.doc.layers[0]
            .pixels
            .set16(3, 4, [12347, 23459, 34571, 65535]);
        let shared = Arc::new(std::sync::Mutex::new(engine));
        let state =
            std::env::temp_dir().join(format!("peerbrush-save-review-{}", crate::engine::id()));
        std::fs::create_dir_all(&state).unwrap();
        let path = state.join("Reviewed.psd");
        server::save(&shared, &path).unwrap();
        {
            let mut e = shared.lock().unwrap();
            let layer = e.doc.layers[0].id.clone();
            e.edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"invert","settings":{}})],
                None,
                None,
                "Editable effect",
            )
            .unwrap();
        }
        let mut app = PeerBrush::init(
            &ctx,
            shared.clone(),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: state,
                instance_lock: None,
                availability: Default::default(),
            },
        );
        frame(&mut app, &ctx, vec![]);
        (app, ctx, shared, path)
    }
    fn frame(app: &mut PeerBrush, ctx: &egui::Context, events: Vec<egui::Event>) {
        let _ = ctx.run(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1360., 900.))),
                focused: true,
                events,
                ..Default::default()
            },
            |ctx| app.draw(ctx),
        );
    }
    fn click(app: &mut PeerBrush, ctx: &egui::Context, loss: bool, index: usize) {
        for _ in 0..3 {
            frame(app, ctx, vec![]);
        }
        let rect = if loss {
            app.save_loss_review.as_ref().unwrap().buttons[index]
        } else {
            app.lifecycle.as_ref().unwrap().buttons[index]
        }
        .unwrap();
        for pressed in [true, false] {
            let mut events = vec![
                egui::Event::PointerMoved(rect.center()),
                egui::Event::PointerButton {
                    pos: rect.center(),
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: Default::default(),
                },
            ];
            if loss && index == 1 && !pressed {
                events.push(egui::Event::Key {
                    key: egui::Key::Delete,
                    physical_key: Some(egui::Key::Delete),
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                });
            }
            frame(app, ctx, events);
        }
    }
    fn wait_save(app: &mut PeerBrush, ctx: &egui::Context, shared: &Shared, revision: u64) {
        for _ in 0..200 {
            frame(app, ctx, vec![]);
            if shared.lock().unwrap().saved_revision == revision {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("Reviewed save did not finish: {}", app.message);
    }
    #[test]
    fn native_loss_review_waits_for_existing_file_work() {
        let (mut app, ctx, shared, path) = fixture(16);
        let before = std::fs::read(&path).unwrap();
        app.busy = true;
        app.save(false);
        assert!(app.save_loss_review.is_none());
        app.busy = false;
        app.save(false);
        app.jobs.insert(app.project_id.clone());
        click(&mut app, &ctx, true, 0);
        assert!(app.save_loss_review.is_some());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        app.jobs.clear();
        click(&mut app, &ctx, true, 1);
        let state = app.connection.state_dir.clone();
        drop(app);
        drop(shared);
        std::fs::remove_dir_all(state).unwrap();
    }
    #[test]
    fn native_loss_review_cancel_owns_keyboard_canvas_and_cancellation_frame() {
        for depth in [8, 16] {
            let (mut app, ctx, shared, path) = fixture(depth);
            let before = serde_json::to_value(&shared.lock().unwrap().doc).unwrap();
            let file = std::fs::read(&path).unwrap();
            app.save(false);
            frame(&mut app, &ctx, vec![]);
            assert!(app.save_loss_review.is_some());
            assert!(!app.busy);
            let canvas = app.view_rect.unwrap();
            let point = canvas.left_bottom() + Vec2::new(3., -3.);
            for pressed in [true, false] {
                frame(
                    &mut app,
                    &ctx,
                    vec![
                        egui::Event::PointerMoved(point),
                        egui::Event::PointerButton {
                            pos: point,
                            button: egui::PointerButton::Primary,
                            pressed,
                            modifiers: Default::default(),
                        },
                    ],
                );
            }
            // Backdrop dismissal is cancellation too; reopen to exercise the
            // explicit button and its same-frame Delete event below.
            if app.save_loss_review.is_none() {
                app.save(false);
                frame(&mut app, &ctx, vec![]);
            }
            frame(
                &mut app,
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: Some(egui::Key::Z),
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::COMMAND,
                }],
            );
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                before
            );
            click(&mut app, &ctx, true, 1);
            assert!(app.save_loss_review.is_none());
            assert!(app.lifecycle_frame);
            assert!(!shared.lock().unwrap().closed);
            assert_eq!(std::fs::read(&path).unwrap(), file);
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                before
            );
            let state = app.connection.state_dir.clone();
            drop(app);
            drop(shared);
            std::fs::remove_dir_all(state).unwrap();
        }
    }
    #[test]
    fn native_loss_review_rejects_changed_sources_and_project_switches() {
        for depth in [8, 16] {
            for switch in [false, true] {
                let (mut app, ctx, shared, path) = fixture(depth);
                let file = std::fs::read(&path).unwrap();
                app.save(false);
                frame(&mut app, &ctx, vec![]);
                if switch {
                    let next = crate::workspace::new_project(&shared, 16, 12, depth, None).unwrap();
                    let project = next.lock().unwrap().project_id.clone();
                    app.switch_project(&project);
                } else {
                    let mut e = shared.lock().unwrap();
                    let layer = e.doc.layers[0].id.clone();
                    e.edit("human", &[json!({"op":"paint.fill","layer":layer,"rect":[0,0,2,2],"color":[22,44,66,255]})], None,None,"Later human pixels").unwrap();
                }
                let current = shared.lock().unwrap().doc.export_png().unwrap();
                frame(&mut app, &ctx, vec![]);
                assert!(app.save_loss_review.is_none());
                assert!(!app.busy);
                assert_eq!(std::fs::read(&path).unwrap(), file);
                assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), current);
                assert!(!shared.lock().unwrap().closed);
                let state = app.connection.state_dir.clone();
                drop(app);
                drop(shared);
                std::fs::remove_dir_all(state).unwrap();
            }
        }
    }
    #[test]
    fn native_loss_acceptance_saves_current_native_sources_without_changing_history() {
        for depth in [8, 16] {
            let (mut app, ctx, shared, path) = fixture(depth);
            let before = shared.lock().unwrap().doc.clone();
            let history = shared.lock().unwrap().undo.len();
            app.save(false);
            click(&mut app, &ctx, true, 0);
            wait_save(&mut app, &ctx, &shared, before.revision);
            let decoded = crate::psd::decode(&std::fs::read(&path).unwrap()).unwrap();
            assert!(!decoded.read_only);
            assert_eq!(decoded.bit_depth, depth);
            assert_eq!(
                decoded.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(decoded.layers[0].effects.len(), 1);
            assert_eq!(decoded.export_png().unwrap(), before.export_png().unwrap());
            assert_eq!(shared.lock().unwrap().undo.len(), history);
            assert_eq!(shared.lock().unwrap().doc.revision, before.revision);
            let state = app.connection.state_dir.clone();
            drop(app);
            drop(shared);
            std::fs::remove_dir_all(state).unwrap();
        }
    }
    #[test]
    fn close_review_keeps_work_open_on_loss_cancel_and_closes_only_after_accepted_save() {
        for depth in [8, 16] {
            let (mut app, ctx, shared, path) = fixture(depth);
            let before = shared.lock().unwrap().doc.clone();
            let file = std::fs::read(&path).unwrap();
            let project = shared.lock().unwrap().project_id.clone();
            app.request_close(lifecycle::Intent::Close(project));
            click(&mut app, &ctx, false, 0);
            assert!(app.save_loss_review.is_some());
            assert!(!shared.lock().unwrap().closed);
            assert_eq!(std::fs::read(&path).unwrap(), file);
            click(&mut app, &ctx, true, 1);
            assert!(app.lifecycle.is_some());
            assert!(!shared.lock().unwrap().closed);
            click(&mut app, &ctx, false, 0);
            click(&mut app, &ctx, true, 0);
            wait_save(&mut app, &ctx, &shared, before.revision);
            for _ in 0..20 {
                frame(&mut app, &ctx, vec![]);
                if shared.lock().unwrap().closed {
                    break;
                }
            }
            assert!(shared.lock().unwrap().closed);
            let decoded = crate::psd::decode(&std::fs::read(&path).unwrap()).unwrap();
            assert_eq!(decoded.export_png().unwrap(), before.export_png().unwrap());
            assert_eq!(decoded.layers[0].effects.len(), 1);
            let state = app.connection.state_dir.clone();
            drop(app);
            drop(shared);
            std::fs::remove_dir_all(state).unwrap();
        }
    }
}
