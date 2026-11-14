use super::*;
use crate::effects::patterns;
use crate::engine::view::EffectView;
use std::sync::{mpsc, Arc, Mutex};
type ResultReceiver = Arc<Mutex<mpsc::Receiver<Result<Option<String>, String>>>>;
#[derive(Clone)]
enum Settings {
    Resident(Arc<Value>),
    Native(crate::psd::memory36::Admitted<Value>),
}
impl AsRef<Value> for Settings {
    fn as_ref(&self) -> &Value {
        match self {
            Self::Resident(value) => value,
            Self::Native(value) => value,
        }
    }
}
impl std::ops::Deref for Settings {
    type Target = Value;
    fn deref(&self) -> &Value {
        self.as_ref()
    }
}
#[derive(Clone)]
struct Pending {
    project: String,
    document: String,
    revision: u64,
    layer: String,
    effect: String,
    kind: String,
    settings: Settings,
    receiver: ResultReceiver,
    native: Option<(
        crate::engine::native::DocumentKey,
        crate::large_document::ResidentBudget,
    )>,
    _charge: Option<Arc<crate::psd::memory36::MetadataCharge>>,
}
fn capture<'a>(
    engine: &crate::engine::Engine,
    observed: (&str, u64),
    layer: impl Into<LayerView<'a>>,
    effect: impl Into<EffectView<'a>>,
    receiver: ResultReceiver,
) -> Result<Pending, String> {
    let layer = layer.into();
    let effect = effect.into();
    let view = crate::engine::view::current(engine);
    if (view.id(), view.revision()) != observed {
        return Err(
            "Pattern source changed; finish the current edit before choosing a pattern".into(),
        );
    }
    let actual = view
        .find_layer(layer.id())
        .and_then(|layer| {
            (0..layer.effect_count())
                .filter_map(|i| layer.effect(i))
                .find(|entry| entry.id() == effect.id())
        })
        .ok_or("Pattern effect is no longer present")?;
    if actual.kind() != effect.kind() || !std::ptr::eq(actual.settings(), effect.settings()) {
        return Err("Pattern effect source changed".into());
    }
    let charge = view
        .native()
        .map(|native| {
            let bytes = [
                engine.project_id.len(),
                view.id().len(),
                layer.id().len(),
                effect.id().len(),
                effect.kind().len(),
            ]
            .into_iter()
            .try_fold(2048usize, |sum, n| sum.checked_add(n))
            .filter(|bytes| *bytes <= 64 * 1024 * 1024)
            .ok_or("Pattern metadata exceeds admission")?;
            crate::psd::memory36::MetadataCharge::reserve(native.budget(), bytes, &|| Ok(()))
        })
        .transpose()?;
    Ok(Pending {
        project: engine.project_id.clone(),
        document: observed.0.into(),
        revision: observed.1,
        layer: layer.id().to_owned(),
        effect: effect.id().to_owned(),
        kind: effect.kind().to_owned(),
        settings: match effect {
            EffectView::Resident(effect) => Settings::Resident(effect.settings.clone()),
            EffectView::Native(_) => Settings::Native(crate::engine::native::copy_admitted(
                effect.settings(),
                view.native()
                    .ok_or("Native pattern requires native authority")?
                    .budget(),
                true,
                &|| Ok(()),
            )?),
        },
        receiver,
        native: view
            .native()
            .map(|native| (native.key(), native.budget().clone())),
        _charge: charge,
    })
}
fn key() -> egui::Id {
    egui::Id::new("embedded pattern import")
}
fn command(pending: &Pending, source: &str) -> Value {
    let mut settings = pending.settings.as_ref().clone();
    let defaults = crate::effects::styles::defaults(&pending.kind);
    for key in ["color", "color2", "scale", "invert"] {
        if pending.kind == "texture" && matches!(key, "color" | "color2") {
            continue;
        }
        settings[key] = defaults[key].clone();
    }
    settings["pattern"] = json!("bitmap");
    settings["bitmap_png"] = json!(source);
    json!({"op":"effect.update", "layer":pending.layer, "effect":pending.effect,
        "settings":settings, "document_id":pending.document, "source_revision":pending.revision})
}
fn apply(app: &mut PeerBrush, pending: &Pending, source: &str) -> Result<(), String> {
    if app.edit_worker.pending() {
        return Err("An edit is still being applied; pattern import is waiting".into());
    }
    if app.project_id != pending.project {
        return Err("Pattern source changed while choosing the PNG · import cancelled".into());
    }
    let commands = if let Some((key, pool)) = &pending.native {
        let engine = app
            .shared
            .try_lock()
            .map_err(|_| "Pattern edit is waiting for the document")?;
        if crate::engine::view::current(&engine)
            .native()
            .is_none_or(|doc| doc.key() != *key)
        {
            return Err("Pattern source changed while choosing the PNG".into());
        }
        drop(engine);
        let Settings::Native(settings) = &pending.settings else {
            return Err("Native pattern metadata is missing".into());
        };
        let bytes = settings
            .owner()
            .bytes()
            .checked_add(source.len())
            .and_then(|n| n.checked_mul(4))
            .and_then(|n| n.checked_add(8192))
            .filter(|n| *n <= 64 * 1024 * 1024)
            .ok_or("Pattern command exceeds admission")?;
        let _working = crate::psd::memory36::MetadataCharge::reserve(pool, bytes, &|| Ok(()))?;
        edit_job::Commands::Native(crate::engine::native::copy_admitted(
            &command(pending, source),
            pool,
            true,
            &|| Ok(()),
        )?)
    } else {
        edit_job::Commands::resident(vec![command(pending, source)])
    };
    let ctx = app.edit_context.clone();
    app.submit_edit_job(
        edit_job::Request {
            project: pending.project.clone(),
            document: pending.document.clone(),
            revision: pending.revision,
            label: "Embed bitmap pattern".into(),
            selection_edit: false,
            origin: edit_job::Origin::Action,
        },
        commands,
        &ctx,
    )?;
    app.interrupt_animation();
    Ok(())
}

pub(super) fn poll(app: &mut PeerBrush, ctx: &egui::Context) {
    let Some(pending) = ctx.data(|d| d.get_temp::<Pending>(key())) else {
        return;
    };
    let cancelled = !app.effect_is_selected(&pending.layer, false, &pending.effect)
        || app.selected != pending.layer
        || app.parameter_gesture.is_some()
        || app.busy
        || app.lifecycle.is_some()
        || app.save_loss_review.is_some()
        || ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.pointer.any_pressed());
    let current = if cancelled {
        false
    } else {
        if app.edit_worker.pending() {
            ctx.request_repaint_after(Duration::from_millis(80));
            return;
        }
        let Ok(engine) = app.shared.try_lock() else {
            ctx.request_repaint_after(Duration::from_millis(80));
            return;
        };
        engine.project_id == pending.project
            && crate::workspace::guard(&engine, &pending.document, pending.revision).is_ok()
            && pending.native.as_ref().is_none_or(|(key, _)| {
                crate::engine::view::current(&engine)
                    .native()
                    .is_some_and(|doc| doc.key() == *key)
            })
    };
    if !current {
        ctx.data_mut(|d| {
            d.remove::<Pending>(key());
        });
        app.message = "Pattern source changed while choosing the PNG · import cancelled".into();
        return;
    }
    let result = pending.receiver.lock().unwrap().try_recv();
    match result {
        Err(mpsc::TryRecvError::Empty) => {
            ctx.request_repaint_after(std::time::Duration::from_millis(80));
            return;
        }
        Err(mpsc::TryRecvError::Disconnected) => {
            app.message = "Pattern import could not complete · current work preserved".into()
        }
        Ok(Ok(None)) => {}
        Ok(Err(error)) => app.message = error.into(),
        Ok(Ok(Some(source))) => {
            if let Err(error) = apply(app, &pending, &source) {
                app.message = error.into();
            }
        }
    }
    ctx.data_mut(|d| {
        d.remove::<Pending>(key());
    });
}
pub(super) fn control<'a>(
    app: &mut PeerBrush,
    ui: &mut egui::Ui,
    layer: impl Into<LayerView<'a>>,
    effect: impl Into<EffectView<'a>>,
    observed: (&str, u64),
) {
    let layer = layer.into();
    let effect = effect.into();
    let waiting = ui.ctx().data(|d| d.get_temp::<Pending>(key()).is_some());
    let response = ui.add_enabled(
        !waiting && !app.edit_worker.pending(),
        egui::Button::new(if waiting {
            "Choosing pattern…"
        } else {
            "Choose bitmap PNG…"
        }),
    );
    response.clone().on_hover_text("Embed a static PNG up to 512 × 512 pixels. Supported RGB ICC converts to sRGB at native depth; a 16-bit pattern promotes the document.");
    if response.clicked() {
        app.finish_parameter(false);
        if app.edit_worker.pending() {
            app.message = "Finish the current edit before choosing a pattern".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        let pending = capture(
            &app.shared.lock().unwrap(),
            observed,
            layer,
            effect,
            Arc::new(Mutex::new(rx)),
        );
        let pending = match pending {
            Ok(pending) => pending,
            Err(error) => {
                app.message = error.into();
                return;
            }
        };
        ui.ctx().data_mut(|d| d.insert_temp(key(), pending));
        let ctx = ui.ctx().clone();
        std::thread::spawn(move || {
            let result = std::panic::catch_unwind(|| {
                rfd::FileDialog::new()
                    .add_filter("PNG pattern", &["png"])
                    .pick_file()
                    .map(|path| patterns::read_file(&path))
                    .transpose()
            })
            .unwrap_or_else(|_| {
                Err("Pattern import could not complete · current work preserved".into())
            });
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }
    if matches!(effect, EffectView::Native(_)) {
        if patterns::active(effect.kind(), effect.settings()) {
            ui.label(
                RichText::new("Embedded bitmap PNG · canvas origin")
                    .size(11.)
                    .color(MUTED),
            );
        }
    } else if let Ok(Some(bitmap)) = patterns::from_settings(effect.kind(), effect.settings()) {
        ui.label(
            RichText::new(format!(
                "{} × {} · {} bit · canvas origin",
                bitmap.width, bitmap.height, bitmap.depth
            ))
            .size(11.)
            .color(MUTED),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (crate::engine::Engine, Pending, String) {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let mut e = crate::engine::Engine::new();
        let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
        e.edit(
            "human",
            &[json!({"op":"effect.add", "layer":layer, "kind":"pattern_overlay"})],
            None,
            None,
            "Pattern",
        )
        .unwrap();
        let effect = &e.resident_state().expect("resident fixture").document.layers[0].effects[0];
        let (_, rx) = mpsc::channel();
        let pending = Pending {
            project: e.project_id.clone(),
            document: e.resident_state().expect("resident fixture").document.id.clone(),
            revision: e.resident_state().expect("resident fixture").document.revision,
            layer,
            effect: effect.id.clone(),
            kind: effect.kind.clone(),
            settings: Settings::Resident(effect.settings.clone()),
            receiver: Arc::new(Mutex::new(rx)),
            native: None,
            _charge: None,
        };
        let png =
            STANDARD.encode(crate::raster::png16(1, 1, &[12345, 23457, 34569, 65535]).unwrap());
        (e, pending, png)
    }
    fn make_app(engine: crate::engine::Engine, pending: &Pending) -> (PeerBrush, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = PeerBrush::init(
            &ctx,
            Arc::new(Mutex::new(engine)),
            Connection {
                port: 0,
                token: String::new(),
                state_dir: Default::default(),
                instance_lock: None,
                availability: Default::default(),
            },
        );
        app.selected = pending.layer.clone();
        app.effect_selected = Some((pending.layer.clone(), false, pending.effect.clone()));
        (app, ctx)
    }
    #[test]
    fn completed_pattern_import_is_one_guarded_native_depth_edit() {
        let (e, pending, png) = fixture();
        let before = serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap();
        let history = e.resident_state().expect("resident fixture").undo.len();
        let (mut app, _) = make_app(e, &pending);
        apply(&mut app, &pending, &png).unwrap();
        super::super::text_tests::wait_edit(&mut app);
        let mut e = app.shared.lock().unwrap();
        assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, 16);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history + 1);
        e.undo("human").unwrap();
        assert_eq!(
            serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap()["layers"],
            before["layers"]
        );
        assert_eq!(e.resident_state().expect("resident fixture").document.bit_depth, 8);
    }
    #[test]
    fn picker_receipts_reject_source_changes_without_replacing_human_work() {
        for change in 0..3 {
            let (mut e, pending, png) = fixture();
            match change {
                0 => e.project_id = crate::engine::id(),
                1 => e.resident_state_mut().expect("resident fixture").document.id = crate::engine::id(),
                _ => {
                    e.edit(
                        "human",
                        &[json!({"op":"layer.update","layer":pending.layer,"name":"Human edit"})],
                        None,
                        None,
                        "Human edit",
                    )
                    .unwrap();
                }
            }
            let before = serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap();
            let history = e.resident_state().expect("resident fixture").undo.len();
            let (mut app, _) = make_app(e, &pending);
            let result = apply(&mut app, &pending, &png);
            if change == 0 {
                assert!(result.is_err());
            } else {
                result.unwrap();
                super::super::text_tests::wait_edit(&mut app);
                assert!(app.message.contains("changed"));
            }
            let e = app.shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(), before);
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history);
        }
    }
    #[test]
    fn ready_picker_receipts_cancel_for_selection_busy_and_escape_without_edits() {
        for change in 0..3 {
            let (e, mut pending, png) = fixture();
            let before = serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap();
            let history = e.resident_state().expect("resident fixture").undo.len();
            let shared = Arc::new(Mutex::new(e));
            let ctx = egui::Context::default();
            let mut app = PeerBrush::init(
                &ctx,
                shared.clone(),
                crate::server::Connection {
                    port: 0,
                    token: String::new(),
                    state_dir: Default::default(),
                    instance_lock: None,
                    availability: Default::default(),
                },
            );
            app.selected = pending.layer.clone();
            app.effect_selected = Some((pending.layer.clone(), false, pending.effect.clone()));
            let (tx, rx) = mpsc::channel();
            tx.send(Ok(Some(png))).unwrap();
            pending.receiver = Arc::new(Mutex::new(rx));
            ctx.data_mut(|d| d.insert_temp(key(), pending));
            match change {
                0 => app.selected.clear(),
                1 => app.busy = true,
                _ => {}
            }
            let events = if change == 2 {
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }]
            } else {
                vec![]
            };
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| poll(&mut app, ctx),
            );
            assert!(ctx.data(|d| d.get_temp::<Pending>(key()).is_none()));
            let e = shared.lock().unwrap();
            assert_eq!(serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(), before);
            assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history);
        }
    }

    #[test]
    fn inspector_procedural_switch_and_real_import_receipts_preserve_native_sources_and_late_work()
    {
        use super::super::workspace_tests as workspace;
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let size = Vec2::new(1100., 1800.);
        let asset = |depth, reverse| {
            let mut words = vec![
                12347, 23459, 34571, 65535, 53127, 13729, 29131, 32769, 17333, 62337, 9353, 65535,
                45139, 21341, 59347, 65535, 39349, 11351, 51353, 65535, 7357, 33359, 23363, 65535,
                63367, 45371, 15373, 32769, 25377, 57379, 37381, 65535,
            ];
            if reverse {
                words.reverse();
            }
            let png = if depth == 16 {
                crate::raster::png16(4, 2, &words)
            } else {
                crate::raster::png(
                    4,
                    2,
                    &words
                        .into_iter()
                        .map(crate::raster::project16)
                        .collect::<Vec<_>>(),
                )
            }
            .unwrap();
            STANDARD.encode(png)
        };
        for depth in [8, 16] {
            for outcome in ["ready", "newer", "escape"] {
                for kind in ["pattern_overlay", "texture"] {
                    let (mut app, ctx) = workspace::fixture(depth);
                    let old_png = asset(depth, false);
                    let new_png = asset(depth, true);
                    let effect = {
                        let mut engine = app.shared.lock().unwrap();
                        let layer = app.selected.clone();
                        engine
                            .edit(
                                "human",
                                &[json!({"op":"effect.add","layer":layer,
                        "kind":kind,"settings":{"pattern":"bitmap","bitmap_png":old_png,
                            "sampling":"nearest","bitmap_scale":2.,"phase_x":1.}})],
                                None,
                                None,
                                "Original embedded pattern",
                            )
                            .unwrap();
                        engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone()
                    };
                    app.select_effect(&app.selected.clone(), false, &effect);
                    workspace::draw(&mut app, &ctx, size, vec![]);
                    let output = workspace::draw(&mut app, &ctx, size, vec![]);
                    let before = app.shared.lock().unwrap().resident_state().expect("resident fixture").document.clone();
                    let count = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                    let choice = workspace::label(&output, "Bitmap PNG");
                    workspace::click(&mut app, &ctx, size, choice);
                    let output = workspace::draw(&mut app, &ctx, size, vec![]);
                    let procedural = workspace::label(&output, "Checker");
                    workspace::click(&mut app, &ctx, size, procedural);
                    {
                        let engine = app.shared.lock().unwrap();
                        let settings = &engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings;
                        assert_eq!(settings["pattern"], "checker");
                        for key in crate::effects::styles::bitmap_defaults()
                            .as_object()
                            .unwrap()
                            .keys()
                        {
                            assert!(
                                settings.get(key).is_none(),
                                "Removed bitmap field {key} returned"
                            );
                        }
                        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), count + 1);
                        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
                        assert_ne!(
                            engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                            before.export_png().unwrap()
                        );
                    }
                    let (tx, rx) = mpsc::channel();
                    let pending = {
                        let engine = app.shared.lock().unwrap();
                        let current = &engine.resident_state().expect("resident fixture").document.layers[0].effects[0];
                        Pending {
                            project: engine.project_id.clone(),
                            document: engine.resident_state().expect("resident fixture").document.id.clone(),
                            revision: engine.resident_state().expect("resident fixture").document.revision,
                            layer: app.selected.clone(),
                            effect: effect.clone(),
                            kind: current.kind.clone(),
                            settings: Settings::Resident(current.settings.clone()),
                            receiver: Arc::new(Mutex::new(rx)),
                            native: None,
                            _charge: None,
                        }
                    };
                    if outcome == "newer" {
                        app.shared
                            .lock()
                            .unwrap()
                            .edit(
                                "human",
                                &[json!({"op":"layer.update",
                        "layer":app.selected,"name":"Newer human work"})],
                                None,
                                None,
                                "Newer work",
                            )
                            .unwrap();
                    }
                    let retained = workspace::source(&app);
                    let history = app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len();
                    tx.send(Ok(Some(new_png.clone()))).unwrap();
                    ctx.data_mut(|data| data.insert_temp(key(), pending));
                    let events = if outcome == "escape" {
                        vec![egui::Event::Key {
                            key: egui::Key::Escape,
                            physical_key: None,
                            pressed: true,
                            repeat: false,
                            modifiers: Default::default(),
                        }]
                    } else {
                        vec![]
                    };
                    workspace::draw(&mut app, &ctx, size, events);
                    super::super::text_tests::wait_edit(&mut app);
                    assert!(ctx.data(|data| data.get_temp::<Pending>(key()).is_none()));
                    if outcome == "ready" {
                        {
                            let engine = app.shared.lock().unwrap();
                            assert_eq!(
                                engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["pattern"],
                                "bitmap"
                            );
                            assert_eq!(
                                engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["bitmap_png"],
                                json!(new_png)
                            );
                            assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), history + 1);
                            assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, depth);
                            assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].pixels, before.layers[0].pixels);
                        }
                        let imported = workspace::source(&app);
                        for _ in 0..3 {
                            workspace::draw(&mut app, &ctx, size, vec![]);
                        }
                        assert_eq!(workspace::source(&app), imported);
                        let mut engine = app.shared.lock().unwrap();
                        engine.undo("human").unwrap();
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["pattern"],
                            "checker"
                        );
                        assert!(engine.resident_state().expect("resident fixture").document.layers[0].effects[0]
                            .settings
                            .get("bitmap_png")
                            .is_none());
                        engine.undo("human").unwrap();
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["bitmap_png"],
                            json!(old_png)
                        );
                        assert_eq!(
                            engine.resident_state().expect("resident fixture").document.export_png().unwrap(),
                            before.export_png().unwrap()
                        );
                    } else {
                        assert_eq!(workspace::source(&app), retained, "{depth}/{outcome}");
                        assert_eq!(app.shared.lock().unwrap().resident_state().expect("resident fixture").undo.len(), history);
                    }
                }
            }
        }
    }
    #[test]
    fn ready_pattern_receipts_wait_without_blocking_or_consuming_during_another_edit() {
        for outcome in ["held", "commit", "cancel", "escape"] {
            let (engine, mut pending, png) = fixture();
            let history = engine.history_lengths();
            let (mut app, ctx) = make_app(engine, &pending);
            let (tx, rx) = mpsc::channel();
            tx.send(Ok(Some(png))).unwrap();
            pending.receiver = Arc::new(Mutex::new(rx));
            let settings = pending.settings.clone();
            ctx.data_mut(|data| data.insert_temp(key(), pending.clone()));
            let shared = app.shared.clone();
            let guard = shared.lock().unwrap();
            if outcome != "held" {
                app.submit_edit_job(
                    edit_job::Request {
                        project: pending.project.clone(),
                        document: pending.document.clone(),
                        revision: pending.revision,
                        label: "Other edit".into(),
                        selection_edit: false,
                        origin: edit_job::Origin::Action,
                    },
                    edit_job::Commands::resident(vec![
                        json!({"op":"layer.update","layer":pending.layer,"name":"Other work"}),
                    ]),
                    &ctx,
                )
                .unwrap();
            }
            let events = if outcome == "escape" {
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: Default::default(),
                }]
            } else {
                vec![]
            };
            let _ = ctx.run(
                egui::RawInput {
                    events,
                    ..Default::default()
                },
                |ctx| poll(&mut app, ctx),
            );
            let saved = ctx.data(|data| data.get_temp::<Pending>(key()));
            assert_eq!(saved.is_some(), outcome != "escape");
            if let Some(saved) = saved {
                assert!(
                    matches!((&settings, &saved.settings), (Settings::Resident(before), Settings::Resident(after)) if Arc::ptr_eq(before, after))
                );
            }
            assert_eq!(guard.history_lengths(), history);
            if matches!(outcome, "cancel" | "escape") {
                app.cancel_pending_edit();
            }
            drop(guard);
            if outcome != "held" {
                super::super::text_tests::wait_edit(&mut app);
            }
            let _ = ctx.run(egui::RawInput::default(), |ctx| poll(&mut app, ctx));
            super::super::text_tests::wait_edit(&mut app);
            assert!(ctx.data(|data| data.get_temp::<Pending>(key()).is_none()));
            let engine = shared.lock().unwrap();
            if matches!(outcome, "held" | "cancel") {
                assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 16);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings["pattern"],
                    "bitmap"
                );
                assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
            } else {
                assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 8);
                assert_eq!(
                    engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings.as_ref(),
                    settings.as_ref()
                );
                assert_eq!(
                    engine.history_lengths(),
                    if outcome == "commit" {
                        [history[0] + 1, 0]
                    } else {
                        history
                    }
                );
                if outcome == "commit" {
                    assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].name, "Other work");
                }
            }
        }
    }

    #[test]
    fn queued_pattern_command_obeys_cancellation_reservations_and_source_identity() {
        for outcome in ["commit", "cancel", "reserved", "replacement"] {
            let (mut engine, pending, png) = fixture();
            if outcome == "reserved" {
                engine
                    .reserve(
                        "agent",
                        "Protected pattern",
                        vec![crate::engine::Scope::layer(&pending.layer)],
                    )
                    .unwrap();
            }
            if outcome == "replacement" {
                engine.resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
            }
            let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
            let history = engine.history_lengths();
            let (mut app, _) = make_app(engine, &pending);
            let shared = app.shared.clone();
            let guard = shared.lock().unwrap();
            apply(&mut app, &pending, &png).unwrap();
            assert!(app.message.starts_with("Applying"));
            assert_eq!(guard.history_lengths(), history);
            if outcome == "cancel" {
                app.cancel_pending_edit();
            }
            drop(guard);
            super::super::text_tests::wait_edit(&mut app);
            let engine = shared.lock().unwrap();
            if outcome == "commit" {
                assert_eq!(engine.history_lengths(), [history[0] + 1, 0]);
                assert_eq!(engine.resident_state().expect("resident fixture").document.bit_depth, 16);
                let settings = &engine.resident_state().expect("resident fixture").document.layers[0].effects[0].settings;
                let bitmap = patterns::from_settings(&pending.kind, settings)
                    .unwrap()
                    .unwrap();
                assert_eq!(bitmap.depth, 16);
                assert_eq!(settings["bitmap_png"], png);
                assert_eq!(app.message.as_ref(), "Embed bitmap pattern");
            } else {
                assert_eq!(engine.history_lengths(), history);
                assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
            }
        }
    }
    #[test]
    fn picker_capture_rejects_replacement_documents_and_shares_immutable_settings() {
        let (mut engine, observed, _) = fixture();
        let layer = engine.resident_state().expect("resident fixture").document.layers[0].clone();
        let effect = &layer.effects[0];
        let (_, rx) = mpsc::channel();
        let receiver = Arc::new(Mutex::new(rx));
        let captured = capture(
            &engine,
            (&observed.document, observed.revision),
            &layer,
            effect,
            receiver.clone(),
        )
        .unwrap();
        assert!(
            matches!(&captured.settings, Settings::Resident(value) if Arc::ptr_eq(value, &effect.settings))
        );
        engine.resident_state_mut().expect("resident fixture").document.id = crate::engine::id();
        assert!(capture(
            &engine,
            (&observed.document, observed.revision),
            &layer,
            effect,
            receiver.clone()
        )
        .is_err());
        engine.resident_state_mut().expect("resident fixture").document.id = observed.document.clone();
        engine.resident_state_mut().expect("resident fixture").document.revision += 1;
        assert!(capture(
            &engine,
            (&observed.document, observed.revision),
            &layer,
            effect,
            receiver
        )
        .is_err());
    }
}
