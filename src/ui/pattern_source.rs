use super::*;
use crate::effects::{patterns, Effect};
use std::sync::{mpsc, Arc, Mutex};
type ResultReceiver = Arc<Mutex<mpsc::Receiver<Result<Option<String>, String>>>>;
#[derive(Clone)]
struct Pending {
    project: String,
    document: String,
    revision: u64,
    layer: String,
    effect: String,
    kind: String,
    settings: Value,
    receiver: ResultReceiver,
}
fn key() -> egui::Id {
    egui::Id::new("embedded pattern import")
}
fn apply(
    engine: &mut crate::engine::Engine,
    pending: &Pending,
    source: &str,
) -> Result<(), String> {
    if engine.project_id != pending.project
        || engine.doc.id != pending.document
        || engine.doc.revision != pending.revision
    {
        return Err("Pattern source changed while choosing the PNG · import cancelled".into());
    }
    let mut settings = pending.settings.clone();
    let defaults = crate::effects::styles::defaults(&pending.kind);
    for key in ["color", "color2", "scale", "invert"] {
        if pending.kind == "texture" && matches!(key, "color" | "color2") {
            continue;
        }
        settings[key] = defaults[key].clone();
    }
    settings["pattern"] = json!("bitmap");
    settings["bitmap_png"] = json!(source);
    engine.edit("human", &[json!({"op":"effect.update", "layer":pending.layer, "effect":pending.effect, "settings":settings})], Some(pending.revision), None, "Embed bitmap pattern")?;
    Ok(())
}
pub(super) fn poll(app: &mut PeerBrush, ctx: &egui::Context) {
    let Some(pending) = ctx.data(|d| d.get_temp::<Pending>(key())) else {
        return;
    };
    let current = {
        let engine = app.shared.lock().unwrap();
        engine.project_id == pending.project
            && engine.doc.id == pending.document
            && engine.doc.revision == pending.revision
            && app.effect_is_selected(&pending.layer, false, &pending.effect)
            && app.selected == pending.layer
    };
    if !current
        || app.parameter_gesture.is_some()
        || app.busy
        || app.lifecycle.is_some()
        || app.save_loss_review.is_some()
        || ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.pointer.any_pressed())
    {
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
        Ok(Err(error)) => app.message = error,
        Ok(Ok(Some(source))) => {
            app.animation.cancel();
            app.message = apply(&mut app.shared.lock().unwrap(), &pending, &source)
                .map(|_| "Embedded bitmap pattern".into())
                .unwrap_or_else(|e| e);
            app.last_preview = None;
        }
    }
    ctx.data_mut(|d| {
        d.remove::<Pending>(key());
    });
}
pub(super) fn control(
    app: &mut PeerBrush,
    ui: &mut egui::Ui,
    layer: &crate::engine::Layer,
    effect: &Effect,
    revision: u64,
) {
    let waiting = ui.ctx().data(|d| d.get_temp::<Pending>(key()).is_some());
    let response = ui.add_enabled(
        !waiting,
        egui::Button::new(if waiting {
            "Choosing pattern…"
        } else {
            "Choose bitmap PNG…"
        }),
    );
    response.clone().on_hover_text("Embed a static PNG up to 512 × 512 pixels. Supported RGB ICC converts to sRGB at native depth; a 16-bit pattern promotes the document.");
    if response.clicked() {
        app.finish_parameter(false);
        let engine = app.shared.lock().unwrap();
        if engine.doc.revision != revision {
            app.message = "Finish the current edit before choosing a pattern".into();
            return;
        }
        let (tx, rx) = mpsc::channel();
        let pending = Pending {
            project: engine.project_id.clone(),
            document: engine.doc.id.clone(),
            revision,
            layer: layer.id.clone(),
            effect: effect.id.clone(),
            kind: effect.kind.clone(),
            settings: effect.settings.as_ref().clone(),
            receiver: Arc::new(Mutex::new(rx)),
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
    if let Ok(Some(bitmap)) = patterns::from_settings(&effect.kind, &effect.settings) {
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
        let layer = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[json!({"op":"effect.add", "layer":layer, "kind":"pattern_overlay"})],
            None,
            None,
            "Pattern",
        )
        .unwrap();
        let effect = &e.doc.layers[0].effects[0];
        let (_, rx) = mpsc::channel();
        let pending = Pending {
            project: e.project_id.clone(),
            document: e.doc.id.clone(),
            revision: e.doc.revision,
            layer,
            effect: effect.id.clone(),
            kind: effect.kind.clone(),
            settings: effect.settings.as_ref().clone(),
            receiver: Arc::new(Mutex::new(rx)),
        };
        let png =
            STANDARD.encode(crate::raster::png16(1, 1, &[12345, 23457, 34569, 65535]).unwrap());
        (e, pending, png)
    }
    #[test]
    fn completed_pattern_import_is_one_guarded_native_depth_edit() {
        let (mut e, pending, png) = fixture();
        let before = serde_json::to_value(&e.doc).unwrap();
        let history = e.undo.len();
        apply(&mut e, &pending, &png).unwrap();
        assert_eq!(e.doc.bit_depth, 16);
        assert_eq!(e.undo.len(), history + 1);
        e.undo("human").unwrap();
        assert_eq!(
            serde_json::to_value(&e.doc).unwrap()["layers"],
            before["layers"]
        );
        assert_eq!(e.doc.bit_depth, 8);
    }
    #[test]
    fn picker_receipts_reject_source_changes_without_replacing_human_work() {
        for change in 0..3 {
            let (mut e, pending, png) = fixture();
            match change {
                0 => e.project_id = crate::engine::id(),
                1 => e.doc.id = crate::engine::id(),
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
            let before = serde_json::to_value(&e.doc).unwrap();
            let history = e.undo.len();
            assert!(apply(&mut e, &pending, &png).is_err());
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert_eq!(e.undo.len(), history);
        }
    }
    #[test]
    fn ready_picker_receipts_cancel_for_selection_busy_and_escape_without_edits() {
        for change in 0..3 {
            let (e, mut pending, png) = fixture();
            let before = serde_json::to_value(&e.doc).unwrap();
            let history = e.undo.len();
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
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert_eq!(e.undo.len(), history);
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
                        engine.doc.layers[0].effects[0].id.clone()
                    };
                    app.select_effect(&app.selected.clone(), false, &effect);
                    workspace::draw(&mut app, &ctx, size, vec![]);
                    let output = workspace::draw(&mut app, &ctx, size, vec![]);
                    let before = app.shared.lock().unwrap().doc.clone();
                    let count = app.shared.lock().unwrap().undo.len();
                    let choice = workspace::label(&output, "Bitmap PNG");
                    workspace::click(&mut app, &ctx, size, choice);
                    let output = workspace::draw(&mut app, &ctx, size, vec![]);
                    let procedural = workspace::label(&output, "Checker");
                    workspace::click(&mut app, &ctx, size, procedural);
                    {
                        let engine = app.shared.lock().unwrap();
                        let settings = &engine.doc.layers[0].effects[0].settings;
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
                        assert_eq!(engine.undo.len(), count + 1);
                        assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
                        assert_ne!(
                            engine.doc.export_png().unwrap(),
                            before.export_png().unwrap()
                        );
                    }
                    let (tx, rx) = mpsc::channel();
                    let pending = {
                        let engine = app.shared.lock().unwrap();
                        let current = &engine.doc.layers[0].effects[0];
                        Pending {
                            project: engine.project_id.clone(),
                            document: engine.doc.id.clone(),
                            revision: engine.doc.revision,
                            layer: app.selected.clone(),
                            effect: effect.clone(),
                            kind: current.kind.clone(),
                            settings: current.settings.as_ref().clone(),
                            receiver: Arc::new(Mutex::new(rx)),
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
                    let history = app.shared.lock().unwrap().undo.len();
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
                    assert!(ctx.data(|data| data.get_temp::<Pending>(key()).is_none()));
                    if outcome == "ready" {
                        {
                            let engine = app.shared.lock().unwrap();
                            assert_eq!(
                                engine.doc.layers[0].effects[0].settings["pattern"],
                                "bitmap"
                            );
                            assert_eq!(
                                engine.doc.layers[0].effects[0].settings["bitmap_png"],
                                json!(new_png)
                            );
                            assert_eq!(engine.undo.len(), history + 1);
                            assert_eq!(engine.doc.bit_depth, depth);
                            assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
                        }
                        let imported = workspace::source(&app);
                        for _ in 0..3 {
                            workspace::draw(&mut app, &ctx, size, vec![]);
                        }
                        assert_eq!(workspace::source(&app), imported);
                        let mut engine = app.shared.lock().unwrap();
                        engine.undo("human").unwrap();
                        assert_eq!(
                            engine.doc.layers[0].effects[0].settings["pattern"],
                            "checker"
                        );
                        assert!(engine.doc.layers[0].effects[0]
                            .settings
                            .get("bitmap_png")
                            .is_none());
                        engine.undo("human").unwrap();
                        assert_eq!(
                            engine.doc.layers[0].effects[0].settings["bitmap_png"],
                            json!(old_png)
                        );
                        assert_eq!(
                            engine.doc.export_png().unwrap(),
                            before.export_png().unwrap()
                        );
                    } else {
                        assert_eq!(workspace::source(&app), retained, "{depth}/{outcome}");
                        assert_eq!(app.shared.lock().unwrap().undo.len(), history);
                    }
                }
            }
        }
    }
}
