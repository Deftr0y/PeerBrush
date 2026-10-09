//! Gesture-local derived images. Re-evaluate padded source windows, retaining untouched output.
//! These buffers never modify global caches, document rasters, history or saved sources.
use crate::{
    compositor::Plan,
    depth16::{Gray16, Image16, Plan16},
    effects::{self, Image},
    engine::{Document, Mask},
    mask::GrayMask,
    raster::Raster,
};
use serde_json::{json, Value};
use std::sync::Arc;

fn layout(doc: &Document) -> Value {
    json!([
        doc.width,
        doc.height,
        doc.bit_depth,
        doc.layers
            .iter()
            .map(|l| json!([
                l.id,
                l.kind,
                l.parent,
                l.clip_to,
                l.x,
                l.y,
                l.pixels.width,
                l.pixels.height,
                l.visible,
                l.opacity,
                l.blend,
                l.color,
                l.effects,
                l.mask.as_ref().map(|m| json!([
                    m.enabled,
                    m.steps
                        .iter()
                        .map(|s| json!([
                            s.id,
                            s.kind,
                            s.enabled,
                            s.value,
                            s.settings,
                            s.pixels.width,
                            s.pixels.height
                        ]))
                        .collect::<Vec<_>>()
                ]))
            ]))
            .collect::<Vec<_>>()
    ])
}

pub(crate) fn intersect(area: [i32; 4], width: u32, height: u32) -> [i32; 4] {
    [
        area[0].clamp(0, width as i32),
        area[1].clamp(0, height as i32),
        area[2].clamp(0, width as i32),
        area[3].clamp(0, height as i32),
    ]
}
pub(crate) fn window(area: [i32; 4], reach: i32, width: u32, height: u32) -> [i32; 4] {
    intersect(
        [
            area[0].saturating_sub(reach),
            area[1].saturating_sub(reach),
            area[2].saturating_add(reach),
            area[3].saturating_add(reach),
        ],
        width,
        height,
    )
}
pub(crate) fn crop(source: &Raster, rect: [i32; 4]) -> Raster {
    let mut out = Raster::new_depth(
        (rect[2] - rect[0]) as u32,
        (rect[3] - rect[1]) as u32,
        source.depth,
    );
    for y in rect[1]..rect[3] {
        for x in rect[0]..rect[2] {
            if source.depth == 16 {
                out.set16(x - rect[0], y - rect[1], source.get16(x, y));
            } else {
                out.set(x - rect[0], y - rect[1], source.get(x, y));
            }
        }
    }
    out
}
fn gaussian(value: f32) -> i32 {
    effects::gaussian_radii(value).iter().sum::<usize>() as i32
}
pub(crate) fn mask_reach(mask: &Mask) -> i32 {
    mask.steps
        .iter()
        .filter(|s| s.enabled)
        .map(|s| match s.kind.as_str() {
            "gaussian" => gaussian(effects::number(&s.settings, "radius", 8.)),
            "blur" if s.value >= 0.5 => 3 * ((s.value.round().clamp(1., 64.) as i32 + 2) / 3),
            _ => 0,
        })
        .sum()
}
fn color_reach(layer: &crate::engine::Layer) -> i32 {
    layer
        .effects
        .iter()
        .filter(|e| e.enabled)
        .map(|e| match e.kind.as_str() {
            "blur" => gaussian(effects::number(&e.settings, "radius", 8.)),
            "bloom" => gaussian(effects::number(&e.settings, "spread", 12.)),
            _ => 0,
        })
        .sum()
}
fn order(doc: &Document) -> Vec<usize> {
    crate::compositor::effect_order(doc)
}
fn local(area: [i32; 4], layer: &crate::engine::Layer) -> [i32; 4] {
    [
        area[0].saturating_sub(layer.x),
        area[1].saturating_sub(layer.y),
        area[2].saturating_sub(layer.x),
        area[3].saturating_sub(layer.y),
    ]
}
fn keys(doc: &Document) -> Vec<(String, Option<String>)> {
    doc.layers
        .iter()
        .map(|l| {
            (
                l.effect_key.clone(),
                l.mask.as_ref().map(|m| m.cache_key.clone()),
            )
        })
        .collect()
}

pub(super) struct Prepared8 {
    layout: Value,
    keys: Vec<(String, Option<String>)>,
    pub masks: Vec<Option<Arc<GrayMask>>>,
    pub colors: Vec<Option<Arc<Image>>>,
}
impl Prepared8 {
    pub fn new(doc: &Document) -> Result<Self, String> {
        crate::mask::validate_budget(&doc.layers)?;
        let masks = doc
            .layers
            .iter()
            .map(|l| {
                l.mask
                    .as_ref()
                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
            })
            .collect::<Vec<_>>();
        let colors = effects::prepare(doc, &masks)?;
        Ok(Self {
            layout: layout(doc),
            keys: keys(doc),
            masks,
            colors,
        })
    }
    pub fn compatible(&self, doc: &Document) -> bool {
        self.layout == layout(doc)
    }
    pub fn update(&mut self, doc: &Document, area: [i32; 4]) -> Result<(), String> {
        crate::mask::validate_budget(&doc.layers)?;
        effects::validate_budget(doc)?;
        for (i, layer) in doc.layers.iter().enumerate() {
            if self.keys[i].1 != layer.mask.as_ref().map(|m| m.cache_key.clone()) {
                self.masks[i] = layer.mask.as_ref().and_then(|m| {
                    crate::mask::regional(
                        m,
                        layer.pixels.width,
                        layer.pixels.height,
                        self.masks[i].take(),
                        local(area, layer),
                    )
                });
            }
        }
        for i in order(doc) {
            let layer = &doc.layers[i];
            if self.keys[i].0 == layer.effect_key || !effects::active(layer) {
                continue;
            }
            let Some(old) = self.colors[i].as_ref() else {
                return Err("Regional effect baseline is missing".into());
            };
            let (width, height) = (old.width, old.height);
            let output = intersect(
                if ["group", "adjustment"].contains(&layer.kind.as_str()) {
                    area
                } else {
                    local(area, layer)
                },
                width,
                height,
            );
            if output[0] >= output[2] || output[1] >= output[3] {
                continue;
            }
            let work = window(output, color_reach(layer), width, height);
            let (w, h) = ((work[2] - work[0]) as u32, (work[3] - work[1]) as u32);
            let plan = Plan::new(doc, &self.masks, &self.colors);
            let group = plan.group(Some(&layer.id));
            let mut image = Image {
                width: w,
                height: h,
                bytes: crate::render::rgba8(w, h, |x, y| {
                    let (x, y) = (work[0] + x as i32, work[1] + y as i32);
                    match layer.kind.as_str() {
                        "fill" => layer.color,
                        "group" => plan.sample(group, x, y),
                        "adjustment" => crate::compositor::adjustment_input(
                            doc,
                            i,
                            x,
                            y,
                            &self.masks,
                            &self.colors,
                        ),
                        _ => layer.pixels.get(x, y),
                    }
                }),
            };
            drop(plan);
            for effect in layer.effects.iter().filter(|e| e.enabled) {
                effects::apply_effect(
                    &mut image,
                    effect,
                    Some(u64::from(width) * u64::from(height)),
                )?;
            }
            let dest = Arc::make_mut(self.colors[i].as_mut().unwrap());
            for y in output[1]..output[3] {
                let from =
                    ((y - work[1]) as usize * w as usize + (output[0] - work[0]) as usize) * 4;
                let to = (y as usize * width as usize + output[0] as usize) * 4;
                let len = (output[2] - output[0]) as usize * 4;
                dest.bytes[to..to + len].copy_from_slice(&image.bytes[from..from + len]);
            }
        }
        self.keys = keys(doc);
        Ok(())
    }
}
pub(super) struct Prepared16 {
    layout: Value,
    keys: Vec<(String, Option<String>)>,
    pub masks: Vec<Option<Arc<Gray16>>>,
    pub colors: Vec<Option<Arc<Image16>>>,
}
impl Prepared16 {
    pub fn new(doc: &Document) -> Result<Self, String> {
        crate::depth16::validate_budget(doc)?;
        let masks = crate::depth16::prepare_masks(doc)?;
        let colors = crate::depth16::prepare_with_masks(doc, &masks)?;
        Ok(Self {
            layout: layout(doc),
            keys: keys(doc),
            masks,
            colors,
        })
    }
    pub fn compatible(&self, doc: &Document) -> bool {
        self.layout == layout(doc)
    }
    pub fn update(&mut self, doc: &Document, area: [i32; 4]) -> Result<(), String> {
        crate::depth16::validate_budget(doc)?;
        for (i, layer) in doc.layers.iter().enumerate() {
            if self.keys[i].1 != layer.mask.as_ref().map(|m| m.cache_key.clone()) {
                self.masks[i] = crate::depth16::mask::regional(
                    layer,
                    self.masks[i].take(),
                    local(area, layer),
                )?;
            }
        }
        for i in order(doc) {
            let layer = &doc.layers[i];
            if self.keys[i].0 == layer.effect_key || !effects::active(layer) {
                continue;
            }
            let Some(old) = self.colors[i].as_ref() else {
                return Err("Native regional effect baseline is missing".into());
            };
            let (width, height) = (old.width, old.height);
            let output = intersect(
                if ["group", "adjustment"].contains(&layer.kind.as_str()) {
                    area
                } else {
                    local(area, layer)
                },
                width,
                height,
            );
            if output[0] >= output[2] || output[1] >= output[3] {
                continue;
            }
            let work = window(output, color_reach(layer), width, height);
            let (w, h) = ((work[2] - work[0]) as u32, (work[3] - work[1]) as u32);
            let plan = Plan16::with_prepared(doc, &self.masks, &self.colors);
            let group = plan.group(Some(&layer.id));
            let mut image = Image16 {
                width: w,
                height: h,
                words: crate::render::rgba16(w, h, |x, y| {
                    let (x, y) = (work[0] + x as i32, work[1] + y as i32);
                    match layer.kind.as_str() {
                        "fill" => layer.color.map(|v| u16::from(v) * 257),
                        "group" => plan.sample(group, x, y),
                        "adjustment" => plan.adjustment_input(i, x, y),
                        _ => layer.pixels.get16(x, y),
                    }
                }),
            };
            drop(plan);
            for effect in layer.effects.iter().filter(|e| e.enabled) {
                crate::depth16::color::apply_effect(
                    &mut image,
                    effect,
                    Some(u64::from(width) * u64::from(height)),
                )?;
            }
            let dest = Arc::make_mut(self.colors[i].as_mut().unwrap());
            for y in output[1]..output[3] {
                let from =
                    ((y - work[1]) as usize * w as usize + (output[0] - work[0]) as usize) * 4;
                let to = (y as usize * width as usize + output[0] as usize) * 4;
                let len = (output[2] - output[0]) as usize * 4;
                dest.words[to..to + len].copy_from_slice(&image.words[from..from + len]);
            }
        }
        self.keys = keys(doc);
        Ok(())
    }
}
