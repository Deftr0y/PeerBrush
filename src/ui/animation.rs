//! Presentation-only animation. Shared edits and history are committed immediately.
use super::*;
use crate::engine::{AiChange, Scope};

const DURATION: f64 = 0.36;
const HIGHLIGHT: f64 = 3.0;

struct Transition {
    change: AiChange,
    before: Document,
    after: Document,
    started: f64,
    interpolated_canvas: bool,
}
struct Fade {
    from: Vec<u8>,
    to: Vec<u8>,
    width: u32,
    height: u32,
    started: f64,
    step: u8,
}
#[derive(Default)]
pub(super) struct Animation {
    observed: Option<Document>,
    serial: u64,
    transition: Option<Transition>,
    fade: Option<Fade>,
    fade_serial: u64,
}
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
fn values(before: &Value, after: &mut Value, t: f32) {
    match (before, after) {
        (Value::Number(a), Value::Number(b)) => {
            if let (Some(a), Some(target)) = (a.as_f64(), b.as_f64()) {
                if let Some(n) = serde_json::Number::from_f64(a + (target - a) * t as f64) {
                    *b = n;
                }
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in b.iter_mut() {
                if let Some(old) = a.get(key) {
                    values(old, value, t);
                }
            }
        }
        (Value::Array(a), Value::Array(b)) if a.len() == b.len() => {
            for (a, b) in a.iter().zip(b.iter_mut()) {
                values(a, b, t);
            }
        }
        _ => {}
    }
}
impl Animation {
    pub(super) fn observe(&mut self, doc: &Document, change: Option<AiChange>, time: f64) -> bool {
        let mut started = false;
        if self.observed.as_ref().is_some_and(|d| d.id != doc.id) {
            self.transition = None;
            self.fade = None;
            self.serial = change.as_ref().map_or(0, |c| c.serial);
        } else if let Some(change) = change {
            if change.serial != self.serial {
                self.serial = change.serial;
                if let Some(before) = self.observed.clone() {
                    let interpolated_canvas = ["move", "opacity", "fill"]
                        .contains(&change.tool.as_str())
                        && doc.layers.iter().any(|l| {
                            before
                                .layers
                                .iter()
                                .find(|old| old.id == l.id)
                                .is_some_and(|old| {
                                    old.opacity != l.opacity
                                        || old.x != l.x
                                        || old.y != l.y
                                        || old.color != l.color
                                })
                        });
                    self.transition = Some(Transition {
                        change,
                        before,
                        after: doc.clone(),
                        started: time,
                        interpolated_canvas,
                    });
                    self.fade = None;
                    started = true;
                }
            } else if self
                .transition
                .as_ref()
                .is_some_and(|t| t.after.revision != doc.revision)
            {
                // A later human edit takes precedence, including while a render is pending.
                self.cancel();
            }
        }
        self.observed = Some(doc.clone());
        if self
            .transition
            .as_ref()
            .is_some_and(|t| time - t.started >= HIGHLIGHT)
            && self.fade.is_none()
        {
            self.transition = None;
        }
        started
    }
    pub(super) fn cancel(&mut self) {
        self.transition = None;
        self.fade = None;
    }
    pub(super) fn progress(&self, time: f64) -> f32 {
        self.transition
            .as_ref()
            .map_or(1.0, |t| ease(((time - t.started) / DURATION) as f32))
    }
    pub(super) fn active(&self, time: f64) -> bool {
        self.transition
            .as_ref()
            .is_some_and(|t| time - t.started < HIGHLIGHT)
    }
    pub(super) fn layer(&self, id: &str, time: f64) -> bool {
        self.active(time)
            && self.transition.as_ref().is_some_and(|t| {
                t.change
                    .scopes
                    .iter()
                    .any(|s| s.target.as_deref() == Some(id))
            })
    }
    pub(super) fn tool(&self, time: f64) -> Option<&str> {
        self.transition
            .as_ref()
            .filter(|_| self.active(time))
            .map(|t| t.change.tool.as_str())
    }
    pub(super) fn scopes(&self, time: f64) -> Vec<Scope> {
        self.transition
            .as_ref()
            .filter(|_| self.active(time))
            .map_or_else(Vec::new, |t| t.change.scopes.clone())
    }
    pub(super) fn arrival(&self, id: &str, time: f64) -> f32 {
        if self.transition.as_ref().is_some_and(|t| {
            !t.before.layers.iter().any(|l| l.id == id) && t.after.layers.iter().any(|l| l.id == id)
        }) {
            self.progress(time)
        } else {
            1.0
        }
    }
    pub(super) fn render_key(&self, time: f64) -> String {
        self.transition
            .as_ref()
            .filter(|t| t.interpolated_canvas)
            .map_or_else(String::new, |t| {
                // At most 12 intermediate renders, plus the exact final state.
                let step = (((time - t.started) / DURATION).clamp(0.0, 1.0) * 12.0).round() as u8;
                format!("ai:{}:{step}", t.change.serial)
            })
    }
    pub(super) fn live_key(&self) -> String {
        self.transition
            .as_ref()
            .filter(|t| t.interpolated_canvas)
            .map_or_else(String::new, |t| format!("ai:{}", t.change.serial))
    }
    pub(super) fn document(&self, doc: &Document, time: f64, canvas: bool) -> Document {
        let Some(transition) = &self.transition else {
            return doc.clone();
        };
        if canvas && !transition.interpolated_canvas {
            return doc.clone();
        }
        let linear = ((time - transition.started) / DURATION).clamp(0.0, 1.0) as f32;
        // Match the render key exactly so a cached presentation never represents a different value.
        let t = ease(if canvas {
            (linear * 12.0).round() / 12.0
        } else {
            linear
        });
        if t >= 1.0 || doc.revision != transition.after.revision {
            return doc.clone();
        }
        let cache_tag = if canvas {
            format!("step{}", (linear * 12.0).round() as u8)
        } else {
            format!("ui{}", (time * 1000.0).round() as u64)
        };
        let mut display = doc.clone();
        for l in &mut display.layers {
            let Some(old) = transition.before.layers.iter().find(|old| old.id == l.id) else {
                continue;
            };
            // Effected ancestors are invalidated by the engine too. Keep every changed cache key
            // separate from authoritative state while rendering intermediate child opacity/moves.
            if old.effect_key != l.effect_key {
                l.effect_key = format!(
                    "{}:ai{}:{cache_tag}",
                    l.effect_key, transition.change.serial
                );
            }
            l.opacity = lerp(old.opacity, l.opacity, t);
            l.x = lerp(old.x as f32, l.x as f32, t).round() as i32;
            l.y = lerp(old.y as f32, l.y as f32, t).round() as i32;
            l.color = std::array::from_fn(|i| {
                lerp(old.color[i] as f32, l.color[i] as f32, t).round() as u8
            });
            if !canvas {
                for effect in &mut l.effects {
                    if let Some(old) = old
                        .effects
                        .iter()
                        .find(|e| e.id == effect.id && e.kind == effect.kind)
                    {
                        effect.weight = lerp(old.weight, effect.weight, t);
                        values(&old.settings, &mut effect.settings, t);
                    }
                }
                if let (Some(old), Some(mask)) = (&old.mask, &mut l.mask) {
                    if old.cache_key != mask.cache_key {
                        mask.cache_key = format!(
                            "{}:ai{}:{cache_tag}",
                            mask.cache_key, transition.change.serial
                        );
                    }
                    for step in &mut mask.steps {
                        if let Some(old) = old
                            .steps
                            .iter()
                            .find(|s| s.id == step.id && s.kind == step.kind)
                        {
                            step.weight = lerp(old.weight, step.weight, t);
                            step.value = lerp(old.value, step.value, t);
                            values(&old.settings, &mut step.settings, t);
                        }
                    }
                }
            }
        }
        display
    }
    pub(super) fn fade_preview(
        &mut self,
        previous: Option<&(u32, u32, Vec<u8>)>,
        preview: &Preview,
        time: f64,
    ) -> bool {
        let Some(t) = &self.transition else {
            return false;
        };
        if t.interpolated_canvas
            || preview.revision != t.after.revision
            || self.fade_serial == t.change.serial
        {
            return false;
        }
        self.fade_serial = t.change.serial;
        let Some((w, h, from)) = previous else {
            return false;
        };
        if (*w, *h) != (preview.w, preview.h) || from.len() != preview.bytes.len() {
            return false;
        }
        self.fade = Some(Fade {
            from: from.clone(),
            to: preview.bytes.clone(),
            width: *w,
            height: *h,
            started: time,
            step: u8::MAX,
        });
        true
    }
    pub(super) fn fade_pixels(&mut self, time: f64) -> Option<(u32, u32, Vec<u8>)> {
        let fade = self.fade.as_mut()?;
        let progress = ((time - fade.started) / DURATION).clamp(0.0, 1.0) as f32;
        let step = (progress * 12.0).round() as u8;
        if fade.step == step {
            return None;
        }
        fade.step = step;
        let bytes = if step == 12 {
            fade.to.clone()
        } else {
            crossfade(&fade.from, &fade.to, ease(progress))
        };
        let output = Some((fade.width, fade.height, bytes));
        if step == 12 {
            self.fade = None;
        }
        output
    }
    pub(super) fn fading(&self) -> bool {
        self.fade.is_some()
    }
}
fn crossfade(from: &[u8], to: &[u8], t: f32) -> Vec<u8> {
    let mut out = Vec::with_capacity(to.len());
    for (a, b) in from.chunks_exact(4).zip(to.chunks_exact(4)) {
        let alpha = lerp(a[3] as f32, b[3] as f32, t);
        for i in 0..3 {
            let value = if alpha > 0.0 {
                lerp(a[i] as f32 * a[3] as f32, b[i] as f32 * b[3] as f32, t) / alpha
            } else {
                0.0
            };
            out.push(value.round() as u8);
        }
        out.push(alpha.round() as u8);
    }
    out
}
impl PeerBrush {
    pub(super) fn ai_reserved(&self, id: &str) -> bool {
        let e = self.shared.lock().unwrap();
        e.leases
            .iter()
            .filter(|l| l.owner != "human" && l.expires > crate::engine::now())
            .any(|l| {
                l.scopes
                    .iter()
                    .any(|s| e.scope_overlap(s, &Scope::layer(id)))
            })
    }
    pub(super) fn ai_layer(&self, id: &str, time: f64) -> bool {
        self.ai_reserved(id) || self.animation.layer(id, time)
    }
    pub(super) fn ai_canvas(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        doc: &Document,
        rect: Rect,
        scale: f32,
    ) {
        let time = ui.input(|i| i.time);
        let mut scopes = self.animation.scopes(time);
        scopes.extend(
            self.shared
                .lock()
                .unwrap()
                .leases
                .iter()
                .filter(|l| l.owner != "human")
                .flat_map(|l| l.scopes.clone()),
        );
        scopes.sort_by_key(|s| (s.target.clone(), s.rect));
        scopes.dedup();
        if scopes.is_empty() {
            return;
        }
        for scope in scopes {
            let bounds = scope.rect.or_else(|| {
                scope
                    .target
                    .as_ref()
                    .and_then(|id| doc.layers.iter().find(|l| &l.id == id))
                    .map(|l| {
                        [
                            l.x,
                            l.y,
                            l.x + l.pixels.width as i32,
                            l.y + l.pixels.height as i32,
                        ]
                    })
            });
            let Some(bounds) = bounds else { continue };
            let r = Rect::from_min_max(
                rect.min + Vec2::new(bounds[0] as f32 * scale, bounds[1] as f32 * scale),
                rect.min + Vec2::new(bounds[2] as f32 * scale, bounds[3] as f32 * scale),
            );
            let color = AI_BLUE.gamma_multiply(0.7 + 0.2 * (time as f32 * 4.0).sin());
            let length = r.width().min(r.height()).min(14.0).max(2.0);
            for (p, x, y) in [
                (r.left_top(), 1.0, 1.0),
                (r.right_top(), -1.0, 1.0),
                (r.left_bottom(), 1.0, -1.0),
                (r.right_bottom(), -1.0, -1.0),
            ] {
                painter.line_segment(
                    [p + Vec2::new(length * x, 0.0), p],
                    Stroke::new(1.5_f32, color),
                );
                painter.line_segment(
                    [p, p + Vec2::new(0.0, length * y)],
                    Stroke::new(1.5_f32, color),
                );
            }
        }
        ui.ctx().request_repaint_after(Duration::from_millis(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fade_preserves_transparency_and_does_not_leave_old_pixels() {
        assert_eq!(
            crossfade(&[255, 0, 0, 255], &[0, 0, 255, 0], 0.5),
            [255, 0, 0, 128]
        );
        assert_eq!(
            crossfade(&[255, 0, 0, 255], &[0, 0, 255, 255], 0.5),
            [128, 0, 128, 255]
        );
        assert_eq!(
            crossfade(&[255, 0, 0, 255], &[0, 0, 0, 0], 1.0),
            [0, 0, 0, 0]
        );
    }
    #[test]
    fn ai_parameters_interpolate_only_the_presentation_and_human_edits_cancel() {
        let mut engine = crate::engine::Engine::new();
        let layer = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"blur"})],
                None,
                None,
                "Blur",
            )
            .unwrap();
        let effect = engine.doc.layers[0].effects[0].id.clone();
        let mut animation = Animation::default();
        animation.observe(&engine.doc, None, 0.0);
        engine.edit("agent", &[
            json!({"op":"layer.update","layer":layer,"opacity":0.2}),
            json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"radius":24.0}}),
        ], None, None, "Soften").unwrap();
        animation.observe(&engine.doc, engine.ai_change.clone(), 1.0);
        let display = animation.document(&engine.doc, 1.18, false);
        assert!((display.layers[0].opacity - 0.6).abs() < 0.001);
        assert!(
            (display.layers[0].effects[0].settings["radius"]
                .as_f64()
                .unwrap()
                - 16.0)
                .abs()
                < 0.01
        );
        assert_eq!(engine.doc.layers[0].opacity, 0.2);
        assert_eq!(engine.undo.len(), 2);
        assert!(animation.layer(&layer, 1.18));
        engine
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"opacity":0.8})],
                None,
                None,
                "My opacity",
            )
            .unwrap();
        animation.observe(&engine.doc, engine.ai_change.clone(), 1.2);
        assert_eq!(
            animation.document(&engine.doc, 1.2, false).layers[0].opacity,
            0.8
        );
        assert!(!animation.layer(&layer, 1.2));
    }
    #[test]
    fn intermediate_opacity_under_effected_group_cannot_poison_actual_render_cache() {
        let mut engine = crate::engine::Engine::new();
        engine.doc = Document::new(32, 32).unwrap();
        let layer = engine.doc.layers[0].id.clone();
        engine.doc.layers[0].kind = "fill".into();
        engine.doc.layers[0].color = [200, 100, 50, 255];
        let folder = engine
            .edit(
                "human",
                &[json!({"op":"layer.add","kind":"group"})],
                None,
                None,
                "Group",
            )
            .unwrap()["created"][0]
            .as_str()
            .unwrap()
            .to_string();
        engine.edit("human", &[
            json!({"op":"layer.parent","layer":layer,"parent":folder}),
            json!({"op":"effect.add","layer":folder,"kind":"adjust","settings":{"brightness":0.1,"contrast":1.0,"saturation":1.0}}),
        ], None, None, "Group effect").unwrap();
        let mut animation = Animation::default();
        animation.observe(&engine.doc, None, 0.0);
        engine
            .edit(
                "agent",
                &[json!({"op":"layer.update","layer":layer,"opacity":0.2})],
                None,
                None,
                "Opacity",
            )
            .unwrap();
        animation.observe(&engine.doc, engine.ai_change.clone(), 1.0);
        let mut reference = engine.doc.clone();
        for l in &mut reference.layers {
            l.effect_key = crate::engine::id();
        }
        let expected = reference.preview(None, 32, None, false).unwrap().2;
        let intermediate = animation
            .document(&engine.doc, 1.18, true)
            .preview(None, 32, None, false)
            .unwrap()
            .2;
        let actual = engine.doc.preview(None, 32, None, false).unwrap().2;
        assert_ne!(intermediate, expected);
        assert_eq!(actual, expected);
        assert_eq!(actual[3], 51);
    }
}
