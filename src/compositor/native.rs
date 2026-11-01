//! Fallible native composition over captured Source36 metadata. Providers own
//! prepared source/derived regions; sampling performs no backing-file I/O.
use crate::{
    engine::scene::{Document36, SceneSnapshot},
    large_document::{Reservation, ResidentBudget},
    raster::{backed::Region, surface::SurfaceRegion, Pixel, Pixel16},
};
use std::collections::HashMap;

type Result<T> = std::result::Result<T, String>;

pub(crate) trait Precision {
    const DEPTH: u16;
    type Pixel: Copy;
    type Amount: Copy;
    fn zero() -> Self::Pixel;
    fn fill(pixel: Pixel) -> Self::Pixel;
    fn opacity(value: f32) -> Self::Amount;
    fn multiply(a: Self::Amount, b: Self::Amount) -> Self::Amount;
    fn sample(view: &SurfaceRegion, x: i32, y: i32) -> Result<Self::Pixel>;
    fn opaque(pixel: Self::Pixel) -> Self::Pixel;
    fn alpha_from(pixel: Self::Pixel, source: Self::Pixel) -> Self::Pixel;
    fn compose(a: Self::Pixel, b: Self::Pixel, t: Self::Amount, mode: &str) -> Self::Pixel;
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
        let base_id = doc.layers[3].id.clone();
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
        if clipped {
            adjustment.clip_to = Some(base_id);
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
    fn prepared_fractional_and_cached_masks_adjustments_and_pass_through_match_legacy() {
        let region = Region::new([-2, -1, 11, 9]).unwrap();
        for depth in [8, 16] {
            for pass in [false, true] {
                for clipped in [false, true] {
                    for cached in [false, true] {
                        let doc = setup(depth, pass, clipped, cached);
                        let (scene, path) = tests::capture(&doc);
                        assert!(RawLayers::prepare(&scene, region, &mut || Ok(())).is_err());
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
                            for y in -1..9 {
                                for x in -2..11 {
                                    assert_eq!(
                                        plan.pixel(x, y).unwrap(),
                                        legacy.sample(legacy.group(None), x, y),
                                        "8 {pass} {clipped} {cached} {x},{y}"
                                    );
                                    assert_eq!(plan.layer(0, x, y).unwrap(), legacy.layer(0, x, y));
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
                            for y in -1..9 {
                                for x in -2..11 {
                                    assert_eq!(
                                        plan.pixel(x, y).unwrap(),
                                        legacy.pixel(x, y),
                                        "16 {pass} {clipped} {cached} {x},{y}"
                                    );
                                    assert_eq!(plan.layer(0, x, y).unwrap(), legacy.layer(0, x, y));
                                    assert_eq!(
                                        plan.adjustment_input(2, x, y).unwrap(),
                                        legacy.adjustment_input(2, x, y)
                                    );
                                }
                            }
                        }
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
    ($name:ty, $pixel:ty, $word:ty, $float:ty, $depth:expr, $max:expr, $blend:path, $fill:expr, $sample:ident) => {
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
            fn opacity(value: f32) -> Self::Amount {
                value as $float
            }
            fn multiply(a: Self::Amount, b: Self::Amount) -> Self::Amount {
                a * b
            }
            fn sample(view: &SurfaceRegion, x: i32, y: i32) -> Result<Self::Pixel> {
                view.$sample(x, y).map_err(|e| e.to_string())
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
    pixel
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
    pixel16
);

/// Implementations retain all pins, derived outputs and their shared charges.
/// Mask coordinates are layer-local. Colour coordinates are document-global;
/// spatial style expansion and adjustment backdrops have already been prepared.
pub(crate) trait PreparedLayers<P: Precision> {
    fn scene(&self) -> &SceneSnapshot;
    fn has_colour(&self, layer: usize) -> bool;
    fn colour(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel>;
    fn source(&self, layer: usize, x: i32, y: i32) -> Result<P::Pixel>;
    fn mask(&self, layer: usize, x: i32, y: i32, raw: bool) -> Result<P::Amount>;
}

/// Real source-backed provider for the unstyled base stage. It refuses derived
/// work rather than falling back to an unprepared legacy full-image cache.
pub(crate) struct RawLayers<'a> {
    scene: &'a SceneSnapshot,
    sources: Vec<Option<SurfaceRegion>>,
    _index: Reservation,
}
impl<'a> RawLayers<'a> {
    pub(crate) fn prepare(
        scene: &'a SceneSnapshot,
        region: Region,
        check: &mut dyn FnMut() -> crate::large_document::Result<()>,
    ) -> Result<Self> {
        check().map_err(|e| e.to_string())?;
        let layers = &scene.metadata().layers;
        let bytes = layers
            .len()
            .checked_mul(512)
            .ok_or("Regional source index overflow")?;
        let charge = scene
            .budget()
            .reserve_bytes(bytes, 4 * 1024 * 1024)
            .map_err(|e| e.to_string())?;
        let mut sources = Vec::new();
        sources
            .try_reserve_exact(layers.len())
            .map_err(|_| "Regional source index allocation refused")?;
        let [l, t, r, b] = region.bounds();
        for layer in layers {
            check().map_err(|e| e.to_string())?;
            if layer.mask.as_ref().is_some_and(|mask| mask.enabled)
                || layer
                    .effects
                    .iter()
                    .any(|effect| effect.enabled && effect.weight > 0.0)
            {
                return Err("Regional source stage requires prepared masks and effects".into());
            }
            let source = scene.raster(&layer.pixels)?;
            if !source.uses_budget(scene.budget()) || !source.uses_disk_budget(scene.disk_budget())
            {
                return Err("Regional source is outside captured global admission".into());
            }
            let left = (i64::from(l) - i64::from(layer.x)).max(0);
            let top = (i64::from(t) - i64::from(layer.y)).max(0);
            let right = (i64::from(r) - i64::from(layer.x)).min(i64::from(layer.pixels.width));
            let bottom = (i64::from(b) - i64::from(layer.y)).min(i64::from(layer.pixels.height));
            let view = if left < right
                && top < bottom
                && !["group", "fill", "adjustment"].contains(&layer.kind.as_str())
            {
                let region = Region::new([left as i32, top as i32, right as i32, bottom as i32])
                    .map_err(|e| e.to_string())?;
                Some(
                    source
                        .prepare_region(region, check)
                        .map_err(|e| e.to_string())?,
                )
            } else {
                None
            };
            sources.push(view);
        }
        check().map_err(|e| e.to_string())?;
        Ok(Self {
            scene,
            sources,
            _index: charge,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{
            scene::{PendingScene, RasterCollector, RasterRef36, RasterRoster, SceneIdentity},
            Document, Layer,
        },
        large_document::{DiskBudget, Limits, Store},
        raster::{Raster, Surface},
    };
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
        let budget = ResidentBudget::new(32 * 1024 * 1024).unwrap();
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
        let scene = SceneSnapshot::new(
            SceneIdentity {
                project_id: "regional-test".into(),
                document_id: doc.id.clone(),
                revision: doc.revision,
            },
            pending,
            vec![],
            None,
            None,
        )
        .unwrap();
        (scene, path)
    }
    pub(super) fn cleanup(path: &std::path::Path) {
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
    pub(super) fn doc(depth: u16, pass: bool, blend: &str) -> Document {
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
}
impl<P: Precision> PreparedLayers<P> for RawLayers<'_> {
    fn scene(&self) -> &SceneSnapshot {
        self.scene
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
    fn mask(&self, _: usize, _: i32, _: i32, _: bool) -> Result<P::Amount> {
        Err("No native mask region was prepared".into())
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Raster,
    Fill,
    Group(usize),
    Adjustment,
}

pub(crate) struct Plan<'a, P: Precision, S: PreparedLayers<P>> {
    doc: &'a Document36,
    prepared: &'a S,
    region: Region,
    groups: Vec<Vec<usize>>,
    parent_group: Vec<usize>,
    group_owner: Vec<Option<usize>>,
    kinds: Vec<Kind>,
    clips: Vec<Vec<usize>>,
    _precision: std::marker::PhantomData<P>,
    // Declared last so every topology allocation is released before its charge.
    _topology: Reservation,
}

impl<'a, P: Precision, S: PreparedLayers<P>> Plan<'a, P, S> {
    pub(crate) fn new(scene: &'a SceneSnapshot, prepared: &'a S, region: Region) -> Result<Self> {
        if !std::ptr::eq(scene, prepared.scene()) {
            return Err("Regional provider belongs to another captured scene".into());
        }
        if scene.metadata().bit_depth != P::DEPTH {
            return Err("Regional compositor precision differs from captured native depth".into());
        }
        Self::compile(scene.metadata(), scene.budget(), prepared, region)
    }

    fn compile(
        doc: &'a Document36,
        budget: &ResidentBudget,
        prepared: &'a S,
        region: Region,
    ) -> Result<Self> {
        let count = doc.layers.len();
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
        let mut ids = HashMap::with_capacity(count);
        let mut parents = HashMap::with_capacity(count + 1);
        parents.insert(None, 0);
        let mut group_owner = vec![None];
        for (i, layer) in doc.layers.iter().enumerate() {
            if ids.insert(layer.id.as_str(), i).is_some() || layer.id.is_empty() {
                return Err("Duplicate or empty regional layer identity".into());
            }
            if !layer.opacity.is_finite() || !(0.0..=1.0).contains(&layer.opacity) {
                return Err("Invalid regional layer opacity".into());
            }
            if layer.blend != "pass_through"
                && !crate::raster::BLENDS
                    .iter()
                    .any(|(name, _)| *name == layer.blend)
            {
                return Err("Unsupported regional blend mode".into());
            }
            if layer.kind == "group" {
                parents.insert(Some(layer.id.as_str()), group_owner.len());
                group_owner.push(Some(i));
            }
        }
        let mut groups = vec![Vec::new(); group_owner.len()];
        let mut parent_group = Vec::with_capacity(count);
        let mut kinds = Vec::with_capacity(count);
        let mut clips = vec![Vec::new(); count];
        for (i, layer) in doc.layers.iter().enumerate() {
            let group = *parents
                .get(&layer.parent.as_deref())
                .ok_or("Unknown regional parent folder")?;
            parent_group.push(group);
            let mut parent = layer.parent.as_deref();
            let mut depth = 0;
            while let Some(id) = parent {
                depth += 1;
                if depth > 16 || id == layer.id {
                    return Err("Cyclic or too-deep regional hierarchy".into());
                }
                let owner = *ids.get(id).ok_or("Unknown regional ancestor")?;
                if doc.layers[owner].kind != "group" {
                    return Err("Regional parent is not a folder".into());
                }
                parent = doc.layers[owner].parent.as_deref();
            }
            if layer.blend == "pass_through"
                && (layer.kind != "group"
                    || layer.clip_to.is_some()
                    || layer.effects.iter().any(|e| e.enabled))
            {
                return Err("Invalid pass-through regional folder".into());
            }
            kinds.push(match layer.kind.as_str() {
                "group" => Kind::Group(parents[&Some(layer.id.as_str())]),
                "fill" => Kind::Fill,
                "adjustment" => Kind::Adjustment,
                _ => Kind::Raster,
            });
            if let Some(id) = layer.clip_to.as_deref() {
                let base = (i + 1..count)
                    .find(|&j| {
                        doc.layers[j].parent == layer.parent && doc.layers[j].clip_to.is_none()
                    })
                    .ok_or("Regional clipping has no base")?;
                let b = &doc.layers[base];
                if b.id != id || b.kind == "adjustment" || b.blend == "pass_through" {
                    return Err("Regional clipping base is inconsistent".into());
                }
                if layer.visible {
                    clips[base].push(i);
                }
            } else if layer.visible {
                groups[group].push(i);
            }
            let needs_colour = layer
                .effects
                .iter()
                .any(|effect| effect.enabled && effect.weight > 0.0);
            if needs_colour && !prepared.has_colour(i) {
                return Err("Enabled regional effects have no prepared native output".into());
            }
        }
        for children in &mut groups {
            children.reverse();
        }
        for children in &mut clips {
            children.reverse();
        }
        Ok(Self {
            doc,
            prepared,
            region,
            groups,
            parent_group,
            group_owner,
            kinds,
            clips,
            _precision: std::marker::PhantomData,
            _topology: charge,
        })
    }

    fn check_point(&self, x: i32, y: i32) -> Result<()> {
        let [l, t, r, b] = self.region.bounds();
        if x < l || y < t || x >= r || y >= b {
            return Err("Composition sample is outside its prepared region".into());
        }
        Ok(())
    }
    pub(crate) fn pixel(&self, x: i32, y: i32) -> Result<P::Pixel> {
        self.check_point(x, y)?;
        self.below(0, 0, x, y, P::zero())
    }
    pub(crate) fn layer(&self, index: usize, x: i32, y: i32) -> Result<P::Pixel> {
        self.check_point(x, y)?;
        if index >= self.kinds.len() {
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
            .doc
            .layers
            .get(index)
            .ok_or("Unknown regional mask layer")?;
        if layer.mask.as_ref().is_none_or(|mask| !raw && !mask.enabled) {
            return Ok(P::opacity(1.0));
        }
        let lx = x
            .checked_sub(layer.x)
            .ok_or("Regional mask coordinate overflow")?;
        let ly = y
            .checked_sub(layer.y)
            .ok_or("Regional mask coordinate overflow")?;
        self.prepared.mask(index, lx, ly, raw)
    }
    pub(crate) fn adjustment_input(&self, index: usize, x: i32, y: i32) -> Result<P::Pixel> {
        self.check_point(x, y)?;
        let layer = self
            .doc
            .layers
            .get(index)
            .ok_or("Unknown regional adjustment")?;
        if let Some(id) = layer.clip_to.as_deref() {
            let base = self
                .doc
                .layers
                .iter()
                .position(|l| l.id == id)
                .ok_or("Missing regional clip base")?;
            self.unit(base, index + 1, x, y)
        } else {
            let group = self.parent_group[index];
            self.below(group, index + 1, x, y, self.backdrop(group, x, y)?)
        }
    }
    fn backdrop(&self, group: usize, x: i32, y: i32) -> Result<P::Pixel> {
        let Some(index) = self.group_owner[group] else {
            return Ok(P::zero());
        };
        if self.doc.layers[index].blend != "pass_through" {
            return Ok(P::zero());
        }
        let parent = self.parent_group[index];
        self.below(parent, index + 1, x, y, self.backdrop(parent, x, y)?)
    }
    fn amount(&self, i: usize, x: i32, y: i32) -> Result<P::Amount> {
        let layer = &self.doc.layers[i];
        let opacity = P::opacity(layer.opacity);
        Ok(P::multiply(opacity, self.mask_value(i, x, y, false)?))
    }
    fn raw(&self, i: usize, x: i32, y: i32) -> Result<P::Pixel> {
        if self.prepared.has_colour(i) {
            return self.prepared.colour(i, x, y);
        }
        let layer = &self.doc.layers[i];
        match self.kinds[i] {
            Kind::Group(group) => self.below(group, 0, x, y, P::zero()),
            Kind::Adjustment => Ok(P::zero()),
            Kind::Fill | Kind::Raster => {
                let lx = i64::from(x) - i64::from(layer.x);
                let ly = i64::from(y) - i64::from(layer.y);
                if lx < 0
                    || ly < 0
                    || lx >= i64::from(layer.pixels.width)
                    || ly >= i64::from(layer.pixels.height)
                {
                    return Ok(P::zero());
                }
                if matches!(self.kinds[i], Kind::Fill) {
                    return Ok(P::fill(layer.color));
                }
                self.prepared.source(i, lx as i32, ly as i32)
            }
        }
    }
    fn unit(&self, base: usize, above: usize, x: i32, y: i32) -> Result<P::Pixel> {
        let mut pixel = self.raw(base, x, y)?;
        for &i in &self.clips[base] {
            if i < above {
                continue;
            }
            if matches!(self.kinds[i], Kind::Adjustment) && !self.prepared.has_colour(i) {
                continue;
            }
            let source = self.raw(i, x, y)?;
            let amount = self.amount(i, x, y)?;
            let blend = &self.doc.layers[i].blend;
            pixel = if matches!(self.kinds[i], Kind::Adjustment) {
                P::adjust(pixel, source, amount, blend)
            } else {
                P::alpha_from(P::compose(P::opaque(pixel), source, amount, blend), pixel)
            };
        }
        Ok(pixel)
    }
    fn below(
        &self,
        group: usize,
        above: usize,
        x: i32,
        y: i32,
        mut out: P::Pixel,
    ) -> Result<P::Pixel> {
        for &i in &self.groups[group] {
            if i < above {
                continue;
            }
            let layer = &self.doc.layers[i];
            if let Kind::Group(children) = self.kinds[i] {
                if layer.blend == "pass_through" {
                    out = P::mix(
                        out,
                        self.below(children, 0, x, y, out)?,
                        self.amount(i, x, y)?,
                    );
                    continue;
                }
            }
            if matches!(self.kinds[i], Kind::Adjustment) {
                if self.prepared.has_colour(i) {
                    let image = self.prepared.colour(i, x, y)?;
                    let amount = self.amount(i, x, y)?;
                    out = if layer.blend == "normal" {
                        P::mix(out, image, amount)
                    } else {
                        P::adjust(out, image, amount, &layer.blend)
                    };
                }
            } else {
                out = P::compose(
                    out,
                    self.unit(i, above, x, y)?,
                    self.amount(i, x, y)?,
                    &layer.blend,
                );
            }
        }
        Ok(out)
    }
}
