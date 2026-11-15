//! Presentation-only animation. Shared edits and history are committed immediately.
use super::*;
use crate::engine::{AiChange, Scope};
use std::borrow::Cow;
mod controls;
mod native_clock;

const DURATION: f64 = 0.36;
const HIGHLIGHT: f64 = 3.0;

struct Transition {
    change: RuntimeValue<AiChange>,
    before: DocumentSnapshot,
    after: DocumentSnapshot,
    arriving_layers: HashSet<String>,
    started: f64,
    interpolated_canvas: bool,
    native: native_clock::Clock,
    native_failed: bool,
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
    observed: Option<DocumentSnapshot>,
    serial: u64,
    transition: Option<Transition>,
    fade: Option<Fade>,
    fade_serial: u64,
}
pub(super) enum Observation<'a> {
    Resident(&'a Document),
    Snapshot(&'a DocumentSnapshot),
}
impl<'a> From<&'a Document> for Observation<'a> {
    fn from(value: &'a Document) -> Self {
        Self::Resident(value)
    }
}
impl<'a> From<&'a DocumentSnapshot> for Observation<'a> {
    fn from(value: &'a DocumentSnapshot) -> Self {
        Self::Snapshot(value)
    }
}
impl Observation<'_> {
    fn view(&self) -> DocumentView<'_> {
        match self {
            Self::Resident(doc) => DocumentView::Resident(doc),
            Self::Snapshot(doc) => doc.view(),
        }
    }
    fn snapshot(&self) -> DocumentSnapshot {
        match self {
            Self::Resident(doc) => (*doc).clone().into(),
            Self::Snapshot(doc) => (*doc).clone(),
        }
    }
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
            let linked_shadow = b.contains_key("global_light");
            for (key, value) in b.iter_mut() {
                if matches!(key.as_str(), "preset" | "global_light") || linked_shadow && key == "angle" { continue; }
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
    pub(super) fn observe<'a>(
        &mut self,
        source: impl Into<Observation<'a>>,
        change: Option<RuntimeValue<AiChange>>,
        time: f64,
    ) -> bool {
        let source = source.into();
        let doc = source.view();
        let mut started = false;
        if self
            .observed
            .as_ref()
            .is_some_and(|d| d.view().id() != doc.id())
        {
            self.transition = None;
            self.fade = None;
            self.serial = change.as_ref().map_or(0, |c| c.serial);
        } else if let Some(change) = change {
            if change.serial != self.serial {
                self.serial = change.serial;
                if let Some(before) = self.observed.clone() {
                    let previous_layers: HashMap<_, _> = before.view().layers()
                        .map(|layer| (layer.id(), layer)).collect();
                    let compare_canvas = ["move", "opacity", "fill"].contains(&change.tool.as_str());
                    let mut arriving_layers = HashSet::new();
                    let mut interpolated_canvas = false;
                    for layer in doc.layers() {
                        if let Some(old) = previous_layers.get(layer.id()) {
                            interpolated_canvas = interpolated_canvas || compare_canvas
                                && (old.opacity() != layer.opacity() || old.origin() != layer.origin()
                                    || old.color() != layer.color());
                        } else {
                            arriving_layers.insert(layer.id().to_owned());
                        }
                    }
                    drop(previous_layers);
                    self.transition = Some(Transition {
                        change,
                        before,
                        after: source.snapshot(),
                        arriving_layers,
                        started: time,
                        interpolated_canvas,
                        native: native_clock::Clock::default(),
                        native_failed: false,
                    });
                    self.fade = None;
                    started = true;
                }
            } else if self
                .transition
                .as_ref()
                .is_some_and(|t| t.after.view().revision() != doc.revision())
            {
                // A later human edit takes precedence, including while a render is pending.
                self.cancel();
            }
        }
        if self
            .transition
            .as_ref()
            .is_some_and(|transition| !transition.after.view().same_observation(doc))
        {
            self.cancel();
        }
        self.observed = Some(source.snapshot());
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
    pub(super) fn clear(&mut self) {
        self.cancel();
        self.observed = None;
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
                    .any(|s| s.target.is_none() || s.target.as_deref() == Some(id))
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
        if self.transition.as_ref().is_some_and(|t| t.arriving_layers.contains(id)) {
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
    pub(super) fn native_presentation(
        &mut self,
        doc: &crate::engine::native::NativeDocument,
        time: f64,
    ) -> Result<Option<crate::engine::scene::presentation::Presentation>, String> {
        let Some(transition) = &mut self.transition else {
            return Ok(None);
        };
        if !transition.interpolated_canvas
            || transition.native_failed
            || !transition.after.view().same_observation(doc.into())
        {
            return Ok(None);
        }
        if !time.is_finite() {
            return Err("Native presentation time is invalid".into());
        }
        let t = ease(transition.native.fraction(time));
        if t >= 1. {
            return Ok(None);
        }
        let DocumentSnapshot::Native(before) = &transition.before else {
            return Ok(None);
        };
        let presentation =
            crate::engine::scene::presentation::Presentation::capture(before, doc, t, &|| Ok(()))?;
        Ok(Some(presentation))
    }
    pub(super) fn native_before(
        &self,
        document: &crate::engine::native::NativeDocument,
    ) -> Option<&crate::engine::native::NativeDocument> {
        let transition = self.transition.as_ref()?;
        if transition.interpolated_canvas
            || transition.native_failed
            || !transition.after.view().same_observation(document.into())
        {
            return None;
        }
        let DocumentSnapshot::Native(before) = &transition.before else {
            return None;
        };
        let a = before.metadata();
        let b = document.metadata();
        (a.width() == b.width() && a.height() == b.height() && a.depth() == b.depth())
            .then_some(before)
    }
    pub(super) fn native_blend(
        &mut self,
        document: &crate::engine::native::NativeDocument,
        time: f64,
    ) -> Result<Option<crate::render::native_preview::crossfade::Blend>, String> {
        if self.native_before(document).is_none() {
            return Ok(None);
        }
        if !time.is_finite() {
            return Err("Native presentation time is invalid".into());
        }
        let Some(transition) = self.transition.as_mut() else {
            return Ok(None);
        };
        let t = ease(transition.native.fraction(time));
        if t >= 1. {
            return Ok(None);
        }
        let DocumentSnapshot::Native(before) = &transition.before else {
            return Ok(None);
        };
        crate::render::native_preview::crossfade::Blend::capture(before, document, t, &|| Ok(()))
            .map(Some)
    }
    pub(super) fn stop_native_canvas(&mut self) {
        if let Some(transition) = &mut self.transition {
            transition.native_failed = true;
        }
    }
    pub(super) fn native_captured(
        &mut self,
        document: &crate::engine::native::NativeDocument,
        signature: [u8; 32],
    ) {
        if let Some(transition) = self
            .transition
            .as_mut()
            .filter(|transition| transition.after.view().same_observation(document.into()))
        {
            transition.native.captured(signature);
        }
    }
    pub(super) fn native_presented(
        &mut self,
        document: &crate::engine::native::NativeDocument,
        signature: [u8; 32],
        time: f64,
    ) {
        if let Some(transition) = self
            .transition
            .as_mut()
            .filter(|transition| transition.after.view().same_observation(document.into()))
        {
            transition.native.presented(signature, time);
        }
    }
    pub(super) fn document<'a>(
        &self,
        doc: &'a Document,
        time: f64,
        canvas: bool,
    ) -> Cow<'a, Document> {
        let Some(transition) = &self.transition else {
            return Cow::Borrowed(doc);
        };
        if canvas && !transition.interpolated_canvas {
            return Cow::Borrowed(doc);
        }
        let linear = ((time - transition.started) / DURATION).clamp(0.0, 1.0) as f32;
        // Match the render key exactly so a cached presentation never represents a different value.
        let t = ease(if canvas {
            (linear * 12.0).round() / 12.0
        } else {
            linear
        });
        if t >= 1.0
            || doc.id != transition.after.view().id()
            || doc.revision != transition.after.view().revision()
        {
            return Cow::Borrowed(doc);
        }
        let cache_tag = if canvas {
            format!("step{}", (linear * 12.0).round() as u8)
        } else {
            format!("ui{}", (time * 1000.0).round() as u64)
        };
        // This compatibility renderer accepts an actual resident source only;
        // native presentation uses its source-backed worker and metadata view.
        let DocumentSnapshot::Resident(before) = &transition.before else {
            return Cow::Borrowed(doc);
        };
        let mut display = doc.clone();
        if !canvas {
            if let (Some(old), Some(source)) = (
                crate::selection::wand_source(before),
                display.wand_source.as_mut(),
            ) {
                if old.id == source.id && source.revision == doc.revision {
                    let source = Arc::make_mut(source);
                    for (key, default) in [("tolerance", 24.), ("feather", 0.)] {
                        let a = old.command[key].as_f64().unwrap_or(default) as f32;
                        let b = source.command[key].as_f64().unwrap_or(default) as f32;
                        source.command[key] = json!(lerp(a, b, t));
                    }
                }
            }
            for filter in &mut display.filters {
                if let Some(old) = before
                    .filters
                    .iter()
                    .find(|f| f.id == filter.id && f.kind == filter.kind)
                {
                    filter.weight = lerp(old.weight, filter.weight, t);
                    values(&old.settings, Arc::make_mut(&mut filter.settings), t);
                }
            }
        }
        for l in &mut display.layers {
            let Some(old) = before.layers.iter().find(|old| old.id == l.id) else {
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
                        values(&old.settings, Arc::make_mut(&mut effect.settings), t);
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
        Cow::Owned(display)
    }
    pub(super) fn fade_preview(
        &mut self,
        previous: Option<&(u32, u32, Vec<u8>)>,
        preview: &Preview,
        time: f64,
    ) -> bool {
        // An accepted replacement can change sampling grids during a fade.
        // Its pixels must take precedence over the older fade endpoint.
        self.fade = None;
        let Some(t) = &self.transition else {
            return false;
        };
        if t.interpolated_canvas
            || preview.revision != t.after.view().revision()
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
    pub(super) fn interrupt_animation(&mut self) {
        if self.animation.fading() {
            // The canvas may still contain an intermediate blend. Schedule
            // the committed frame even if the document revision is unchanged.
            self.canvas_preview = None;
            self.last_preview = None;
        }
        self.animation.cancel();
    }

    pub(super) fn canvas_reservations(
        &self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        doc: DocumentView<'_>,
        projection: &view::Projection,
        ai_scopes: &[Scope],
        human_regions: &[[i32; 4]],
    ) {
        let clip = painter.clip_rect();
        if !clip.is_positive() {
            return;
        }
        let clip = clip.expand(2. + 2. / ui.ctx().pixels_per_point());
        let time = ui.input(|i| i.time);
        let mut scopes = self.animation.scopes(time);
        scopes.extend_from_slice(ai_scopes);
        scopes.sort_unstable_by(|a, b| (&a.target, a.rect).cmp(&(&b.target, b.rect)));
        scopes.dedup();
        let color = AI_BLUE.gamma_multiply(0.7 + 0.2 * (time as f32 * 4.0).sin());
        let mut pulse_visible = false;
        for scope in scopes {
            let bounds = scope.rect.or_else(|| {
                scope
                    .target
                    .as_ref()
                    .and_then(|id| doc.find_layer(id))
                    .map(|l| {
                        let [x, y] = l.origin();
                        [
                            x,
                            y,
                            x + l.pixels().width as i32,
                            y + l.pixels().height as i32,
                        ]
                    })
            });
            let Some(bounds) = bounds else { continue };
            pulse_visible |= paint_ai_brackets(painter, projection, bounds, clip, color);
        }
        for &bounds in human_regions {
            let corners = projection.region_corners(bounds);
            if (0..corners.len()).any(|i| {
                clip.intersects(Rect::from_two_pos(corners[i], corners[(i + 1) % corners.len()]))
            }) {
                painter.add(egui::Shape::closed_line(corners.to_vec(), Stroke::new(2.0, ACCENT)));
            }
        }
        if pulse_visible {
            ui.ctx().request_repaint_after(Duration::from_millis(30));
        }
    }
}

fn paint_ai_brackets(
    painter: &egui::Painter,
    projection: &view::Projection,
    bounds: [i32; 4],
    clip: Rect,
    color: Color32,
) -> bool {
    let [left, top, right, bottom] = bounds.map(i64::from);
    let length = ((right - left).min(bottom - top) as f32 * projection.scale()).clamp(2., 14.);
    let x = projection.vector(Vec2::new(length, 0.));
    let y = projection.vector(Vec2::new(0., length));
    let mut visible = false;
    for (point, (dx, dy)) in projection.region_corners(bounds).into_iter()
        .zip([(x, y), (-x, y), (-x, -y), (x, -y)])
    {
        for [a, b] in [[point + dx, point], [point, point + dy]] {
            if clip.intersects(Rect::from_two_pos(a, b)) {
                painter.line_segment([a, b], Stroke::new(1.5, color));
                visible = true;
            }
        }
    }
    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ai_brackets_keep_rotated_visible_arms_and_skip_offscreen_corners() {
        let ctx = egui::Context::default();
        let clip = Rect::from_min_size(Pos2::ZERO, Vec2::splat(10.));
        let view = view::View { angle: 90., mirror_h: true, mirror_v: false };
        let projection = view.projection(Rect::from_min_size(Pos2::ZERO, Vec2::splat(100.)), 1.);
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            let painter = ctx.layer_painter(egui::LayerId::background()).with_clip_rect(clip);
            assert!(!paint_ai_brackets(&painter, &projection, [-50, -50, 50, 50], clip, AI_BLUE));
            assert!(paint_ai_brackets(&painter, &projection, [4, -4, 40, 45], clip, AI_BLUE));
        });
        let arms = output.shapes.iter().filter_map(|clipped| match &clipped.shape {
            egui::Shape::LineSegment { points, stroke } if stroke.color == AI_BLUE => Some(*points),
            _ => None,
        });
        assert_eq!(arms.collect::<Vec<_>>(), [[egui::pos2(-4., 4.), egui::pos2(10., 4.)]]);
    }

    #[test]
    fn ai_document_filters_highlight_whole_image_and_interpolate_only_controls() {
        let mut e = crate::engine::Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(12, 8, 16).unwrap();
        e.edit(
            "human",
            &[json!({"op":"filter.add","kind":"posterize","settings":{"levels":3},"weight":1.0})],
            None,
            None,
            "Posterize",
        )
        .unwrap();
        let id = e.resident_state().expect("resident fixture").document.filters[0].id.clone();
        let mut a = Animation::default();
        a.observe(&e.resident_state().expect("resident fixture").document, None, 0.);
        e.edit(
            "agent",
            &[json!({"op":"filter.update","filter":id,"weight":0.2})],
            None,
            None,
            "Soften posterize",
        )
        .unwrap();
        let history = e.resident_state().expect("resident fixture").undo.len();
        let saved = e.resident_state().expect("resident fixture").document.export_png().unwrap();
        a.observe(&e.resident_state().expect("resident fixture").document, e.ai_change.clone(), 1.);
        assert!(a.layer(&e.resident_state().expect("resident fixture").document.layers[0].id, 1.18));
        assert_eq!(a.tool(1.18), Some("effects"));
        assert!((a.document(&e.resident_state().expect("resident fixture").document, 1.18, false).filters[0].weight - 0.6).abs() < 0.001);
        assert_eq!(a.document(&e.resident_state().expect("resident fixture").document, 1.18, true).filters[0].weight, 0.2);
        assert_eq!(e.resident_state().expect("resident fixture").document.export_png().unwrap(), saved);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history);
        a.cancel();
        assert!(!a.active(1.18));
        assert_eq!(e.resident_state().expect("resident fixture").document.filters[0].weight, 0.2);
    }
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
    fn ai_wand_parameters_animate_only_runtime_presentation_and_interrupt_cleanly() {
        let mut e = crate::engine::Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(16, 12, 16).unwrap();
        e.edit(
            "human",
            &[json!({"op":"selection","kind":"wand","point":[3,3],"tolerance":0})],
            None,
            None,
            "Seed",
        )
        .unwrap();
        let mut animation = Animation::default();
        animation.observe(&e.resident_state().expect("resident fixture").document, None, 0.);
        e.edit(
            "agent",
            &[json!({"op":"selection.wand_update","tolerance":200,"feather":8})],
            None,
            None,
            "Wand parameters",
        )
        .unwrap();
        let history = e.resident_state().expect("resident fixture").undo.len();
        let saved = crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap();
        animation.observe(&e.resident_state().expect("resident fixture").document, e.ai_change.clone(), 1.);
        let display = animation.document(&e.resident_state().expect("resident fixture").document, 1.18, false);
        let source = crate::selection::wand_source(&display).unwrap();
        assert!((source.command["tolerance"].as_f64().unwrap() - 100.).abs() < 0.01);
        assert!((source.command["feather"].as_f64().unwrap() - 4.).abs() < 0.01);
        let numbers = animation.controls(&e.resident_state().expect("resident fixture").document, 1.18);
        let current = DocumentView::from(&e.resident_state().expect("resident fixture").document).wand_source().unwrap();
        assert!((numbers.wand_number(current, "tolerance", 24.) - 100.).abs() < 0.01);
        assert!((numbers.wand_number(current, "feather", 0.) - 4.).abs() < 0.01);
        assert_eq!(
            numbers.wand_number(("another-click", current.1), "tolerance", 24.),
            200.
        );
        assert!(animation.layer("@selection", 1.18));
        assert_eq!(
            crate::selection::wand_source(&e.resident_state().expect("resident fixture").document).unwrap().command["tolerance"],
            200.
        );
        assert_eq!(
            animation.document(&e.resident_state().expect("resident fixture").document, 1.18, true).selection,
            e.resident_state().expect("resident fixture").document.selection
        );
        assert_eq!(crate::psd::encode(&e.resident_state().expect("resident fixture").document).unwrap(), saved);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history);
        animation.cancel();
        assert!(!animation.active(1.18));
        assert_eq!(
            crate::selection::wand_source(&animation.document(&e.resident_state().expect("resident fixture").document, 1.18, false))
                .unwrap()
                .command["tolerance"],
            200.
        );
    }
    #[test]
    fn ai_parameters_interpolate_only_the_presentation_and_human_edits_cancel() {
        let mut engine = crate::engine::Engine::new();
        let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"blur"})],
                None,
                None,
                "Blur",
            )
            .unwrap();
        let effect = engine.resident_state().expect("resident fixture").document.layers[0].effects[0].id.clone();
        let mut animation = Animation::default();
        animation.observe(&engine.resident_state().expect("resident fixture").document, None, 0.0);
        engine.edit("agent", &[
            json!({"op":"layer.update","layer":layer,"opacity":0.2}),
            json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"radius":24.0}}),
        ], None, None, "Soften").unwrap();
        animation.observe(&engine.resident_state().expect("resident fixture").document, engine.ai_change.clone(), 1.0);
        let display = animation.document(&engine.resident_state().expect("resident fixture").document, 1.18, false);
        assert!(matches!(display, Cow::Owned(_)));
        let mut other = Document::new(1, 1).unwrap();
        other.revision = engine.resident_state().expect("resident fixture").document.revision;
        assert!(matches!(
            animation.document(&other, 1.18, false),
            Cow::Borrowed(_)
        ));
        assert!((display.layers[0].opacity - 0.6).abs() < 0.001);
        assert!(
            (display.layers[0].effects[0].settings["radius"]
                .as_f64()
                .unwrap()
                - 16.0)
                .abs()
                < 0.01
        );
        assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].opacity, 0.2);
        assert_eq!(engine.resident_state().expect("resident fixture").undo.len(), 2);
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
        animation.observe(&engine.resident_state().expect("resident fixture").document, engine.ai_change.clone(), 1.2);
        assert!(matches!(
            animation.document(&engine.resident_state().expect("resident fixture").document, 1.2, false),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            animation.document(&engine.resident_state().expect("resident fixture").document, 1.2, false).layers[0].opacity,
            0.8
        );
        assert!(!animation.layer(&layer, 1.2));
    }
    #[test]
    fn intermediate_opacity_under_effected_group_cannot_poison_actual_render_cache() {
        let mut engine = crate::engine::Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new(32, 32).unwrap();
        let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
        engine.resident_state_mut().expect("resident fixture").document.layers[0].kind = "fill".into();
        engine.resident_state_mut().expect("resident fixture").document.layers[0].color = [200, 100, 50, 255];
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
        animation.observe(&engine.resident_state().expect("resident fixture").document, None, 0.0);
        engine
            .edit(
                "agent",
                &[json!({"op":"layer.update","layer":layer,"opacity":0.2})],
                None,
                None,
                "Opacity",
            )
            .unwrap();
        animation.observe(&engine.resident_state().expect("resident fixture").document, engine.ai_change.clone(), 1.0);
        let mut reference = engine.resident_state().expect("resident fixture").document.clone();
        for l in &mut reference.layers {
            l.effect_key = crate::engine::id();
        }
        let expected = reference.preview(None, 32, None, false).unwrap().2;
        let intermediate = animation
            .document(&engine.resident_state().expect("resident fixture").document, 1.18, true)
            .preview(None, 32, None, false)
            .unwrap()
            .2;
        let actual = engine.resident_state().expect("resident fixture").document.preview(None, 32, None, false).unwrap().2;
        assert_ne!(intermediate, expected);
        assert_eq!(actual, expected);
        assert_eq!(actual[3], 51);
    }
}
