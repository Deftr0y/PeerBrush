//! Fallible native composition over captured Source36 metadata. Providers own
//! prepared source/derived regions; sampling performs no backing-file I/O.
//! Editable definitions supplement the captured current raster. Reading that
//! raster never rerenders text/vector sources or resolves unavailable fonts.
use crate::{
    engine::{
        native::LayerMetadata,
        scene::{Document36, SceneSnapshot},
    },
    large_document::{Reservation, ResidentBudget},
    raster::{backed::Region, fill_blend::SpecialFill, surface::SurfaceRegion, Pixel, Pixel16},
    workspace::source_runtime::ReadChecks,
};
use std::collections::HashMap;
mod derived;
pub(crate) use derived::Layers;
mod filters;
pub(crate) use filters::prepare as prepare_filters;
pub(crate) use filters::prepare_with_layers as prepare_filters_with_layers;
pub(crate) use filters::prepare_with_checks as prepare_filters_with_checks;
mod stored;
pub(crate) use stored::{LayerWindow, Stored};

type Result<T> = std::result::Result<T, String>;

pub(crate) trait Precision {
    const DEPTH: u16;
    type Pixel: Copy;
    type Amount: Copy + Into<f64>;
    fn zero() -> Self::Pixel;
    fn fill(pixel: Pixel) -> Self::Pixel;
    fn opacity(value: f64) -> Self::Amount;
    fn multiply(a: Self::Amount, b: Self::Amount) -> Self::Amount;
    fn range_coverage(ranges: crate::psd::blend_ranges::Ranges, backdrop: Self::Pixel, source: Self::Pixel, scale: f64) -> f64;
    fn alpha(pixel: Self::Pixel) -> u16;
    fn scale_alpha(pixel: Self::Pixel, scale: f64) -> Self::Pixel;
    fn keep_channels(before: Self::Pixel, after: Self::Pixel, excluded: u8) -> Self::Pixel;
    fn keep_clipped_channels(before: Self::Pixel, after: Self::Pixel, excluded: u8, before_visible: f64, after_visible: f64) -> Self::Pixel;
    fn sample(view: &SurfaceRegion, x: i32, y: i32) -> Result<Self::Pixel>;
    fn colour_sample(
        view: &crate::effects::regional::RegionalImage,
        x: i32,
        y: i32,
    ) -> Result<Self::Pixel>;
    fn mask_sample(
        view: &crate::mask::region::RegionalMask,
        x: i32,
        y: i32,
    ) -> Result<Self::Amount>;
    fn opaque(pixel: Self::Pixel) -> Self::Pixel;
    fn alpha_from(pixel: Self::Pixel, source: Self::Pixel) -> Self::Pixel;
    fn compose(a: Self::Pixel, b: Self::Pixel, t: Self::Amount, mode: &str) -> Self::Pixel;
    fn compose_special(a: Self::Pixel, b: Self::Pixel, t: Self::Amount, fill: SpecialFill) -> Self::Pixel;
    fn mix(a: Self::Pixel, b: Self::Pixel, t: Self::Amount) -> Self::Pixel;
    fn adjust(a: Self::Pixel, b: Self::Pixel, t: Self::Amount, mode: &str) -> Self::Pixel;
}

pub(crate) struct Native8;
pub(crate) struct Native16;

#[cfg(test)]
mod derived_tests {
    use super::*;
    use crate::{
        effects::Effect,
        engine::{Document, Layer, Mask, MaskStep},
        raster::Raster,
    };
    use std::sync::Arc;
    struct Derived8<'a> {
        scene: &'a SceneSnapshot,
        doc: &'a Document,
        masks: Vec<Option<Arc<crate::mask::GrayMask>>>,
        colors: Vec<Option<Arc<crate::effects::Image>>>,
    }
    impl PreparedLayers<Native8> for Derived8<'_> {
        fn scene(&self) -> &SceneSnapshot {
            self.scene
        }
        fn has_colour(&self, i: usize) -> bool {
            self.colors[i].is_some()
        }
        fn colour(&self, i: usize, x: i32, y: i32) -> Result<Pixel> {
            let l = &self.doc.layers[i];
            let image = self.colors[i].as_ref().ok_or("Missing test image")?;
            Ok(if crate::effects::canvas_frame(l) {
                image.get(x, y)
            } else {
                image.get(x - l.x, y - l.y)
            })
        }
        fn source(&self, i: usize, x: i32, y: i32) -> Result<Pixel> {
            Ok(self.doc.layers[i].pixels.get(x, y))
        }
        fn mask(&self, i: usize, x: i32, y: i32, raw: bool) -> Result<f32> {
            Ok(self.doc.layers[i].mask_value_prepared(x, y, self.masks[i].as_deref(), raw))
        }
    }
    struct Derived16<'a> {
        scene: &'a SceneSnapshot,
        doc: &'a Document,
        masks: Vec<Option<Arc<crate::depth16::Gray16>>>,
        colors: Vec<Option<Arc<crate::depth16::Image16>>>,
    }
    impl PreparedLayers<Native16> for Derived16<'_> {
        fn scene(&self) -> &SceneSnapshot {
            self.scene
        }
        fn has_colour(&self, i: usize) -> bool {
            self.colors[i].is_some()
        }
        fn colour(&self, i: usize, x: i32, y: i32) -> Result<Pixel16> {
            let l = &self.doc.layers[i];
            let image = self.colors[i].as_ref().ok_or("Missing test image")?;
            Ok(if crate::effects::canvas_frame(l) {
                image.get(x, y)
            } else {
                image.get(x - l.x, y - l.y)
            })
        }
        fn source(&self, i: usize, x: i32, y: i32) -> Result<Pixel16> {
            Ok(self.doc.layers[i].pixels.get16(x, y))
        }
        fn mask(&self, i: usize, x: i32, y: i32, raw: bool) -> Result<f64> {
            Ok(crate::depth16::mask_value_prepared(
                &self.doc.layers[i],
                x,
                y,
                self.masks[i].as_deref(),
                raw,
            ))
        }
    }
    fn setup(depth: u16, pass: bool, clipped: bool, cached: bool) -> Document {
        let mut doc = tests::doc(depth, pass, "screen");
        if cached {
            crate::psd::blend_channels::update(&mut doc.layers.last_mut().unwrap().psd_metadata, &serde_json::json!({
                "blend_channels":[true,false,true]
            })).unwrap();
        }
        if pass && cached {
            if clipped {
                crate::psd::blend_channels::update(&mut doc.layers[1].psd_metadata, &serde_json::json!({
                    "blend_channels":[false,true,true]
                })).unwrap();
            } else {
                crate::psd::blend_ranges::update(&mut doc.layers[1].psd_metadata, &serde_json::json!({
                    "blend_if":[[0,0,200,230,30,50,255,255]]
                })).unwrap();
            }
        }
        if !pass {
            doc.layers[1].effects.push(Effect {
                id: crate::engine::id(),
                kind: "invert".into(),
                enabled: true,
                weight: 0.23,
                settings: serde_json::json!({}).into(),
            });
            doc.layers[1].effects.push(Effect {
                id: crate::engine::id(),
                kind: "stroke".into(),
                enabled: true,
                weight: 0.67,
                settings: serde_json::json!({}).into(),
            });
        }
        let base_id = doc.layers[3].id.clone();
        if clipped && cached {
            crate::psd::blend_ranges::update(&mut doc.layers[3].psd_metadata, &serde_json::json!({
                "blend_if":[[30,70,215,240,0,0,100,150]]
            })).unwrap();
        }
        let mut adjustment = Layer::new("Adjustment", "adjustment", 9, 7);
        adjustment.pixels = Raster::new_depth(9, 7, depth);
        adjustment.opacity = 0.31;
        adjustment.blend = if cached { "multiply" } else { "normal" }.into();
        adjustment.effects.push(Effect {
            id: crate::engine::id(),
            kind: "adjust".into(),
            enabled: true,
            weight: 0.67,
            settings: serde_json::json!({"brightness":0.15}).into(),
        });
        if cached {
            adjustment.effects.push(Effect {
                id: crate::engine::id(),
                kind: "blur".into(),
                enabled: true,
                weight: 0.57,
                settings: serde_json::json!({"radius":1.3}).into(),
            });
            if !pass {
                doc.layers[1].effects.push(Effect {
                    id: crate::engine::id(),
                    kind: "bloom".into(),
                    enabled: true,
                    weight: 0.63,
                    settings: serde_json::json!({"spread":1.7,"threshold":0.1}).into(),
                });
            }
        }
        if clipped {
            adjustment.clip_to = Some(base_id);
            if cached {
                crate::psd::blend_channels::update(&mut adjustment.psd_metadata, &serde_json::json!({
                    "blend_channels":[false,true,true]
                })).unwrap();
            }
        }
        if cached {
            crate::psd::blend_ranges::update(&mut adjustment.psd_metadata, &serde_json::json!({
                "blend_if":[[37,75,220,250,0,0,200,227]]
            })).unwrap();
        }
        doc.layers.insert(2, adjustment);
        let child = &mut doc.layers[0];
        child.effects.push(Effect {
            id: crate::engine::id(),
            kind: "invert".into(),
            enabled: true,
            weight: 0.43,
            settings: serde_json::json!({}).into(),
        });
        child.mask = Some(Mask {
            enabled: true,
            linked: true,
            cache_key: crate::engine::id(),
            steps: vec![MaskStep {
                id: crate::engine::id(),
                kind: "fill".into(),
                enabled: true,
                weight: 0.73,
                value: 113.4,
                pixels: Raster::new_depth(9, 7, depth),
                settings: serde_json::json!({}),
            }],
        });
        if cached {
            child.mask.as_mut().unwrap().steps.push(MaskStep {
                id: crate::engine::id(),
                kind: "blur".into(),
                enabled: true,
                weight: 1.0,
                value: 2.0,
                pixels: Raster::new_depth(9, 7, depth),
                settings: serde_json::json!({"radius":2.0}),
            });
        }
        doc
    }
    #[test]
    fn native_rgb8_multiply_preserves_the_admitted_rounding() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.layers[0].pixels.set(0, 0, [155, 161, 119, 255]);
        let mut multiply = Layer::new("Multiply", "paint", 1, 1);
        multiply.pixels.set(0, 0, [197, 197, 197, 255]);
        multiply.blend = "multiply".into();
        multiply.opacity = 89. / 255.;
        multiply.mask = Some(Mask {
            enabled: true,
            linked: true,
            cache_key: crate::engine::id(),
            steps: vec![MaskStep {
                id: crate::engine::id(),
                kind: "fill".into(),
                enabled: true,
                weight: 1.,
                value: 199.,
                pixels: Raster::new(1, 1),
                settings: serde_json::json!({}),
            }],
        });
        doc.layers.insert(0, multiply);
        let region = Region::new([0, 0, 1, 1]).unwrap();
        for (round_bytes, red) in [(false, 145), (true, 146)] {
            doc.psd_rgb8_multiply = round_bytes;
            let (scene, path) = tests::capture(&doc);
            {
                let topology = Topology::new(&scene).unwrap();
                let actual = Layers::for_topology(&topology, region, &mut || Ok(())).unwrap();
                let plan = Plan::<Native8, _>::new(&scene, &actual, region).unwrap();
                assert_eq!(scene.metadata().psd_rgb8_multiply(), round_bytes);
                assert_eq!(plan.pixel(0, 0).unwrap()[0], red);
            }
            drop(scene);
            tests::cleanup(&path);
        }
    }

    #[test]
    fn prepared_fractional_and_cached_masks_adjustments_and_pass_through_match_legacy() {
        let region = Region::new([-2, -1, 11, 9]).unwrap();
        for depth in [8, 16] {
            for pass in [false, true] {
                for clipped in [false, true] {
                    for cached in [false, true] {
                        let doc = setup(depth, pass, clipped, cached);
                        let (scene, path) = tests::capture(&doc);
                        assert!(RawLayers::prepare(&scene, region, &mut || Ok(())).is_err());
                        let topology = Topology::new(&scene).unwrap();
                        let actual =
                            Layers::for_topology(&topology, region, &mut || Ok(())).unwrap();
                        if depth == 8 {
                            let masks: Vec<_> = doc
                                .layers
                                .iter()
                                .map(|l| {
                                    l.mask
                                        .as_ref()
                                        .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
                                })
                                .collect();
                            let colors = crate::effects::prepare(&doc, &masks).unwrap();
                            let provider = Derived8 {
                                scene: &scene,
                                doc: &doc,
                                masks,
                                colors,
                            };
                            let legacy = crate::compositor::Plan::new(
                                &doc,
                                &provider.masks,
                                &provider.colors,
                            );
                            let plan = Plan::<Native8, _>::new(&scene, &provider, region).unwrap();
                            let native = Plan::<Native8, _>::new(&scene, &actual, region).unwrap();
                            for y in -1..9 {
                                for x in -2..11 {
                                    assert_eq!(
                                        plan.pixel(x, y).unwrap(),
                                        legacy.sample(legacy.group(None), x, y),
                                        "8 {pass} {clipped} {cached} {x},{y}"
                                    );
                                    assert_eq!(plan.layer(0, x, y).unwrap(), legacy.layer(0, x, y));
                                    assert_eq!(
                                        native.pixel(x, y).unwrap(),
                                        plan.pixel(x, y).unwrap()
                                    );
                                    for i in 0..doc.layers.len() {
                                        assert_eq!(
                                            native.layer(i, x, y).unwrap(),
                                            plan.layer(i, x, y).unwrap()
                                        );
                                    }
                                }
                            }
                        } else {
                            let masks = crate::depth16::prepare_masks(&doc).unwrap();
                            let colors = crate::depth16::prepare(&doc).unwrap();
                            let provider = Derived16 {
                                scene: &scene,
                                doc: &doc,
                                masks,
                                colors,
                            };
                            let legacy = crate::depth16::Plan16::with_prepared(
                                &doc,
                                &provider.masks,
                                &provider.colors,
                            );
                            let plan = Plan::<Native16, _>::new(&scene, &provider, region).unwrap();
                            let native = Plan::<Native16, _>::new(&scene, &actual, region).unwrap();
                            for y in -1..9 {
                                for x in -2..11 {
                                    assert_eq!(
                                        plan.pixel(x, y).unwrap(),
                                        legacy.pixel(x, y),
                                        "16 {pass} {clipped} {cached} {x},{y}"
                                    );
                                    assert_eq!(plan.layer(0, x, y).unwrap(), legacy.layer(0, x, y));
                                    assert_eq!(
                                        native.pixel(x, y).unwrap(),
                                        plan.pixel(x, y).unwrap()
                                    );
                                    for i in 0..doc.layers.len() {
                                        assert_eq!(
                                            native.layer(i, x, y).unwrap(),
                                            plan.layer(i, x, y).unwrap()
                                        );
                                    }
                                    assert_eq!(
                                        plan.adjustment_input(2, x, y).unwrap(),
                                        legacy.adjustment_input(2, x, y)
                                    );
                                }
                            }
                        }
                        drop(actual);
                        drop(topology);
                        drop(scene);
                        tests::cleanup(&path);
                    }
                }
            }
        }
    }
    #[test]
    fn disabled_native_masks_keep_their_exact_raw_save_values() {
        let region = Region::new([-2, -1, 11, 9]).unwrap();
        for depth in [8, 16] {
            let mut doc = setup(depth, false, false, false);
            doc.layers[0].mask.as_mut().unwrap().enabled = false;
            let (scene, path) = tests::capture(&doc);
            if depth == 8 {
                let masks: Vec<_> = doc
                    .layers
                    .iter()
                    .map(|l| {
                        l.mask
                            .as_ref()
                            .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
                    })
                    .collect();
                let colors = crate::effects::prepare(&doc, &masks).unwrap();
                let provider = Derived8 {
                    scene: &scene,
                    doc: &doc,
                    masks,
                    colors,
                };
                let plan = Plan::<Native8, _>::new(&scene, &provider, region).unwrap();
                assert_eq!(plan.mask(0, 1, 2, false).unwrap(), 1.0);
                let expected =
                    doc.layers[0].mask_value_prepared(3, 1, provider.masks[0].as_deref(), true);
                assert!(expected > 0.0 && expected < 1.0);
                assert_eq!(plan.mask(0, 1, 2, true).unwrap(), expected);
            } else {
                let masks = crate::depth16::prepare_masks(&doc).unwrap();
                let colors = crate::depth16::prepare(&doc).unwrap();
                let provider = Derived16 {
                    scene: &scene,
                    doc: &doc,
                    masks,
                    colors,
                };
                let plan = Plan::<Native16, _>::new(&scene, &provider, region).unwrap();
                assert_eq!(plan.mask(0, 1, 2, false).unwrap(), 1.0);
                let expected = crate::depth16::mask_value_prepared(
                    &doc.layers[0],
                    3,
                    1,
                    provider.masks[0].as_deref(),
                    true,
                );
                assert!(expected > 0.0 && expected < 1.0);
                assert_eq!(plan.mask(0, 1, 2, true).unwrap(), expected);
            }
            drop(scene);
            tests::cleanup(&path);
        }
    }
}
// Keep the established precision and operation order of each native renderer.
// In particular an eight-bit mask/opacity product must remain f32.
macro_rules! precision {
    ($name:ty, $pixel:ty, $word:ty, $float:ty, $depth:expr, $max:expr, $blend:path, $fill:expr, $sample:ident, $mask_sample:ident) => {
        impl Precision for $name {
            const DEPTH: u16 = $depth;
            type Pixel = $pixel;
            type Amount = $float;
            fn zero() -> Self::Pixel {
                [0; 4]
            }
            fn fill(pixel: Pixel) -> Self::Pixel {
                ($fill)(pixel)
            }
            fn opacity(value: f64) -> Self::Amount {
                value as $float
            }
            fn multiply(a: Self::Amount, b: Self::Amount) -> Self::Amount {
                a * b
            }
            fn range_coverage(ranges: crate::psd::blend_ranges::Ranges, backdrop: Self::Pixel, source: Self::Pixel, scale: f64) -> f64 {
                ranges.coverage_scaled(backdrop.map(u16::from), source.map(u16::from), $max, scale)
            }
            fn alpha(pixel: Self::Pixel) -> u16 {
                u16::from(pixel[3])
            }
            fn scale_alpha(mut pixel: Self::Pixel, scale: f64) -> Self::Pixel {
                pixel[3] = (f64::from(pixel[3]) * scale).round() as $word;
                pixel
            }
            fn keep_channels(before: Self::Pixel, after: Self::Pixel, excluded: u8) -> Self::Pixel {
                if excluded == 0 {
                    return after;
                }
                crate::psd::blend_channels::keep(before.map(u16::from), after.map(u16::from), excluded, $max)
                    .map(|sample| sample as $word)
            }
            fn keep_clipped_channels(before: Self::Pixel, after: Self::Pixel, excluded: u8, before_visible: f64, after_visible: f64) -> Self::Pixel {
                if excluded == 0 {
                    return after;
                }
                crate::psd::blend_channels::keep_clipped(before.map(u16::from), after.map(u16::from),
                    excluded, before_visible, after_visible).map(|sample| sample as $word)
            }
            fn sample(view: &SurfaceRegion, x: i32, y: i32) -> Result<Self::Pixel> {
                view.$sample(x, y).map_err(|e| e.to_string())
            }
            fn colour_sample(
                view: &crate::effects::regional::RegionalImage,
                x: i32,
                y: i32,
            ) -> Result<Self::Pixel> {
                view.$sample(x, y)
            }
            fn mask_sample(
                view: &crate::mask::region::RegionalMask,
                x: i32,
                y: i32,
            ) -> Result<Self::Amount> {
                view.$mask_sample(x, y)
            }
            fn opaque(mut pixel: Self::Pixel) -> Self::Pixel {
                pixel[3] = $max;
                pixel
            }
            fn alpha_from(mut pixel: Self::Pixel, source: Self::Pixel) -> Self::Pixel {
                pixel[3] = source[3];
                pixel
            }
            fn compose(a: Self::Pixel, b: Self::Pixel, t: Self::Amount, mode: &str) -> Self::Pixel {
                if b[3] == 0 || t == 0.0 {
                    return if a[3] == 0 { [0; 4] } else { a };
                }
                if t == 1.0 && ((mode == "normal" && b[3] == $max) || a[3] == 0) {
                    return b;
                }
                $blend(a, b, t, mode)
            }
            fn compose_special(a: Self::Pixel, b: Self::Pixel, t: Self::Amount, fill: SpecialFill) -> Self::Pixel {
                fill.compose(a.map(u16::from), b.map(u16::from), f64::from(t), $max).map(|v| v as $word)
            }
            fn mix(a: Self::Pixel, b: Self::Pixel, t: Self::Amount) -> Self::Pixel {
                let aa = a[3] as $float / $max as $float;
                let ba = b[3] as $float / $max as $float;
                let alpha = aa * (1. - t) + ba * t;
                if alpha <= 0. {
                    return [0; 4];
                }
                if a[3] == b[3] {
                    return Self::adjust(a, b, t, "normal");
                }
                let mut out = [0, 0, 0, (alpha * $max as $float).round() as $word];
                for c in 0..3 {
                    out[c] = ((a[c] as $float * aa * (1. - t) + b[c] as $float * ba * t) / alpha)
                        .round()
                        .clamp(0., $max as $float) as $word;
                }
                out
            }
            fn adjust(a: Self::Pixel, b: Self::Pixel, t: Self::Amount, mode: &str) -> Self::Pixel {
                if mode == "normal" {
                    let mut out = a;
                    for c in 0..3 {
                        out[c] = (a[c] as $float * (1. - t) + b[c] as $float * t)
                            .round()
                            .clamp(0., $max as $float) as $word;
                    }
                    return out;
                }
                Self::alpha_from($blend(Self::opaque(a), Self::opaque(b), t, mode), a)
            }
        }
    };
}
precision!(
    Native8,
    Pixel,
    u8,
    f32,
    8,
    255,
    crate::raster::blend,
    |p| p,
    pixel,
    value8
);
precision!(
    Native16,
    Pixel16,
    u16,
    f64,
    16,
    65535,
    crate::raster::blend16,
    |p: Pixel| p.map(|v| u16::from(v) * 257),
    pixel16,
    value16
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LayerScope<'a> {
    All,
    Layer(&'a str),
    Preview(&'a str),
    Visible(Option<&'a str>),
}

/// Implementations retain all pins, derived outputs and their shared charges.
/// Mask coordinates are layer-local. Colour coordinates are document-global.
/// Spatial style expansion and adjustment backdrops have already been prepared.
pub(crate) trait PreparedLayers<P: Precision> {
    fn scene(&self) -> &SceneSnapshot;
    /// Derived backdrops belong to the same tree and visibility scope as the final plan.
    fn scope(&self) -> LayerScope<'_> {
        LayerScope::All
    }
    fn has_colour(&self, layer: usize) -> bool;
    fn colour(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel>;
    fn source(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel>;
    fn mask(&self, layer: usize, x: i32, y: i32, raw: bool) -> Result<P::Amount>;
}

/// Real source-backed provider for the unstyled base stage. It refuses derived
/// work rather than falling back to an unprepared legacy full-image cache.
pub(crate) struct RawLayers<'a> {
    scene: &'a SceneSnapshot,
    scope: LayerScope<'a>,
    sources: Vec<Option<SurfaceRegion>>,
    masks: Vec<Option<crate::mask::region::RegionalMask>>,
    _index: Reservation,
}
impl<'a> RawLayers<'a> {
    pub(crate) fn prepare(
        scene: &'a SceneSnapshot,
        region: Region,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> Result<Self> {
        Self::prepare_inner(scene, None, region, false, &mut ReadChecks::caller_only(&mut || {
            check().map_err(|error| error.to_string())
        }))
    }
    pub(crate) fn for_topology(
        topology: &Topology<'a>,
        region: Region,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> Result<Self> {
        Self::prepare_inner(topology.scene, Some(topology), region, false, &mut ReadChecks::caller_only(&mut || {
            check().map_err(|error| error.to_string())
        }))
    }
    fn prepare_inner(
        scene: &'a SceneSnapshot,
        topology: Option<&Topology<'a>>,
        region: Region,
        preparing_effects: bool,
        checks: &mut ReadChecks<'_>,
    ) -> Result<Self> {
        checks.full()?;
        let layers = scene.metadata().layers();
        let bytes = layers
            .len()
            .checked_mul(512)
            .ok_or("Regional source index overflow")?;
        let charge = scene
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut sources = Vec::new();
        let mut masks = Vec::new();
        masks
            .try_reserve_exact(layers.len())
            .map_err(|_| "Regional mask index allocation refused")?;
        sources
            .try_reserve_exact(layers.len())
            .map_err(|_| "Regional source index allocation refused")?;
        let [l, t, r, b] = region.bounds();
        for (index, layer) in layers.enumerate() {
            checks.full()?;
            if topology.is_some_and(|topology| !topology.active[index]) {
                sources.push(None);
                masks.push(None);
                continue;
            }
            if !preparing_effects
                && layer
                    .effects()
                    .any(|effect| effect.enabled() && effect.weight() > 0.0)
            {
                return Err("Regional source stage requires prepared effects".into());
            }
            masks.push(if layer.mask().is_some() {
                Some(crate::mask::region::prepare_scene_with_checks(
                    scene,
                    layer,
                    region.bounds(),
                    None,
                    checks,
                )?)
            } else {
                None
            });
            let source = scene.raster(layer.pixels().slot())?;
            if !source.uses_budget(scene.budget()) || !source.uses_disk_budget(scene.disk_budget())
            {
                return Err("Regional source is outside captured global admission".into());
            }
            let left = (i64::from(l) - i64::from(layer.origin()[0])).max(0);
            let top = (i64::from(t) - i64::from(layer.origin()[1])).max(0);
            let right = (i64::from(r) - i64::from(layer.origin()[0]))
                .min(i64::from(layer.pixels().width()));
            let bottom = (i64::from(b) - i64::from(layer.origin()[1]))
                .min(i64::from(layer.pixels().height()));
            let view = if left < right
                && top < bottom
                && !["group", "fill", "adjustment"].contains(&layer.kind())
                && !(preparing_effects && layer.effects().any(|e| e.enabled() && e.weight() > 0.))
            {
                let region = Region::new([left as i32, top as i32, right as i32, bottom as i32])
                    .map_err(|e| e.to_string())?;
                Some(checks.surface_region(source, region)?)
            } else {
                None
            };
            sources.push(view);
        }
        checks.full()?;
        Ok(Self {
            scene,
            scope: topology.map_or(LayerScope::All, Topology::scope),
            sources,
            masks,
            _index: charge,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{
        engine::{
            scene::{PendingScene, RasterCollector, RasterRef36, RasterRoster, SceneIdentity},
            Document, Layer,
        },
        large_document::{DiskBudget, Limits, Store},
        raster::{Raster, Surface},
    };
    #[test]
    fn occupied_rows_skip_empty_bands_within_the_requested_columns() {
        let pool = crate::large_document::ResidentBudget::new(4096).unwrap();
        let mut occupied = crate::large_document::ScratchBytes::new(&pool, 9, 9).unwrap();
        occupied[7] = 1;
        let tiles = RasterTiles {
            size: [513, 601],
            columns: 3,
            occupied,
        };
        assert_eq!(tiles.first_occupied_row([250, 7, 260, 600]), Some(512));
        assert_eq!(tiles.first_occupied_row([250, 520, 260, 600]), Some(520));
        assert_eq!(tiles.first_occupied_row([250, 7, 260, 512]), None);
        assert_eq!(tiles.first_occupied_row([512, 7, 513, 600]), None);
    }

    struct Capture {
        store: Store,
        sources: Vec<Surface>,
    }
    impl RasterCollector for Capture {
        fn resident(&mut self, raster: &Raster) -> Result<RasterRef36> {
            assert!(raster.retained.is_none());
            let source = Surface::stage_raster(raster, self.store.clone(), &mut || Ok(()))
                .map_err(|e| e.to_string())?;
            self.sources.push(source);
            Ok(RasterRef36 {
                id: self.sources.len() as u32,
                width: raster.width,
                height: raster.height,
                depth: raster.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36> {
            unreachable!()
        }
    }
    pub(super) fn capture(doc: &Document) -> (SceneSnapshot, std::path::PathBuf) {
        capture_kind(doc, false)
    }
    pub(crate) fn capture_kind(
        doc: &Document,
        native: bool,
    ) -> (SceneSnapshot, std::path::PathBuf) {
        capture_kind_with_budget(doc, native, ResidentBudget::new(32 * 1024 * 1024).unwrap())
    }
    pub(crate) fn capture_kind_with_budget(
        doc: &Document,
        native: bool,
        budget: ResidentBudget,
    ) -> (SceneSnapshot, std::path::PathBuf) {
        let disk = DiskBudget::new(128 * 1024 * 1024, 16).unwrap();
        let path = std::fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!("pb-native-compose-{}", crate::engine::id()));
        let store = Store::create_with_disk_budget(
            &path,
            budget.clone(),
            disk.clone(),
            Limits {
                disk_bytes: 128 * 1024 * 1024,
                max_blocks: 256,
                max_resident_blocks: 64,
                manifest_bytes: 1024 * 1024,
            },
        )
        .unwrap();
        let mut capture = Capture {
            store,
            sources: vec![],
        };
        let owner =
            crate::psd::memory36::MetadataCharge::reserve(&budget, 8 * 1024 * 1024, &|| Ok(()))
                .unwrap();
        let metadata = Document36::capture(doc, &mut capture).unwrap();
        let metadata =
            crate::psd::memory36::Admitted::from_parts(std::sync::Arc::new(metadata), owner);
        let pending = PendingScene::new(
            metadata,
            RasterRoster::new(capture.sources).unwrap(),
            budget,
            disk,
        )
        .unwrap();
        let identity = SceneIdentity {
            project_id: "regional-test".into(),
            document_id: doc.id.clone(),
            revision: doc.revision,
        };
        let scene = if native {
            SceneSnapshot::from_native(
                identity,
                crate::engine::native::NativeDocument::from_pending(pending).unwrap(),
                vec![],
                None,
                None,
            )
        } else {
            SceneSnapshot::new(identity, pending, vec![], None, None)
        }
        .unwrap();
        (scene, path)
    }
    pub(crate) fn cleanup(path: &std::path::Path) {
        let owned = std::fs::canonicalize(path).unwrap();
        assert_eq!(
            owned.parent().unwrap(),
            std::fs::canonicalize(std::env::temp_dir()).unwrap()
        );
        assert!(owned
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("pb-native-compose-"));
        std::fs::remove_dir_all(owned).unwrap();
    }
    pub(crate) fn doc(depth: u16, pass: bool, blend: &str) -> Document {
        let mut doc = Document::new_depth(9, 7, depth).unwrap();
        let mut folder = Layer::new("Folder", "group", 9, 7);
        folder.pixels = Raster::new_depth(9, 7, depth);
        folder.opacity = 0.37;
        if pass {
            folder.blend = "pass_through".into();
        }
        let mut child = Layer::new("Child", "paint", 9, 7);
        child.pixels = Raster::new_depth(9, 7, depth);
        child.parent = Some(folder.id.clone());
        child.x = -2;
        child.y = 1;
        child.opacity = 0.63;
        let mut base = Layer::new("Base", "paint", 9, 7);
        base.pixels = Raster::new_depth(9, 7, depth);
        base.blend = blend.into();
        let mut clip = Layer::new("Clip", "paint", 9, 7);
        clip.pixels = Raster::new_depth(9, 7, depth);
        clip.clip_to = Some(base.id.clone());
        clip.opacity = 0.41;
        clip.blend = "multiply".into();
        let mut background = Layer::new("Background", "fill", 9, 7);
        background.pixels = Raster::new_depth(9, 7, depth);
        background.color = [31, 71, 153, 163];
        for (index, layer) in [&mut child, &mut base, &mut clip].into_iter().enumerate() {
            for y in 0..7 {
                for x in 0..9 {
                    if depth == 16 {
                        layer.pixels.set16(
                            x,
                            y,
                            [
                                12347 + x as u16 * 937,
                                33559 + y as u16 * 751,
                                51237,
                                if (x + y + index as i32) % 3 == 0 {
                                    0
                                } else {
                                    23459
                                },
                            ],
                        );
                    } else {
                        layer.pixels.set(
                            x,
                            y,
                            [
                                (x * 23 + 17) as u8,
                                (y * 31 + 7) as u8,
                                173,
                                if (x + y + index as i32) % 3 == 0 {
                                    0
                                } else {
                                    133
                                },
                            ],
                        );
                    }
                }
            }
        }
        doc.layers = vec![child, folder, clip, base, background];
        doc
    }
    #[test]
    fn real_native_sources_match_legacy_groups_clipping_and_every_blend() {
        let region = Region::new([-2, -1, 11, 9]).unwrap();
        for depth in [8, 16] {
            for pass in [false, true] {
                for &(blend, _) in crate::raster::BLENDS {
                    let doc = doc(depth, pass, blend);
                    let (scene, path) = capture(&doc);
                    let provider = RawLayers::prepare(&scene, region, &mut || Ok(())).unwrap();
                    if depth == 8 {
                        let masks = vec![None; doc.layers.len()];
                        let colors = vec![None; doc.layers.len()];
                        let legacy = crate::compositor::Plan::new(&doc, &masks, &colors);
                        let plan = Plan::<Native8, _>::new(&scene, &provider, region).unwrap();
                        for y in -1..9 {
                            for x in -2..11 {
                                assert_eq!(
                                    plan.pixel(x, y).unwrap(),
                                    legacy.sample(legacy.group(None), x, y),
                                    "{blend} {pass} {x},{y}"
                                );
                            }
                        }
                        assert!(plan.pixel(11, 0).is_err());
                    } else {
                        let legacy = crate::depth16::Plan16::new(&doc).unwrap();
                        let plan = Plan::<Native16, _>::new(&scene, &provider, region).unwrap();
                        for y in -1..9 {
                            for x in -2..11 {
                                assert_eq!(
                                    plan.pixel(x, y).unwrap(),
                                    legacy.pixel(x, y),
                                    "{blend} {pass} {x},{y}"
                                );
                            }
                        }
                        assert!(plan.pixel(11, 0).is_err());
                    }
                    drop(provider);
                    drop(scene);
                    cleanup(&path);
                }
            }
        }
    }
    #[test]
    fn actual_source_prepare_late_cancellation_and_scene_precision_mismatch_refuse() {
        let doc = doc(16, false, "normal");
        let (scene, path) = capture(&doc);
        let region = Region::new([0, 0, 9, 7]).unwrap();
        drop(RawLayers::prepare(&scene, region, &mut || Ok(())).unwrap());
        let baseline = scene.budget().used();
        let mut checks = 0;
        let complete = RawLayers::prepare(&scene, region, &mut || {
            checks += 1;
            Ok(())
        })
        .unwrap();
        drop(complete);
        assert_eq!(scene.budget().used(), baseline);
        let end = checks;
        checks = 0;
        assert!(RawLayers::prepare(&scene, region, &mut || {
            checks += 1;
            if checks == end {
                Err(crate::large_document::Error::Stale)
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(scene.budget().used(), baseline);
        let provider = RawLayers::prepare(&scene, region, &mut || Ok(())).unwrap();
        assert!(Plan::<Native8, _>::new(&scene, &provider, region).is_err());
        let (other, other_path) = capture(&doc);
        assert!(Plan::<Native16, _>::new(&other, &provider, region).is_err());
        drop(provider);
        drop(scene);
        drop(other);
        cleanup(&path);
        cleanup(&other_path);
    }
    #[test]
    fn raw_folder_and_adjustment_previews_keep_only_reachable_backdrops() {
        use serde_json::json;
        for depth in [8, 16] {
            let layer = |name, kind, value| {
                let mut layer = Layer::new(name, kind, 9, 7);
                layer.pixels = Raster::new_depth(9, 7, depth);
                if kind == "paint" {
                    layer.pixels.set16(3, 2, [value, 17327, 42019, 53171]);
                }
                if kind == "adjustment" {
                    layer.effects.push(crate::effects::Effect {
                        id: crate::engine::id(), kind: "invert".into(), enabled: true, weight: 0.61,
                        settings: json!({}).into(),
                    });
                }
                layer
            };
            for (blend, ranged) in [("pass_through", true), ("normal", false)] {
                let mut doc = Document::new_depth(9, 7, depth).unwrap();
                let mut folder = layer("Folder", "group", 0);
                folder.blend = blend.into();
                let mut adjustment = layer("Folder adjustment", "adjustment", 0);
                adjustment.parent = Some(folder.id.clone());
                let mut child = layer("Folder paint", "paint", 31951);
                child.parent = Some(folder.id.clone());
                let mut hidden = layer("Hidden child", "paint", 41931);
                hidden.parent = Some(folder.id.clone());
                hidden.visible = false;
                let mut base = layer("Hidden base", "paint", 49153);
                base.visible = false;
                if ranged {
                    crate::psd::blend_ranges::update(&mut base.psd_metadata, &json!({
                        "blend_if":[[0,0,200,230,30,50,255,255]]
                    })).unwrap();
                }
                let mut clipped = layer("Clipped adjustment", "adjustment", 0);
                clipped.clip_to = Some(base.id.clone());
                let mut follower = layer("Lower clip", "paint", 62117);
                follower.clip_to = Some(base.id.clone());
                doc.layers = vec![layer("Above the targets", "paint", 12347), folder, adjustment,
                    child, hidden, clipped, follower, base, layer("Outer backdrop", "paint", 52691)];

                let (scene, path) = capture_kind(&doc, true);
                let region = Region::new([0, 0, 9, 7]).unwrap();
                let full = Topology::new(&scene).unwrap();
                let complete = LayerWindow::prepare(&full, region, None, &mut || Ok(())).unwrap();
                let through = blend == "pass_through";
                for (index, expected) in [
                    (1, if through { &[1, 2, 3, 8][..] } else { &[1, 2, 3] }),
                    (2, if through { &[2, 3, 8][..] } else { &[2, 3] }),
                    (5, if ranged { &[5, 6, 7, 8][..] } else { &[5, 6, 7] }),
                ] {
                    let scoped = Topology::for_preview(&scene, Some(index)).unwrap();
                    let active: Vec<_> = scoped.active.iter().enumerate()
                        .filter_map(|(i, &active)| active.then_some(i)).collect();
                    assert_eq!(active, expected, "{blend} target {index}");
                    let window = LayerWindow::prepare(&scoped, region, None, &mut || Ok(())).unwrap();
                    if depth == 8 {
                        let actual = Plan::<Native8, _>::with_topology(scoped, &window, region).unwrap();
                        let expected = Plan::<Native8, _>::with_topology(full.clone(), &complete, region).unwrap();
                        for (x, y) in [(3, 2), (8, 6)] {
                            assert_eq!(actual.layer(index, x, y).unwrap(), expected.layer(index, x, y).unwrap());
                        }
                        assert!(actual.layer(0, 3, 2).is_err());
                    } else {
                        let actual = Plan::<Native16, _>::with_topology(scoped, &window, region).unwrap();
                        let expected = Plan::<Native16, _>::with_topology(full.clone(), &complete, region).unwrap();
                        for (x, y) in [(3, 2), (8, 6)] {
                            assert_eq!(actual.layer(index, x, y).unwrap(), expected.layer(index, x, y).unwrap());
                        }
                        assert!(actual.layer(0, 3, 2).is_err());
                    }
                }
                drop(complete);
                drop(full);
                drop(scene);
                cleanup(&path);
            }
        }
    }

    #[test]
    fn one_native_topology_reuses_its_admission_across_regions_and_rejects_foreign_scene() {
        for depth in [8, 16] {
            let doc = doc(depth, true, "multiply");
            let (scene, path) = capture_kind(&doc, true);
            let full = Region::new([0, 0, 9, 7]).unwrap();
            drop(RawLayers::prepare(&scene, full, &mut || Ok(())).unwrap());
            let baseline = scene.budget().used();
            let topology = Topology::new(&scene).unwrap();
            let charge = topology._topology.bytes();
            assert_eq!(scene.budget().used(), baseline + charge);
            for rect in [[0, 0, 3, 4], [3, 2, 8, 6], [8, 6, 9, 7]] {
                let region = Region::new(rect).unwrap();
                let provider = RawLayers::prepare(&scene, region, &mut || Ok(())).unwrap();
                let pinned = scene.budget().used();
                if depth == 8 {
                    let plan =
                        Plan::<Native8, _>::with_topology(topology.clone(), &provider, region)
                            .unwrap();
                    assert_eq!(scene.budget().used(), pinned);
                    let masks = vec![None; doc.layers.len()];
                    let colors = vec![None; doc.layers.len()];
                    let expected = crate::compositor::Plan::new(&doc, &masks, &colors);
                    for y in rect[1]..rect[3] {
                        for x in rect[0]..rect[2] {
                            assert_eq!(
                                plan.pixel(x, y).unwrap(),
                                expected.sample(expected.group(None), x, y)
                            );
                        }
                    }
                } else {
                    let plan =
                        Plan::<Native16, _>::with_topology(topology.clone(), &provider, region)
                            .unwrap();
                    assert_eq!(scene.budget().used(), pinned);
                    let expected = crate::depth16::Plan16::new(&doc).unwrap();
                    for y in rect[1]..rect[3] {
                        for x in rect[0]..rect[2] {
                            assert_eq!(plan.pixel(x, y).unwrap(), expected.pixel(x, y));
                        }
                    }
                }
                assert_eq!(std::sync::Arc::strong_count(&topology), 1);
                drop(provider);
                assert_eq!(scene.budget().used(), baseline + charge);
            }
            let (other, other_path) = capture_kind(&doc, true);
            let wrong = RawLayers::prepare(&other, full, &mut || Ok(())).unwrap();
            assert!(Plan::<Native8, _>::with_topology(topology.clone(), &wrong, full).is_err());
            drop(wrong);
            drop(other);
            cleanup(&other_path);
            drop(topology);
            assert_eq!(scene.budget().used(), baseline);
            drop(scene);
            cleanup(&path);
        }
    }
}
impl<P: Precision> PreparedLayers<P> for RawLayers<'_> {
    fn scene(&self) -> &SceneSnapshot {
        self.scene
    }
    fn scope(&self) -> LayerScope<'_> {
        self.scope
    }
    fn has_colour(&self, _: usize) -> bool {
        false
    }
    fn colour(&self, _: usize, _: i32, _: i32) -> Result<P::Pixel> {
        Err("No derived region was prepared".into())
    }
    fn source(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel> {
        let view = self
            .sources
            .get(layer)
            .and_then(Option::as_ref)
            .ok_or("No native source footprint was prepared")?;
        P::sample(view, x, y)
    }
    fn mask(&self, layer: usize, x: i32, y: i32, _: bool) -> Result<P::Amount> {
        let mask = self
            .masks
            .get(layer)
            .and_then(Option::as_ref)
            .ok_or("No native mask region was prepared")?;
        P::mask_sample(mask, x, y)
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Raster,
    Fill,
    Group(usize),
    Adjustment,
}

/// A conservative map of document tiles that may contain rendered color.
pub(crate) struct RasterTiles {
    size: [u32; 2],
    columns: u32,
    occupied: crate::large_document::ScratchBytes,
}
impl RasterTiles {
    pub(crate) fn intersects(&self, bounds: [i32; 4]) -> bool {
        use crate::raster::TILE;
        let [l, t, r, b] = [
            bounds[0].clamp(0, self.size[0] as i32),
            bounds[1].clamp(0, self.size[1] as i32),
            bounds[2].clamp(0, self.size[0] as i32),
            bounds[3].clamp(0, self.size[1] as i32),
        ].map(|n| n as u32);
        l < r && t < b && (t / TILE..b.div_ceil(TILE)).any(|y| {
            (l / TILE..r.div_ceil(TILE)).any(|x| self.occupied[(y * self.columns + x) as usize] != 0)
        })
    }
    /// First row that can contain color in the requested document columns.
    pub(crate) fn first_occupied_row(&self, [left, top, right, bottom]: [i32; 4]) -> Option<i32> {
        let edge = crate::raster::TILE as i32;
        let mut top = top.max(0);
        let bottom = bottom.min(self.size[1] as i32);
        while top < bottom {
            let end = ((top.div_euclid(edge) + 1) * edge).min(bottom);
            if self.intersects([left, top, right, end]) {
                return Some(top);
            }
            top = end;
        }
        None
    }
    pub(crate) fn rects(&self) -> impl Iterator<Item = [i32; 4]> + '_ {
        use crate::raster::TILE;
        self.occupied.iter().enumerate().filter_map(|(i, &marked)| {
            if marked == 0 {
                return None;
            }
            let x = i as u32 % self.columns * TILE;
            let y = i as u32 / self.columns * TILE;
            Some([
                x as i32,
                y as i32,
                (x + TILE).min(self.size[0]) as i32,
                (y + TILE).min(self.size[1]) as i32,
            ])
        })
    }
    /// Consecutive occupied columns within a half-open document rectangle.
    pub(crate) fn column_spans(&self, bounds: [i32; 4]) -> impl Iterator<Item = [i32; 2]> + '_ {
        let [left, top, right, bottom] = bounds;
        let mut x = left;
        let tile = crate::raster::TILE as i32;
        let next = move |x: i32| ((x.div_euclid(tile) + 1) * tile).min(right);
        std::iter::from_fn(move || {
            while x < right && !self.intersects([x, top, next(x), bottom]) {
                x = next(x);
            }
            let start = x;
            while x < right && self.intersects([x, top, next(x), bottom]) {
                x = next(x);
            }
            (start < x).then_some([start, x])
        })
    }
}

/// Immutable metadata/topology is compiled once per captured frame or save.
/// Regional providers keep only the pins for the current bounded footprint.
pub(crate) struct Topology<'a> {
    scene: &'a SceneSnapshot,
    layers: Vec<LayerMetadata<'a>>,
    active: Vec<bool>,
    root: Option<usize>,
    preview_root: Option<usize>,
    visible_only: bool,
    groups: Vec<Vec<usize>>,
    parent_group: Vec<usize>,
    group_owner: Vec<Option<usize>>,
    kinds: Vec<Kind>,
    clips: Vec<Vec<usize>>,
    clip_base: Vec<Option<usize>>,
    opacity: Vec<(f64, Option<SpecialFill>, Option<crate::psd::blend_ranges::Ranges>, u8)>,
    _topology: Reservation,
}
impl<'a> Topology<'a> {
    pub(crate) fn scene(&self) -> &'a SceneSnapshot {
        self.scene
    }
    pub(crate) fn new(scene: &'a SceneSnapshot) -> Result<std::sync::Arc<Self>> {
        Self::for_target(scene, None)
    }
    pub(crate) fn target(&self) -> Option<&'a str> {
        self.root.map(|root| self.layers[root].id())
    }
    fn scope(&self) -> LayerScope<'a> {
        if let Some(root) = self.preview_root {
            LayerScope::Preview(self.layers[root].id())
        } else if self.visible_only {
            LayerScope::Visible(self.target())
        } else {
            self.target().map_or(LayerScope::All, LayerScope::Layer)
        }
    }
    fn passes_backdrop(&self, index: usize) -> bool {
        super::passes_backdrop(self.layers[index].blend(), self.opacity[index].2.is_some())
    }
    /// Occupied document tiles conservatively bound the composite's nonzero
    /// RGBA, including finite effect halos, within the requested crop's tiles.
    /// The renderer resolves exact samples.
    pub(crate) fn composite_tiles(
        &self,
        region: Option<[i32; 4]>,
        check: &mut dyn FnMut() -> Result<()>,
    ) -> Result<Option<RasterTiles>> {
        use crate::{large_document::ScratchBytes, raster::TILE};
        let doc = self.scene.metadata();
        if doc.read_only()
            || self.layers.iter().zip(&self.active).any(|(layer, active)| {
                *active && layer.kind() == "fill"
            })
        {
            return Ok(None);
        }
        let depth = doc.depth();
        let mut shared_halo = 0i64;
        if self.root.is_none() && doc.filters().any(|f| f.enabled() && f.weight() > 0.) {
            if doc.filters().len() > crate::filters::LIMIT {
                return Err("Whole-image filter stack limit: 32".into());
            }
            let bytes = 64 * 1024;
            let _scratch = self.scene.budget().reserve_bytes(bytes, bytes).map_err(|e| e.to_string())?;
            for (index, filter) in doc.filters().enumerate() {
                check()?;
                if filter.id().is_empty() || doc.filters().take(index).any(|old| old.id() == filter.id()) {
                    return Err("Invalid or duplicate whole-image filter ID".into());
                }
                crate::effects::validate_weight(filter.weight())?;
                drop(crate::filters::settings(filter.kind(), filter.settings())?);
            }
            let Some(halo) = sparse_halo(doc.filters(), depth, check)? else { return Ok(None) };
            shared_halo += halo;
        }
        // An adjustment can reach earlier backdrops, including through a
        // pass-through folder. Summing all non-raster halos bounds those paths.
        for (i, layer) in self.layers.iter().enumerate().filter(|(i, _)| self.active[*i]) {
            if !matches!(self.kinds[i], Kind::Raster) {
                let Some(halo) = sparse_halo(layer.effects(), depth, check)? else { return Ok(None) };
                shared_halo += halo;
            }
        }
        let size = [doc.width(), doc.height()];
        let columns = size[0].div_ceil(TILE);
        let count = (columns * size[1].div_ceil(TILE)) as usize;
        let mut occupied = ScratchBytes::new(self.scene.budget(), count, count)
            .map_err(|e| e.to_string())?;
        let [left, top, right, bottom] = region.unwrap_or([0, 0, size[0] as i32, size[1] as i32]);
        // Retain the same occupied cells as a whole-document scan within the crop.
        let align_start = |edge: i32, size: u32| edge.clamp(0, size as i32) as u32 / TILE * TILE;
        let align_end = |edge: i32, size: u32| {
            ((edge.clamp(0, size as i32) as u32).div_ceil(TILE) * TILE).min(size)
        };
        let start = [align_start(left, size[0]), align_start(top, size[1])];
        let end = [align_end(right, size[0]), align_end(bottom, size[1])];
        if start[0] >= end[0] || start[1] >= end[1] {
            return Err("Invalid composite tile coverage region".into());
        }
        for (i, layer) in self.layers.iter().enumerate().filter(|(i, _)| self.active[*i]) {
            check()?;
            // Masks change coverage. Their paint can extend beyond the color
            // support, so validate the mask without adding its tiles to the map.
            crate::mask::region::validate_scene_sources(self.scene, *layer, check)?;
            let source = self.scene.raster(layer.pixels().slot())?;
            if !source.uses_budget(self.scene.budget())
                || !source.uses_disk_budget(self.scene.disk_budget())
            {
                return Err("Regional source is outside captured global admission".into());
            }
            if !matches!(self.kinds[i], Kind::Raster) {
                continue;
            }
            let Some(halo) = sparse_halo(layer.effects(), depth, check)? else { return Ok(None) };
            let halo = halo + shared_halo;
            let origin = layer.origin();
            let source_size = [source.width(), source.height()];
            let local = |point: [u32; 2], offset: i64| {
                std::array::from_fn::<_, 2, _>(|axis| {
                    (i64::from(point[axis]) - i64::from(origin[axis]) + offset)
                        .clamp(0, i64::from(source_size[axis])) as u32
                })
            };
            let [l, t] = local(start, -halo);
            let [r, b] = local(end, halo);
            if l >= r || t >= b {
                continue;
            }
            let xs = l / TILE..r.div_ceil(TILE);
            let ys = t / TILE..b.div_ceil(TILE);
            let mut mark = |(x, y): (u32, u32)| {
                let tile_start = [x * TILE, y * TILE];
                let tile_end = std::array::from_fn(|axis| (tile_start[axis] + TILE).min(source_size[axis]));
                let clip = |point: [u32; 2], offset: i64| {
                    std::array::from_fn::<_, 2, _>(|axis| {
                        (i64::from(point[axis]) + i64::from(origin[axis]) + offset)
                            .clamp(i64::from(start[axis]), i64::from(end[axis])) as u32
                    })
                };
                let [l, t] = clip(tile_start, -halo);
                let [r, b] = clip(tile_end, halo);
                if l >= r || t >= b {
                    return;
                }
                for row in t / TILE..b.div_ceil(TILE) {
                    for column in l / TILE..r.div_ceil(TILE) {
                        occupied[(row * columns + column) as usize] = 1;
                    }
                }
            };
            let physical = source.physical_tiles();
            if xs.len() * ys.len() < physical.len() {
                for x in xs {
                    for y in ys.clone() {
                        check()?;
                        if source.physical_tile((x, y)).is_some() {
                            mark((x, y));
                        }
                    }
                }
            } else {
                for tile in physical {
                    check()?;
                    mark(tile.coordinate());
                }
            }
        }
        Ok(Some(RasterTiles { size, columns, occupied }))
    }
    pub(crate) fn for_target(
        scene: &'a SceneSnapshot,
        target: Option<&str>,
    ) -> Result<std::sync::Arc<Self>> {
        Self::build(scene, target, false)
    }

    pub(crate) fn for_composite(
        scene: &'a SceneSnapshot,
        target: Option<&str>,
    ) -> Result<std::sync::Arc<Self>> {
        let mut topology = Self::for_target(scene, target)?;
        let tree = std::sync::Arc::get_mut(&mut topology).ok_or("Composite topology is already shared")?;
        // Follow the exact group and clipping lists used by Plan::below and Plan::unit.
        fn visit(i: usize, kinds: &[Kind], groups: &[Vec<usize>], clips: &[Vec<usize>], active: &mut [bool]) {
            if std::mem::replace(&mut active[i], true) {
                return;
            }
            if let Kind::Group(group) = kinds[i] {
                for &child in &groups[group] {
                    visit(child, kinds, groups, clips, active);
                }
            }
            for &follower in &clips[i] {
                visit(follower, kinds, groups, clips, active);
            }
        }
        tree.active.fill(false);
        for &i in &tree.groups[0] {
            visit(i, &tree.kinds, &tree.groups, &tree.clips, &mut tree.active);
        }
        tree.visible_only = true;
        Ok(topology)
    }

    fn preview_scope(scene: &'a SceneSnapshot, target: Option<usize>) -> Result<LayerScope<'a>> {
        let Some(index) = target else { return Ok(LayerScope::Visible(None)); };
        let doc = scene.metadata();
        let layer = doc.layer(index).ok_or("Preview layer is missing")?;
        Ok(if doc.read_only() {
            LayerScope::All
        } else if matches!(layer.kind(), "group" | "adjustment") {
            LayerScope::Preview(layer.id())
        } else {
            LayerScope::Layer(layer.id())
        })
    }

    pub(crate) fn check_preview(&self, scene: &'a SceneSnapshot, target: Option<usize>) -> Result<()> {
        if !std::ptr::eq(self.scene, scene) || self.scope() != Self::preview_scope(scene, target)? {
            return Err("Composition topology differs from the captured preview".into());
        }
        Ok(())
    }

    pub(crate) fn for_preview(
        scene: &'a SceneSnapshot,
        target: Option<usize>,
    ) -> Result<std::sync::Arc<Self>> {
        match Self::preview_scope(scene, target)? {
            LayerScope::Visible(target) => Self::for_composite(scene, target),
            LayerScope::All => Self::new(scene),
            LayerScope::Layer(id) => Self::build(scene, Some(id), true),
            LayerScope::Preview(_) => {
                let index = target.ok_or("Preview layer is missing")?;
                let mut topology = Self::new(scene)?;
                let tree = std::sync::Arc::get_mut(&mut topology).ok_or("Preview topology is already shared")?;
                tree.prepare_preview_dependencies(index)?;
                tree.preview_root = Some(index);
                Ok(topology)
            }
        }
    }

    fn build(
        scene: &'a SceneSnapshot,
        target: Option<&str>,
        raw_preview: bool,
    ) -> Result<std::sync::Arc<Self>> {
        let doc = scene.metadata();
        if target.is_some() && doc.read_only() {
            return Err("Protected documents cannot isolate editable source trees".into());
        }
        let budget = scene.budget();
        let count = doc.layers().len();
        if count > 4096 {
            return Err("Regional composition exceeds its topology admission".into());
        }
        let bytes = count
            .checked_mul(512)
            .and_then(|n| n.checked_add(512))
            .ok_or("Composition topology overflow")?;
        let charge = budget
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut layers = Vec::new();
        layers
            .try_reserve_exact(count)
            .map_err(|_| "Composition layer index allocation refused")?;
        layers.extend(doc.layers());
        let mut ids = HashMap::new();
        ids.try_reserve(count)
            .map_err(|_| "Composition identity index allocation refused")?;
        let mut parents = HashMap::new();
        parents
            .try_reserve(count + 1)
            .map_err(|_| "Composition folder index allocation refused")?;
        parents.insert(None, 0);
        let mut group_owner = Vec::new();
        group_owner
            .try_reserve_exact(count + 1)
            .map_err(|_| "Composition folder owners allocation refused")?;
        group_owner.push(None);
        for (i, layer) in layers.iter().enumerate() {
            if ids.insert(layer.id(), i).is_some() || layer.id().is_empty() {
                return Err("Duplicate or empty regional layer identity".into());
            }
            if !layer.opacity().is_finite() || !(0.0..=1.0).contains(&layer.opacity()) {
                return Err("Invalid regional layer opacity".into());
            }
            if layer.blend() != "pass_through"
                && !crate::raster::BLENDS
                    .iter()
                    .any(|(name, _)| *name == layer.blend())
            {
                return Err("Unsupported regional blend mode".into());
            }
            if layer.kind() == "group" {
                parents.insert(Some(layer.id()), group_owner.len());
                group_owner.push(Some(i));
            }
        }
        let root = target
            .map(|target| ids.get(target).copied().ok_or("Export layer is missing"))
            .transpose()?;
        if let Some(root) = root {
            if layers[root].kind() == "adjustment" {
                return Err("An adjustment export requires its backdrop".into());
            }
            if !raw_preview && layers[root].clip_to().is_some() {
                return Err("A clipped export requires its base inside a folder".into());
            }
        }
        let mut active = Vec::new();
        active
            .try_reserve_exact(count)
            .map_err(|_| "Composition tree membership allocation refused")?;
        for (index, layer) in layers.iter().enumerate() {
            let mut kept = root.is_none() || root == Some(index);
            let mut parent = layer.parent();
            let mut depth = 0;
            while let Some(id) = parent {
                depth += 1;
                if depth > 16 || id == layer.id() {
                    return Err("Cyclic or too-deep regional hierarchy".into());
                }
                let owner = *ids.get(id).ok_or("Unknown regional ancestor")?;
                kept |= root == Some(owner);
                parent = layers[owner].parent();
            }
            active.push(kept);
        }
        let mut groups = Vec::new();
        groups
            .try_reserve_exact(group_owner.len())
            .map_err(|_| "Composition groups allocation refused")?;
        groups.resize_with(group_owner.len(), Vec::new);
        let mut parent_group = Vec::new();
        parent_group
            .try_reserve_exact(count)
            .map_err(|_| "Composition parent index allocation refused")?;
        let mut kinds = Vec::new();
        kinds
            .try_reserve_exact(count)
            .map_err(|_| "Composition layer kinds allocation refused")?;
        let mut clips = Vec::new();
        clips
            .try_reserve_exact(count)
            .map_err(|_| "Composition clipping index allocation refused")?;
        clips.resize_with(count, Vec::new);
        let mut clip_base = Vec::new();
        clip_base.try_reserve_exact(count)
            .map_err(|_| "Composition clipping bases allocation refused")?;
        clip_base.resize(count, None);
        for (i, layer) in layers.iter().enumerate() {
            let parent = if root == Some(i) {
                None
            } else {
                layer.parent()
            };
            let group = *parents
                .get(&parent)
                .ok_or("Unknown regional parent folder")?;
            parent_group.push(group);
            let mut parent = layer.parent();
            let mut depth = 0;
            while let Some(id) = parent {
                depth += 1;
                if depth > 16 || id == layer.id() {
                    return Err("Cyclic or too-deep regional hierarchy".into());
                }
                let owner = *ids.get(id).ok_or("Unknown regional ancestor")?;
                if layers[owner].kind() != "group" {
                    return Err("Regional parent is not a folder".into());
                }
                parent = layers[owner].parent();
            }
            if layer.blend() == "pass_through"
                && (layer.kind() != "group"
                    || layer.clip_to().is_some()
                    || layer.effects().any(|e| e.enabled()))
            {
                return Err("Invalid pass-through regional folder".into());
            }
            kinds.push(match layer.kind() {
                "group" => Kind::Group(parents[&Some(layer.id())]),
                "fill" => Kind::Fill,
                "adjustment" => Kind::Adjustment,
                _ => Kind::Raster,
            });
            if let Some(id) = layer.clip_to() {
                let base = (i + 1..count)
                    .find(|&j| {
                        layers[j].parent() == layer.parent() && layers[j].clip_to().is_none()
                    })
                    .ok_or("Regional clipping has no base")?;
                let b = &layers[base];
                if b.id() != id || b.kind() == "adjustment" || b.blend() == "pass_through" {
                    return Err("Regional clipping base is inconsistent".into());
                }
                clip_base[i] = Some(base);
                if active[i] && !active[base] {
                    // Raw raster previews sample the layer before clipping.
                    if raw_preview && root == Some(i) {
                        continue;
                    }
                    return Err("Export folder has a clipping reference outside its tree".into());
                }
                if active[i] && layer.visible() {
                    clips[base]
                        .try_reserve(1)
                        .map_err(|_| "Composition clipping members allocation refused")?;
                    clips[base].push(i);
                }
            } else if active[i] && (layer.visible() || root == Some(i)) {
                groups[group]
                    .try_reserve(1)
                    .map_err(|_| "Composition group members allocation refused")?;
                groups[group].push(i);
            }
        }
        for children in &mut groups {
            children.reverse();
        }
        for children in &mut clips {
            children.reverse();
        }
        let mut opacity = Vec::new();
        opacity.try_reserve_exact(count).map_err(|_| "Composition opacity allocation refused")?;
        opacity.extend(layers.iter().map(|layer| {
            let (opacity, fill) = crate::psd::fill_opacity::render(layer.opacity(), layer.blend(), layer.psd_metadata());
            (opacity, fill, crate::psd::blend_ranges::ranges(layer.psd_metadata()),
                crate::psd::blend_channels::excluded(layer.psd_metadata()))
        }));
        Ok(std::sync::Arc::new(Self {
            scene,
            layers,
            active,
            root,
            preview_root: None,
            visible_only: false,
            groups,
            parent_group,
            group_owner,
            kinds,
            clips,
            clip_base,
            opacity,
            _topology: charge,
        }))
    }

    fn prepare_preview_dependencies(&mut self, index: usize) -> Result<()> {
        let count = self.layers.len();
        let mut pending = Vec::new();
        pending.try_reserve_exact(count)
            .map_err(|_| "Preview layer dependencies allocation refused")?;
        let mut group_cutoffs = Vec::new();
        group_cutoffs.try_reserve_exact(self.groups.len())
            .map_err(|_| "Preview folder dependencies allocation refused")?;
        group_cutoffs.resize(self.groups.len(), usize::MAX);
        let mut clip_counts = Vec::new();
        clip_counts.try_reserve_exact(count)
            .map_err(|_| "Preview clip dependencies allocation refused")?;
        clip_counts.resize(count, 0);
        let mut active = std::mem::take(&mut self.active);
        active.fill(false);
        let mut dependencies = PreviewDependencies { tree: self, active, pending, group_cutoffs, clip_counts };
        dependencies.layer(index);
        while let Some(i) = dependencies.pending.pop() {
            match dependencies.tree.kinds[i] {
                Kind::Group(group) => dependencies.below(group, 0),
                Kind::Adjustment => dependencies.adjustment(i),
                Kind::Raster | Kind::Fill => {}
            }
        }
        self.active = dependencies.active;
        Ok(())
    }
}

struct PreviewDependencies<'a, 'scene> {
    tree: &'a Topology<'scene>,
    active: Vec<bool>,
    pending: Vec<usize>,
    group_cutoffs: Vec<usize>,
    clip_counts: Vec<usize>,
}

impl PreviewDependencies<'_, '_> {
    fn layer(&mut self, index: usize) {
        if !std::mem::replace(&mut self.active[index], true) {
            self.pending.push(index);
        }
    }

    fn unit(&mut self, base: usize, above: usize) {
        self.layer(base);
        let tree = self.tree;
        let clips = &tree.clips[base];
        let end = clips.partition_point(|&index| index >= above);
        let start = self.clip_counts[base];
        for &index in clips.iter().take(end).skip(start) {
            self.layer(index);
        }
        self.clip_counts[base] = start.max(end);
    }

    fn below(&mut self, group: usize, above: usize) {
        let previous = self.group_cutoffs[group];
        if above >= previous {
            return;
        }
        self.group_cutoffs[group] = above;
        let tree = self.tree;
        let children = &tree.groups[group];
        // Only the uppermost previously visited base can have clips beyond the old cutoff.
        let start = children.partition_point(|&index| index >= previous).saturating_sub(1);
        let end = children.partition_point(|&index| index >= above);
        for &index in &children[start..end] {
            self.unit(index, above);
        }
    }

    fn backdrop(&mut self, mut group: usize) {
        while let Some(owner) = self.tree.group_owner[group] {
            if !self.tree.passes_backdrop(owner) {
                break;
            }
            group = self.tree.parent_group[owner];
            self.below(group, owner + 1);
        }
    }

    fn adjustment(&mut self, index: usize) {
        if let Some(base) = self.tree.clip_base[index] {
            self.unit(base, index + 1);
            if self.tree.opacity[base].2.is_some() {
                let group = self.tree.parent_group[base];
                self.below(group, base + 1);
                self.backdrop(group);
            }
        } else {
            let group = self.tree.parent_group[index];
            self.below(group, index + 1);
            self.backdrop(group);
        }
    }
}

fn sparse_halo<'a>(
    effects: impl Iterator<Item = crate::engine::native::EffectMetadata<'a>>,
    depth: u16,
    check: &mut dyn FnMut() -> Result<()>,
) -> Result<Option<i64>> {
    let mut halo = 0;
    for effect in effects {
        check()?;
        if effect.clips().len() != 0 {
            return Ok(None);
        }
        if !effect.enabled() || effect.weight() <= 0. {
            continue;
        }
        crate::effects::validate_weight(effect.weight())?;
        let Some(reach) = crate::effects::regional::zero_preserving_reach(effect.kind(), effect.settings(), depth)? else {
            return Ok(None);
        };
        halo += i64::from(reach);
    }
    Ok(Some(halo))
}

pub(crate) struct Plan<'a, P: Precision, S: PreparedLayers<P>> {
    topology: std::sync::Arc<Topology<'a>>,
    prepared: &'a S,
    // Dependency preparation can span several individually admitted stripes.
    // Public plan constructors still take one bounded output Region.
    region: [i32; 4],
    _precision: std::marker::PhantomData<P>,
}
impl<'a, P: Precision, S: PreparedLayers<P>> Plan<'a, P, S> {
    fn check_preview_dependency(&self, index: usize) -> Result<()> {
        if self.topology.preview_root.is_some() && !self.topology.active[index] {
            return Err("Preview sampled a layer outside its prepared dependencies".into());
        }
        Ok(())
    }

    pub(crate) fn new(scene: &'a SceneSnapshot, prepared: &'a S, region: Region) -> Result<Self> {
        if !std::ptr::eq(scene, prepared.scene()) {
            return Err("Regional provider belongs to another captured scene".into());
        }
        Self::with_topology(Topology::new(scene)?, prepared, region)
    }
    pub(crate) fn with_topology(
        topology: std::sync::Arc<Topology<'a>>,
        prepared: &'a S,
        region: Region,
    ) -> Result<Self> {
        if !std::ptr::eq(topology.scene, prepared.scene()) {
            return Err("Regional provider belongs to another captured scene".into());
        }
        if topology.scope() != prepared.scope() {
            return Err("Regional provider belongs to another layer scope".into());
        }
        if topology.scene.metadata().depth() != P::DEPTH {
            return Err("Regional compositor precision differs from captured native depth".into());
        }
        for (i, layer) in topology.layers.iter().enumerate() {
            if topology.active[i]
                && layer
                    .effects()
                    .any(|effect| effect.enabled() && effect.weight() > 0.0)
                && !prepared.has_colour(i)
            {
                return Err("Enabled regional effects have no prepared native output".into());
            }
        }
        Ok(Self {
            topology,
            prepared,
            region: region.bounds(),
            _precision: std::marker::PhantomData,
        })
    }
    fn check_point(&self, x: i32, y: i32) -> Result<()> {
        let [l, t, r, b] = self.region;
        if x < l || y < t || x >= r || y >= b {
            return Err("Composition sample is outside its prepared region".into());
        }
        Ok(())
    }
    pub(crate) fn pixel(&self, x: i32, y: i32) -> Result<P::Pixel> {
        self.check_point(x, y)?;
        self.below(0, 0, x, y, (P::zero(), 0))
    }
    pub(crate) fn layer(&self, index: usize, x: i32, y: i32) -> Result<P::Pixel> {
        self.check_point(x, y)?;
        if index >= self.topology.kinds.len() {
            return Err("Unknown regional layer".into());
        }
        self.raw(index, x, y)
    }
    /// Standard mask serialization passes raw=true so disabling a layer mask
    /// changes its flag without replacing its editable samples with white.
    pub(crate) fn mask(&self, index: usize, x: i32, y: i32, raw: bool) -> Result<P::Amount> {
        self.check_point(x, y)?;
        self.mask_value(index, x, y, raw)
    }
    fn mask_value(&self, index: usize, x: i32, y: i32, raw: bool) -> Result<P::Amount> {
        let layer = self
            .topology
            .layers
            .get(index)
            .ok_or("Unknown regional mask layer")?;
        if layer.mask().is_none_or(|mask| !raw && !mask.enabled()) {
            return Ok(P::opacity(1.0));
        }
        let lx = x
            .checked_sub(layer.origin()[0])
            .ok_or("Regional mask coordinate overflow")?;
        let ly = y
            .checked_sub(layer.origin()[1])
            .ok_or("Regional mask coordinate overflow")?;
        self.prepared.mask(index, lx, ly, raw)
    }
    pub(crate) fn adjustment_input(&self, index: usize, x: i32, y: i32) -> Result<P::Pixel> {
        self.check_point(x, y)?;
        let base = self.topology.clip_base.get(index).ok_or("Unknown regional adjustment")?;
        if let Some(base) = *base {
            let ranged = self.topology.opacity[base].2.is_some();
            let outside = if ranged {
                let group = self.topology.parent_group[base];
                self.below(group, base + 1, x, y, self.backdrop(group, x, y)?)?
            } else {
                P::zero()
            };
            let (pixel, visible) = self.unit(base, index + 1, x, y, outside)?;
            if ranged {
                Ok(P::scale_alpha(pixel, self.amount(base, x, y)?.into() * visible))
            } else {
                Ok(pixel)
            }
        } else {
            let group = self.topology.parent_group[index];
            self.below(group, index + 1, x, y, self.backdrop(group, x, y)?)
        }
    }
    fn backdrop(&self, group: usize, x: i32, y: i32) -> Result<(P::Pixel, u8)> {
        let Some(index) = self.topology.group_owner[group] else {
            return Ok((P::zero(), 0));
        };
        if !self.topology.passes_backdrop(index) {
            return Ok((P::zero(), 0));
        }
        let parent = self.topology.parent_group[index];
        let backdrop = self.backdrop(parent, x, y)?;
        let out = self.below(parent, index + 1, x, y, backdrop)?;
        Ok((out, backdrop.1 | self.topology.opacity[index].3))
    }
    fn amount(&self, i: usize, x: i32, y: i32) -> Result<P::Amount> {
        let opacity = P::opacity(self.topology.opacity[i].0);
        Ok(P::multiply(opacity, self.mask_value(i, x, y, false)?))
    }
    fn gated_amount(&self, i: usize, dst: P::Pixel, src: P::Pixel, amount: P::Amount) -> P::Amount {
        self.topology.opacity[i].2.map_or(amount, |ranges|
            P::multiply(amount, P::opacity(P::range_coverage(ranges, dst, src, 1.))))
    }
    fn compose_layer(&self, i: usize, dst: P::Pixel, src: P::Pixel, amount: P::Amount) -> P::Pixel {
        if let Some(fill) = self.topology.opacity[i].1 {
            P::compose_special(dst, src, amount, fill)
        } else {
            let layer = self.topology.layers[i];
            let mode = super::layer_blend(
                layer.blend(), P::DEPTH == 8 && self.topology.scene.metadata().psd_rgb8_multiply(),
                layer.kind() == "paint", layer.clip_to().is_none(),
            );
            P::compose(dst, src, amount, mode)
        }
    }
    fn raw(&self, i: usize, x: i32, y: i32) -> Result<P::Pixel> {
        self.check_preview_dependency(i)?;
        if self.prepared.has_colour(i) {
            return self.prepared.colour(i, x, y);
        }
        let layer = &self.topology.layers[i];
        match self.topology.kinds[i] {
            Kind::Group(group) => self.below(group, 0, x, y, (P::zero(), 0)),
            Kind::Adjustment => Ok(P::zero()),
            Kind::Fill | Kind::Raster => {
                let lx = i64::from(x) - i64::from(layer.origin()[0]);
                let ly = i64::from(y) - i64::from(layer.origin()[1]);
                if lx < 0
                    || ly < 0
                    || lx >= i64::from(layer.pixels().width())
                    || ly >= i64::from(layer.pixels().height())
                {
                    return Ok(P::zero());
                }
                if matches!(self.topology.kinds[i], Kind::Fill) {
                    return Ok(P::fill(layer.color()));
                }
                self.prepared.source(i, lx as i32, ly as i32)
            }
        }
    }
    fn unit(&self, base: usize, above: usize, x: i32, y: i32, outside: P::Pixel) -> Result<(P::Pixel, f64)> {
        let mut pixel = self.raw(base, x, y)?;
        let mut visible = self.topology.opacity[base].2.map_or(1., |ranges|
            P::range_coverage(ranges, outside, pixel, 1.));
        if visible == 0. {
            pixel = P::alpha_from(P::zero(), pixel);
        }
        for &i in &self.topology.clips[base] {
            if i < above {
                continue;
            }
            let excluded = self.topology.opacity[i].3;
            if excluded == crate::psd::blend_channels::ALL {
                continue;
            }
            self.check_preview_dependency(i)?;
            let (before, before_visible) = (pixel, visible);
            if matches!(self.topology.kinds[i], Kind::Adjustment) && (!self.prepared.has_colour(i) || visible == 0.) {
                continue;
            }
            let source = self.raw(i, x, y)?;
            let amount = self.amount(i, x, y)?;
            let amount = if let Some(ranges) = self.topology.opacity[i].2 {
                let fill = P::opacity(super::clip_fill_factor(self.topology.opacity[base].1));
                let scale = P::multiply(self.amount(base, x, y)?, fill);
                P::multiply(amount, P::opacity(P::range_coverage(ranges, pixel, source, scale.into() * visible)))
            } else {
                amount
            };
            pixel = if matches!(self.topology.kinds[i], Kind::Adjustment) {
                P::adjust(pixel, source, amount, self.topology.layers[i].blend())
            } else {
                if visible != 1. {
                    visible = super::restore_clip_coverage(visible, P::alpha(source),
                        if P::DEPTH == 8 { 255 } else { 65535 }, amount.into(), self.topology.opacity[i].1);
                }
                P::alpha_from(self.compose_layer(i, P::opaque(pixel), source, amount), pixel)
            };
            pixel = P::keep_clipped_channels(before, pixel, excluded, before_visible, visible);
        }
        Ok((pixel, visible))
    }
    fn below(
        &self,
        group: usize,
        above: usize,
        x: i32,
        y: i32,
        backdrop: (P::Pixel, u8),
    ) -> Result<P::Pixel> {
        let (mut out, inherited) = backdrop;
        for &i in &self.topology.groups[group] {
            if i < above {
                continue;
            }
            let layer = &self.topology.layers[i];
            let excluded = inherited | self.topology.opacity[i].3;
            if excluded == crate::psd::blend_channels::ALL {
                continue;
            }
            self.check_preview_dependency(i)?;
            let before = out;
            if let Kind::Group(children) = self.topology.kinds[i] {
                if self.topology.passes_backdrop(i) {
                    out = P::mix(
                        out,
                        self.below(children, 0, x, y, (out, excluded))?,
                        self.amount(i, x, y)?,
                    );
                    out = P::keep_channels(before, out, excluded);
                    continue;
                }
            }
            if matches!(self.topology.kinds[i], Kind::Adjustment) {
                if self.prepared.has_colour(i) {
                    let image = self.prepared.colour(i, x, y)?;
                    let amount = self.gated_amount(i, out, image, self.amount(i, x, y)?);
                    out = if layer.blend() == "normal" {
                        P::mix(out, image, amount)
                    } else {
                        P::adjust(out, image, amount, &layer.blend())
                    };
                }
            } else {
                let (source, visible) = self.unit(i, above, x, y, out)?;
                let amount = P::multiply(self.amount(i, x, y)?, P::opacity(visible));
                out = self.compose_layer(i, out, source, amount);
            }
            out = P::keep_channels(before, out, excluded);
        }
        Ok(out)
    }
}
