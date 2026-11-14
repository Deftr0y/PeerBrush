//! Scalar presentation over immutable metadata; never an editable document.
use super::*;
use crate::engine::view::{EffectView, LayerView, MaskStepView};
#[cfg(test)]
mod tests;

#[derive(Default)]
pub(in crate::ui) struct Controls {
    before: Option<DocumentSnapshot>,
    t: f32,
}

impl Animation {
    pub(in crate::ui) fn controls<'a>(
        &self,
        doc: impl Into<DocumentView<'a>>,
        time: f64,
    ) -> Controls {
        let doc = doc.into();
        let Some(transition) = self
            .transition
            .as_ref()
            .filter(|transition| time.is_finite() && transition.after.view().same_observation(doc))
        else {
            return Controls::default();
        };
        let t = ease(((time - transition.started) / DURATION) as f32);
        Controls {
            // Snapshot clones share the existing admitted owners, including native sources.
            before: (t < 1.).then(|| transition.before.clone()),
            t,
        }
    }
}

impl Controls {
    fn layer(&self, id: &str) -> Option<LayerView<'_>> {
        self.before.as_ref()?.view().find_layer(id)
    }
    pub(in crate::ui) fn global_light(&self, angle: u32) -> Option<u32> {
        let old = self.before.as_ref()?.view().global_light_angle()?;
        (old != angle).then(|| {
            let delta = (angle as i32 - old as i32 + 180).rem_euclid(360) - 180;
            (old as i32 + (delta as f32 * self.t).round() as i32).rem_euclid(360) as u32
        })
    }
    pub(in crate::ui) fn opacity(&self, layer: LayerView<'_>) -> f32 {
        self.layer(layer.id()).map_or(layer.opacity(), |old| {
            lerp(old.opacity(), layer.opacity(), self.t)
        })
    }
    pub(in crate::ui) fn fill_opacity(&self, layer: LayerView<'_>) -> f32 {
        let fill = f32::from(layer.fill_opacity());
        self.layer(layer.id()).map_or(fill, |old| {
            lerp(f32::from(old.fill_opacity()), fill, self.t)
        })
    }
    pub(in crate::ui) fn blend_if(&self, layer: LayerView<'_>, channel: usize, endpoint: usize, value: u8) -> f32 {
        let Some(old) = self.layer(layer.id()) else { return f32::from(value) };
        let old = old.blend_if().unwrap_or([crate::psd::blend_ranges::DEFAULT; 4]);
        lerp(f32::from(old[channel][endpoint]), f32::from(value), self.t)
    }
    pub(in crate::ui) fn color(&self, layer: LayerView<'_>) -> [u8; 4] {
        self.layer(layer.id()).map_or(layer.color(), |old| {
            std::array::from_fn(|i| {
                lerp(old.color()[i] as f32, layer.color()[i] as f32, self.t).round() as u8
            })
        })
    }
    fn effect(&self, layer: Option<&str>, effect: EffectView<'_>) -> Option<EffectView<'_>> {
        let before = self.before.as_ref()?.view();
        match layer {
            Some(id) => {
                let layer = before.find_layer(id)?;
                (0..layer.effect_count())
                    .filter_map(|i| layer.effect(i))
                    .find(|old| old.id() == effect.id() && old.kind() == effect.kind())
            }
            None => (0..before.filter_count())
                .filter_map(|i| before.filter(i))
                .find(|old| old.id() == effect.id() && old.kind() == effect.kind()),
        }
    }
    pub(in crate::ui) fn effect_weight(&self, layer: Option<&str>, effect: EffectView<'_>) -> f32 {
        self.effect(layer, effect).map_or(effect.weight(), |old| {
            lerp(old.weight(), effect.weight(), self.t)
        })
    }
    pub(in crate::ui) fn effect_settings(
        &self,
        layer: Option<&str>,
        effect: EffectView<'_>,
        display: &mut Value,
    ) {
        if let Some(old) = self.effect(layer, effect) {
            // Only interpolate fields already selected by the inspector. Embedded bitmap
            // data and other omitted source payloads must not be copied into presentation.
            values(old.settings(), display, self.t);
        }
    }
    fn step(&self, layer: &str, step: MaskStepView<'_>) -> Option<MaskStepView<'_>> {
        let mask = self.layer(layer)?.mask()?;
        (0..mask.step_count())
            .filter_map(|i| mask.step(i))
            .find(|old| old.id() == step.id() && old.kind() == step.kind())
    }
    pub(in crate::ui) fn mask_weight(&self, layer: &str, step: MaskStepView<'_>) -> f32 {
        self.step(layer, step).map_or(step.weight(), |old| {
            lerp(old.weight(), step.weight(), self.t)
        })
    }
    pub(in crate::ui) fn mask_value(&self, layer: &str, step: MaskStepView<'_>) -> f32 {
        self.step(layer, step)
            .map_or(step.value(), |old| lerp(old.value(), step.value(), self.t))
    }
    pub(in crate::ui) fn mask_settings(
        &self,
        layer: &str,
        step: MaskStepView<'_>,
        display: &mut Value,
    ) {
        if let Some(old) = self.step(layer, step) {
            values(old.settings(), display, self.t);
        }
    }
    pub(in crate::ui) fn wand_number(
        &self,
        source: (&str, &Value),
        key: &str,
        default: f32,
    ) -> f32 {
        let current = source.1[key].as_f64().unwrap_or(default as f64) as f32;
        self.before
            .as_ref()
            .and_then(|doc| doc.view().wand_source())
            .filter(|old| old.0 == source.0)
            .map_or(current, |old| {
                lerp(
                    old.1[key].as_f64().unwrap_or(default as f64) as f32,
                    current,
                    self.t,
                )
            })
    }
}
