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
}
