//! Shared straight-alpha composition with isolated clipping groups.
use crate::{
    effects::Image,
    engine::Document,
    mask::GrayMask,
    raster::{blend, Pixel},
};
use std::sync::Arc;
type Masks = [Option<Arc<GrayMask>>];
type Colors = [Option<Arc<Image>>];

fn raw(doc: &Document, i: usize, x: i32, y: i32, masks: &Masks, colors: &Colors) -> Pixel {
    let l = &doc.layers[i];
    if let Some(image) = &colors[i] {
        return if ["group", "adjustment"].contains(&l.kind.as_str()) {
            image.get(x, y)
        } else {
            image.get(x - l.x, y - l.y)
        };
    }
    match l.kind.as_str() {
        "group" => sample_group(doc, Some(&l.id), x, y, masks, colors),
        "fill"
            if x >= l.x
                && y >= l.y
                && x - l.x < l.pixels.width as i32
                && y - l.y < l.pixels.height as i32 =>
        {
            l.color
        }
        "fill" | "adjustment" => [0; 4],
        _ => l.pixels.get(x - l.x, y - l.y),
    }
}
fn amount(doc: &Document, i: usize, x: i32, y: i32, masks: &Masks) -> f32 {
    let l = &doc.layers[i];
    l.opacity * l.mask_value_prepared(x - l.x, y - l.y, masks[i].as_deref(), false)
}
// Adjustments replace the derived backdrop, rather than layering a copy over it.
fn mix(a: Pixel, b: Pixel, t: f32) -> Pixel {
    let aa = a[3] as f32 / 255.;
    let ba = b[3] as f32 / 255.;
    let alpha = aa * (1. - t) + ba * t;
    if alpha <= 0. {
        return [0; 4];
    }
    if a[3] == b[3] {
        return color_adjust(a, b, t, "normal");
    }
    let mut out = [0, 0, 0, (alpha * 255.).round() as u8];
    for c in 0..3 {
        out[c] = ((a[c] as f32 * aa * (1. - t) + b[c] as f32 * ba * t) / alpha)
            .round()
            .clamp(0., 255.) as u8;
    }
    out
}
fn color_adjust(a: Pixel, b: Pixel, t: f32, mode: &str) -> Pixel {
    if mode == "normal" {
        let mut out = a;
        for c in 0..3 {
            out[c] = (a[c] as f32 * (1.0 - t) + b[c] as f32 * t)
                .round()
                .clamp(0.0, 255.0) as u8;
        }
        return out;
    }
    let mut dst = a;
    let mut src = b;
    dst[3] = 255;
    src[3] = 255;
    let out = blend(dst, src, t, mode);
    [out[0], out[1], out[2], a[3]]
}
fn backdrop_adjust(a: Pixel, b: Pixel, t: f32, mode: &str) -> Pixel {
    if mode == "normal" {
        mix(a, b, t)
    } else {
        color_adjust(a, b, t, mode)
    }
}
fn unit(
    doc: &Document,
    base: usize,
    above: usize,
    x: i32,
    y: i32,
    masks: &Masks,
    colors: &Colors,
) -> Pixel {
    let l = &doc.layers[base];
    if !l.visible {
        return [0; 4];
    }
    let mut p = raw(doc, base, x, y, masks, colors);
    for i in (above..base).rev() {
        let clip = &doc.layers[i];
        if clip.parent != l.parent || clip.clip_to.as_deref() != Some(&l.id) || !clip.visible {
            continue;
        }
        let mut src = raw(doc, i, x, y, masks, colors);
        let t = amount(doc, i, x, y, masks);
        if clip.kind == "adjustment" {
            if colors[i].is_none() {
                continue;
            }
            p = color_adjust(p, src, t, &clip.blend);
        } else {
            let alpha = p[3];
            let mut opaque = p;
            opaque[3] = 255;
            src = blend(opaque, src, t, &clip.blend);
            p = [src[0], src[1], src[2], alpha];
        }
    }
    p
}
fn below(
    doc: &Document,
    parent: Option<&str>,
    above: usize,
    x: i32,
    y: i32,
    masks: &Masks,
    colors: &Colors,
) -> Pixel {
    let mut out = [0; 4];
    for i in (above..doc.layers.len()).rev() {
        let l = &doc.layers[i];
        if l.parent.as_deref() != parent || !l.visible || l.clip_to.is_some() {
            continue;
        }
        if l.kind == "adjustment" {
            if let Some(image) = &colors[i] {
                out = backdrop_adjust(out, image.get(x, y), amount(doc, i, x, y, masks), &l.blend);
            }
        } else {
            out = blend(
                out,
                unit(doc, i, above, x, y, masks, colors),
                amount(doc, i, x, y, masks),
                &l.blend,
            );
        }
    }
    out
}
pub(crate) fn sample_group(
    doc: &Document,
    parent: Option<&str>,
    x: i32,
    y: i32,
    masks: &Masks,
    colors: &Colors,
) -> Pixel {
    below(doc, parent, 0, x, y, masks, colors)
}
pub(crate) fn adjustment_input(
    doc: &Document,
    index: usize,
    x: i32,
    y: i32,
    masks: &Masks,
    colors: &Colors,
) -> Pixel {
    let l = &doc.layers[index];
    if let Some(base) = l
        .clip_to
        .as_deref()
        .and_then(|id| doc.layers.iter().position(|l| l.id == id))
    {
        unit(doc, base, index + 1, x, y, masks, colors)
    } else {
        below(doc, l.parent.as_deref(), index + 1, x, y, masks, colors)
    }
}

pub fn validate_clipping(doc: &Document) -> Result<(), String> {
    for (i, l) in doc.layers.iter().enumerate() {
        if let Some(id) = &l.clip_to {
            let siblings: Vec<_> = doc
                .layers
                .iter()
                .enumerate()
                .skip(i + 1)
                .filter(|(_, b)| b.parent == l.parent)
                .collect();
            let Some((_, base)) = siblings.iter().find(|(_, b)| b.clip_to.is_none()) else {
                return Err("Clipping needs a base layer below it".into());
            };
            if base.id != *id || base.kind == "adjustment" {
                return Err(
                    "Keep clipped layers directly above their base, or release clipping first"
                        .into(),
                );
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Kind {
    Raster,
    Fill,
    Group(usize),
    Adjustment,
}
/// Immutable composition topology, compiled once instead of filtering every layer per pixel.
pub struct Plan<'a> {
    doc: &'a Document,
    masks: &'a Masks,
    colors: &'a Colors,
    groups: Vec<Vec<usize>>,
    parents: std::collections::HashMap<Option<&'a str>, usize>,
    kinds: Vec<Kind>,
    clips: Vec<Vec<usize>>,
    normal: Vec<bool>,
    bounds: Vec<Option<[i32; 4]>>,
}
impl<'a> Plan<'a> {
    pub fn new(doc: &'a Document, masks: &'a Masks, colors: &'a Colors) -> Self {
        let mut parents = std::collections::HashMap::from([(None, 0)]);
        for layer in &doc.layers {
            if layer.kind == "group" {
                let next = parents.len();
                parents.insert(Some(layer.id.as_str()), next);
            }
        }
        let mut groups = vec![vec![]; parents.len()];
        let mut kinds = Vec::with_capacity(doc.layers.len());
        for (i, layer) in doc.layers.iter().enumerate() {
            kinds.push(match layer.kind.as_str() {
                "group" => Kind::Group(parents[&Some(layer.id.as_str())]),
                "fill" => Kind::Fill,
                "adjustment" => Kind::Adjustment,
                _ => Kind::Raster,
            });
            if layer.visible && layer.clip_to.is_none() {
                if let Some(&group) = parents.get(&layer.parent.as_deref()) {
                    groups[group].push(i);
                }
            }
        }
        for children in &mut groups {
            children.reverse();
        }
        let clips = doc
            .layers
            .iter()
            .enumerate()
            .map(|(base, layer)| {
                (0..base)
                    .rev()
                    .filter(|&i| {
                        let clip = &doc.layers[i];
                        clip.visible
                            && clip.parent == layer.parent
                            && clip.clip_to.as_deref() == Some(layer.id.as_str())
                    })
                    .collect()
            })
            .collect();
        let normal = doc
            .layers
            .iter()
            .map(|layer| layer.blend == "normal")
            .collect();
        fn envelope(
            index: usize,
            doc: &Document,
            colors: &Colors,
            kinds: &[Kind],
            groups: &[Vec<usize>],
            memo: &mut [Option<Option<[i32; 4]>>],
            depth: usize,
        ) -> Option<[i32; 4]> {
            if let Some(bounds) = memo[index] {
                return bounds;
            }
            if depth > 16 {
                return Some([-100000, -100000, 100000, 100000]);
            }
            let layer = &doc.layers[index];
            let bounds = if let Some(image) = &colors[index] {
                let [x, y] = if matches!(kinds[index], Kind::Adjustment | Kind::Group(_)) {
                    [0, 0]
                } else {
                    [layer.x, layer.y]
                };
                Some([x, y, x + image.width as i32, y + image.height as i32])
            } else {
                match kinds[index] {
                    Kind::Fill => Some([
                        layer.x,
                        layer.y,
                        layer.x + layer.pixels.width as i32,
                        layer.y + layer.pixels.height as i32,
                    ]),
                    Kind::Adjustment => None,
                    Kind::Group(group) => {
                        let mut union: Option<[i32; 4]> = None;
                        for &child in &groups[group] {
                            if let Some(b) =
                                envelope(child, doc, colors, kinds, groups, memo, depth + 1)
                            {
                                union = Some(union.map_or(b, |a| {
                                    [
                                        a[0].min(b[0]),
                                        a[1].min(b[1]),
                                        a[2].max(b[2]),
                                        a[3].max(b[3]),
                                    ]
                                }));
                            }
                        }
                        union
                    }
                    Kind::Raster => layer.pixels.tile_bounds().map(|b| {
                        [
                            layer.x + b[0],
                            layer.y + b[1],
                            layer.x + b[2],
                            layer.y + b[3],
                        ]
                    }),
                }
            };
            memo[index] = Some(bounds);
            bounds
        }
        let mut memo = vec![None; doc.layers.len()];
        let bounds = (0..doc.layers.len())
            .map(|index| envelope(index, doc, colors, &kinds, &groups, &mut memo, 0))
            .collect();
        Self {
            doc,
            masks,
            colors,
            groups,
            parents,
            kinds,
            clips,
            normal,
            bounds,
        }
    }
    pub fn group(&self, parent: Option<&str>) -> usize {
        self.parents.get(&parent).copied().unwrap_or(usize::MAX)
    }
    /// Isolated layer colors omit layer opacity/mask, matching Document::preview.
    pub fn layer(&self, index: usize, x: i32, y: i32) -> Pixel {
        self.raw(index, x, y)
    }
    pub fn sample(&self, group: usize, x: i32, y: i32) -> Pixel {
        let Some(children) = self.groups.get(group) else {
            return [0; 4];
        };
        let mut out = [0; 4];
        for &i in children {
            if matches!(self.kinds[i], Kind::Adjustment) {
                if let Some(image) = &self.colors[i] {
                    out = backdrop_adjust(
                        out,
                        image.get(x, y),
                        self.amount(i, x, y),
                        &self.doc.layers[i].blend,
                    );
                }
            } else {
                if !self.bounds[i].is_some_and(|b| x >= b[0] && y >= b[1] && x < b[2] && y < b[3]) {
                    // A transparent unit still clears hidden RGB when the rounded alpha is zero.
                    if out[3] == 0 {
                        out = [0; 4];
                    }
                    continue;
                }
                out = compose(
                    out,
                    self.unit(i, x, y),
                    self.amount(i, x, y),
                    &self.doc.layers[i].blend,
                    self.normal[i],
                );
            }
        }
        out
    }
    fn amount(&self, i: usize, x: i32, y: i32) -> f32 {
        let layer = &self.doc.layers[i];
        if layer.mask.as_ref().is_none_or(|mask| !mask.enabled) {
            layer.opacity
        } else {
            layer.opacity
                * layer.mask_value_prepared(
                    x - layer.x,
                    y - layer.y,
                    self.masks[i].as_deref(),
                    false,
                )
        }
    }
    fn raw(&self, i: usize, x: i32, y: i32) -> Pixel {
        let layer = &self.doc.layers[i];
        let lx = x - layer.x;
        let ly = y - layer.y;
        if !matches!(self.kinds[i], Kind::Adjustment)
            && !self.bounds[i].is_some_and(|b| x >= b[0] && y >= b[1] && x < b[2] && y < b[3])
        {
            return [0; 4];
        }
        if let Some(image) = &self.colors[i] {
            return if matches!(self.kinds[i], Kind::Group(_) | Kind::Adjustment) {
                image.get(x, y)
            } else {
                image.get(lx, ly)
            };
        }
        match self.kinds[i] {
            Kind::Group(group) => self.sample(group, x, y),
            Kind::Fill
                if lx >= 0
                    && ly >= 0
                    && lx < layer.pixels.width as i32
                    && ly < layer.pixels.height as i32 =>
            {
                layer.color
            }
            Kind::Fill | Kind::Adjustment => [0; 4],
            Kind::Raster => layer.pixels.get(lx, ly),
        }
    }
    fn unit(&self, base: usize, x: i32, y: i32) -> Pixel {
        let mut pixel = self.raw(base, x, y);
        for &i in &self.clips[base] {
            let mut source = self.raw(i, x, y);
            let t = self.amount(i, x, y);
            if matches!(self.kinds[i], Kind::Adjustment) {
                if self.colors[i].is_none() {
                    continue;
                }
                pixel = color_adjust(pixel, source, t, &self.doc.layers[i].blend);
            } else {
                let alpha = pixel[3];
                let mut opaque = pixel;
                opaque[3] = 255;
                source = compose(opaque, source, t, &self.doc.layers[i].blend, self.normal[i]);
                pixel = [source[0], source[1], source[2], alpha];
            }
        }
        pixel
    }
}
// These identities preserve straight-alpha bytes, including hidden RGB at zero rounded alpha.
fn compose(dst: Pixel, src: Pixel, opacity: f32, mode: &str, normal: bool) -> Pixel {
    if src[3] == 0 || opacity == 0.0 {
        return if dst[3] == 0 { [0; 4] } else { dst };
    }
    if opacity == 1.0 && ((normal && src[3] == 255) || dst[3] == 0) {
        return src;
    }
    blend(dst, src, opacity, mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        effects::Effect,
        engine::{id, Layer, Mask, MaskStep},
        raster::{Raster, BLENDS},
    };
    use serde_json::json;
    #[test]
    fn compiled_plan_matches_reference_for_nested_clipping_adjustments_masks_and_all_blends() {
        for case in 0..48 {
            let mut doc = Document::new(9, 7).unwrap();
            let folder = Layer::new("Folder", "group", 9, 7);
            let mut child = Layer::new("Child", "paint", 9, 7);
            child.parent = Some(folder.id.clone());
            child.x = -2;
            child.y = 1;
            let mut base = Layer::new("Base", "paint", 9, 7);
            base.opacity = [0.0, 0.003, 0.37, 1.0][case % 4];
            base.blend = BLENDS[case % BLENDS.len()].0.into();
            let mut clipped = Layer::new("Clipped", "paint", 9, 7);
            clipped.clip_to = Some(base.id.clone());
            clipped.opacity = 0.63;
            clipped.blend = BLENDS[(case + 3) % BLENDS.len()].0.into();
            let mut background = Layer::new("Backdrop", "fill", 9, 7);
            background.color = [30, 150, 230, if case % 3 == 0 { 0 } else { 255 }];
            for layer in [&mut base, &mut clipped, &mut child] {
                for y in 0..7 {
                    for x in 0..9 {
                        layer.pixels.set(
                            x,
                            y,
                            [
                                (x * 23) as u8,
                                (y * 31) as u8,
                                183,
                                if (x + y + case as i32) % 3 == 0 {
                                    0
                                } else {
                                    (x * 19 + y * 9 + 11) as u8
                                },
                            ],
                        );
                    }
                }
            }
            base.mask = Some(Mask {
                enabled: case % 5 != 0,
                cache_key: id(),
                steps: vec![MaskStep {
                    id: id(),
                    kind: "fill".into(),
                    enabled: true,
                    value: 113.,
                    pixels: Raster::new(9, 7),
                    settings: json!({}),
                }],
            });
            if case % 2 == 0 {
                child.effects.push(Effect {
                    id: id(),
                    kind: "invert".into(),
                    enabled: true,
                    settings: json!({}),
                });
            }
            let mut adjustment = Layer::new("Adjustment", "adjustment", 9, 7);
            adjustment.opacity = 0.3;
            adjustment.blend = BLENDS[(case + 9) % BLENDS.len()].0.into();
            adjustment.effects.push(Effect {
                id: id(),
                kind: "adjust".into(),
                enabled: true,
                settings: json!({"brightness":0.15}),
            });
            if case % 4 == 0 {
                adjustment.clip_to = Some(base.id.clone());
            }
            doc.layers = vec![adjustment, clipped, child, folder, base, background];
            doc.layers[3].visible = case % 7 != 0;
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
            let plan = Plan::new(&doc, &masks, &colors);
            for parent in [None, Some(doc.layers[3].id.as_str())] {
                for y in -1..9 {
                    for x in -3..12 {
                        assert_eq!(
                            plan.sample(plan.group(parent), x, y),
                            sample_group(&doc, parent, x, y, &masks, &colors),
                            "case {case}, parent {parent:?}, ({x},{y})"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn fast_alpha_identities_match_reference_for_all_channel_and_alpha_bytes() {
        for &(mode, _) in BLENDS {
            for c in 0..=255 {
                for alpha in [0, 1, 127, 255] {
                    let dst = [c, 255 - c, 37, alpha];
                    let src = [255 - c, c, 197, alpha];
                    for opacity in [0.0, 0.003, 0.5, 1.0] {
                        assert_eq!(
                            compose(dst, src, opacity, mode, mode == "normal"),
                            blend(dst, src, opacity, mode)
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn adjustment_blend_modes_affect_color_and_keep_original_alpha() {
        let base = [200, 110, 50, 137];
        let adjusted = [120, 220, 190, 137];
        let normal = backdrop_adjust(base, adjusted, 0.6, "normal");
        for mode in ["multiply", "screen", "overlay", "difference"] {
            let actual = backdrop_adjust(base, adjusted, 0.6, mode);
            assert_ne!(actual, normal, "adjustment {mode} ignored");
            assert_eq!(actual[3], base[3]);
            let expected = blend(
                [base[0], base[1], base[2], 255],
                [adjusted[0], adjusted[1], adjusted[2], 255],
                0.6,
                mode,
            );
            assert_eq!(&actual[..3], &expected[..3]);
        }
    }
    #[test]
    fn sparse_tile_and_folder_envelopes_keep_off_canvas_and_empty_regions_exact() {
        let mut doc = Document::new(768, 512).unwrap();
        let folder = Layer::new("Folder", "group", 768, 512);
        let mut child = Layer::new("Sparse child", "paint", 768, 512);
        child.parent = Some(folder.id.clone());
        child.x = -17;
        child.y = 9;
        child.pixels.set(520, 300, [170, 220, 80, 200]);
        let tile = child.pixels.tiles.values().next().unwrap().clone();
        child.pixels.tiles.insert((u32::MAX, u32::MAX), tile);
        let mut backdrop = Layer::new("Backdrop", "fill", 768, 512);
        backdrop.color = [30, 50, 80, 255];
        doc.layers = vec![child, folder, backdrop];
        let masks = vec![None; 3];
        let colors = vec![None; 3];
        let plan = Plan::new(&doc, &masks, &colors);
        for parent in [None, Some(doc.layers[1].id.as_str())] {
            for y in [0, 8, 9, 255, 309, 510, 520] {
                for x in [-18, -17, 0, 255, 502, 503, 520, 767, 800] {
                    assert_eq!(
                        plan.sample(plan.group(parent), x, y),
                        sample_group(&doc, parent, x, y, &masks, &colors)
                    );
                }
            }
        }
        assert_eq!(plan.layer(0, 503, 309), [170, 220, 80, 200]);
    }
}
