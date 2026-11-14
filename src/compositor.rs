//! Shared straight-alpha composition with clipping and backdrop-aware folders.
pub(crate) mod native;
pub(crate) mod region;
use crate::{
    effects::Image,
    engine::Document,
    mask::GrayMask,
    psd::{blend_channels, blend_ranges::{self, Ranges}},
    raster::{blend, fill_blend::SpecialFill, Pixel},
};
use std::sync::Arc;
type Masks = [Option<Arc<GrayMask>>];
type Colors = [Option<Arc<Image>>];

pub(crate) fn passes_backdrop(blend: &str, has_ranges: bool) -> bool {
    // Blend If evaluates one isolated result even when the saved mode is Pass Through.
    blend == "pass_through" && !has_ranges
}

pub(crate) fn folder_merge_blend(blend: &str) -> &str {
    if blend == "pass_through" { "normal" } else { blend }
}

fn layer_blend(blend: &str, photoshop_multiply: bool, paint: bool, unclipped: bool) -> &str {
    if photoshop_multiply && paint && unclipped && blend == "multiply" {
        crate::raster::PHOTOSHOP_RGB8_MULTIPLY
    } else {
        folder_merge_blend(blend)
    }
}

fn raw(doc: &Document, i: usize, x: i32, y: i32, masks: &Masks, colors: &Colors) -> Pixel {
    let l = &doc.layers[i];
    if let Some(image) = &colors[i] {
        return if crate::effects::canvas_frame(l) {
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
fn amount(doc: &Document, i: usize, x: i32, y: i32, masks: &Masks) -> (f32, Option<SpecialFill>) {
    let l = &doc.layers[i];
    let (opacity, fill) = crate::psd::fill_opacity::render(l.opacity, &l.blend, &l.psd_metadata);
    (
        opacity as f32 * l.mask_value_prepared(x - l.x, y - l.y, masks[i].as_deref(), false),
        fill,
    )
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
fn gated_amount(ranges: Option<Ranges>, dst: Pixel, src: Pixel, amount: f32) -> f32 {
    ranges.map_or(amount, |ranges| {
        amount * ranges.coverage(dst.map(u16::from), src.map(u16::from), 255) as f32
    })
}
pub(crate) fn clip_fill_factor(fill: Option<SpecialFill>) -> f64 {
    fill.map_or(1., |fill| f64::from(fill.amount()) / 255.)
}
pub(crate) fn restore_clip_coverage(visible: f64, alpha: u16, max: u16, amount: f64, fill: Option<SpecialFill>) -> f64 {
    let coverage = f64::from(alpha) / f64::from(max) * amount * clip_fill_factor(fill);
    visible + (1. - visible) * coverage
}
fn unit(
    doc: &Document,
    base: usize,
    above: usize,
    x: i32,
    y: i32,
    masks: &Masks,
    colors: &Colors,
    outside: Pixel,
) -> (Pixel, f64) {
    let l = &doc.layers[base];
    if !l.visible {
        return ([0; 4], 1.);
    }
    let mut p = raw(doc, base, x, y, masks, colors);
    let mut visible = blend_ranges::ranges(&l.psd_metadata).map_or(1., |ranges|
        ranges.coverage(outside.map(u16::from), p.map(u16::from), 255));
    if visible == 0. {
        p[..3].fill(0);
    }
    for i in (above..base).rev() {
        let clip = &doc.layers[i];
        if clip.parent != l.parent || clip.clip_to.as_deref() != Some(&l.id) || !clip.visible {
            continue;
        }
        let excluded = blend_channels::excluded(&clip.psd_metadata);
        if excluded == blend_channels::ALL {
            continue;
        }
        let (before, before_visible) = (p, visible);
        let mut src = raw(doc, i, x, y, masks, colors);
        let (t, fill) = amount(doc, i, x, y, masks);
        let t = blend_ranges::ranges(&clip.psd_metadata).map_or(t, |ranges| {
            let (base_amount, base_fill) = amount(doc, base, x, y, masks);
            let scale = f64::from(base_amount * clip_fill_factor(base_fill) as f32);
            t * ranges.coverage_scaled(p.map(u16::from), src.map(u16::from), 255, scale * visible) as f32
        });
        if clip.kind == "adjustment" {
            if colors[i].is_none() || visible == 0. {
                continue;
            }
            p = color_adjust(p, src, t, &clip.blend);
        } else {
            if visible != 1. {
                visible = restore_clip_coverage(visible, u16::from(src[3]), 255, f64::from(t), fill);
            }
            let alpha = p[3];
            let mut opaque = p;
            opaque[3] = 255;
            src = if let Some(fill) = fill {
                fill.compose8(opaque, src, t)
            } else {
                blend(opaque, src, t, &clip.blend)
            };
            p = [src[0], src[1], src[2], alpha];
        }
        p = blend_channels::keep_clipped8(before, p, excluded, before_visible, visible);
    }
    (p, visible)
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
    below_on(doc, parent, above, x, y, masks, colors, ([0; 4], 0))
}
#[allow(clippy::too_many_arguments)]
fn below_on(
    doc: &Document,
    parent: Option<&str>,
    above: usize,
    x: i32,
    y: i32,
    masks: &Masks,
    colors: &Colors,
    backdrop: (Pixel, u8),
) -> Pixel {
    let (mut out, inherited) = backdrop;
    for i in (above..doc.layers.len()).rev() {
        let l = &doc.layers[i];
        if l.parent.as_deref() != parent || !l.visible || l.clip_to.is_some() {
            continue;
        }
        let excluded = inherited | blend_channels::excluded(&l.psd_metadata);
        if excluded == blend_channels::ALL {
            continue;
        }
        let before = out;
        if l.kind == "group" && passes_backdrop(&l.blend, blend_ranges::bytes(&l.psd_metadata).is_some()) {
            let result = below_on(doc, Some(&l.id), 0, x, y, masks, colors, (out, excluded));
            out = mix(out, result, amount(doc, i, x, y, masks).0);
        } else if l.kind == "adjustment" {
            if let Some(image) = &colors[i] {
                let source = image.get(x, y);
                let amount = gated_amount(blend_ranges::ranges(&l.psd_metadata), out, source, amount(doc, i, x, y, masks).0);
                out = backdrop_adjust(out, source, amount, &l.blend);
            }
        } else {
            let (source, visible) = unit(doc, i, above, x, y, masks, colors, out);
            let (amount, fill) = amount(doc, i, x, y, masks);
            let amount = amount * visible as f32;
            out = if let Some(fill) = fill {
                fill.compose8(out, source, amount)
            } else {
                let mode = layer_blend(&l.blend, doc.psd_rgb8_multiply, l.kind == "paint", l.clip_to.is_none());
                blend(out, source, amount, mode)
            };
        }
        out = blend_channels::keep8(before, out, excluded);
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
        let ranged = blend_ranges::bytes(&doc.layers[base].psd_metadata).is_some();
        let outside = if ranged {
            below_on(doc, l.parent.as_deref(), base + 1, x, y, masks, colors,
                group_backdrop(doc, l.parent.as_deref(), x, y, masks, colors))
        } else {
            [0; 4]
        };
        let (mut pixel, visible) = unit(doc, base, index + 1, x, y, masks, colors, outside);
        if ranged {
            let scale = f64::from(amount(doc, base, x, y, masks).0) * visible;
            pixel[3] = (f64::from(pixel[3]) * scale).round() as u8;
        }
        pixel
    } else {
        below_on(
            doc,
            l.parent.as_deref(),
            index + 1,
            x,
            y,
            masks,
            colors,
            group_backdrop(doc, l.parent.as_deref(), x, y, masks, colors),
        )
    }
}

fn group_backdrop(
    doc: &Document,
    parent: Option<&str>,
    x: i32,
    y: i32,
    masks: &Masks,
    colors: &Colors,
) -> (Pixel, u8) {
    let Some((i, folder)) = doc
        .layers
        .iter()
        .enumerate()
        .find(|(_, l)| Some(l.id.as_str()) == parent)
    else {
        return ([0; 4], 0);
    };
    if !passes_backdrop(&folder.blend, blend_ranges::bytes(&folder.psd_metadata).is_some()) {
        return ([0; 4], 0);
    }
    let backdrop = group_backdrop(doc, folder.parent.as_deref(), x, y, masks, colors);
    let out = below_on(
        doc,
        folder.parent.as_deref(),
        i + 1,
        x,
        y,
        masks,
        colors,
        backdrop,
    );
    (out, backdrop.1 | blend_channels::excluded(&folder.psd_metadata))
}

/// Prepare lower sibling subtrees before an adjustment that reads their backdrop.
/// A depth-only sort cannot honor dependencies across pass-through boundaries.
pub(crate) fn effect_order(doc: &Document) -> Vec<usize> {
    fn visit(doc: &Document, parent: Option<&str>, out: &mut Vec<usize>) {
        for (i, layer) in doc
            .layers
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, l)| l.parent.as_deref() == parent)
        {
            if layer.kind == "group" {
                visit(doc, Some(&layer.id), out);
            }
            out.push(i);
        }
    }
    let mut out = Vec::with_capacity(doc.layers.len());
    visit(doc, None, &mut out);
    out
}

pub fn validate_clipping(doc: &Document) -> Result<(), String> {
    for (i, l) in doc.layers.iter().enumerate() {
        if blend_channels::excluded(&l.psd_metadata) != 0 {
            blend_channels::validate_layer(&l.psd_metadata, &l.kind, &l.blend,
                l.effects.iter().map(|effect| effect.kind.as_str()),
                !blend_channels::supports_clip_base(&l.kind, l.effects.iter().map(|effect| effect.kind.as_str()),
                    crate::psd::clipping::grouped(&l.psd_metadata))
                    && doc.layers.iter().any(|node| node.clip_to.as_deref() == Some(&l.id)))?;
        }
        crate::psd::blend_ranges::validate_layer(
            &l.psd_metadata, &l.kind, &l.blend, l.effects.iter().map(|effect| effect.enabled), l.clip_to.is_some(),
            blend_ranges::bytes(&l.psd_metadata).is_some()
                && !blend_ranges::supports_clip_base(&l.kind, &l.blend, !l.effects.is_empty(),
                    crate::psd::clipping::grouped(&l.psd_metadata), crate::psd::fill_opacity::value(&l.psd_metadata))
                && doc.layers.iter().any(|node| node.clip_to.as_deref() == Some(&l.id)),
        )?;
        crate::psd::fill_opacity::validate(
            crate::psd::fill_opacity::value(&l.psd_metadata), &l.kind, &l.blend, l.effects.iter().map(|effect| effect.enabled),
        )?;
        if !crate::psd::clipping::grouped(&l.psd_metadata)
            && doc.layers.iter().any(|node| node.clip_to.as_deref() == Some(&l.id))
        {
            crate::psd::clipping::validate_base(&l.psd_metadata, &l.kind, &l.blend, !l.effects.is_empty())?;
        }
        if l.blend == "pass_through" {
            if l.kind != "group" {
                return Err("Pass Through is available only for folders".into());
            }
            if l.effects.iter().any(|e| e.enabled) {
                return Err(
                    "Choose an isolated folder blend before enabling folder effects".into(),
                );
            }
            if l.clip_to.is_some()
                || doc
                    .layers
                    .iter()
                    .any(|n| n.clip_to.as_deref() == Some(&l.id))
            {
                return Err("Release folder clipping before choosing Pass Through".into());
            }
        }
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
            blend_ranges::validate_clipped_member(
                &l.psd_metadata, &l.kind, &base.kind, !base.effects.is_empty(),
                crate::psd::clipping::grouped(&base.psd_metadata),
            )?;
            blend_channels::validate_clipped_member(
                &l.psd_metadata, &l.kind, &base.kind, base.effects.iter().map(|effect| effect.kind.as_str()),
                crate::psd::clipping::grouped(&base.psd_metadata),
            )?;
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
    opacity: Vec<(f32, Option<SpecialFill>, Option<Ranges>, u8)>,
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
            .map(|layer| folder_merge_blend(&layer.blend) == "normal")
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
                let [x, y] = if crate::effects::canvas_frame(layer) {
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
            opacity: doc.layers.iter().map(|layer| {
                let (opacity, fill) = crate::psd::fill_opacity::render(layer.opacity, &layer.blend, &layer.psd_metadata);
                (opacity as f32, fill, blend_ranges::ranges(&layer.psd_metadata), blend_channels::excluded(&layer.psd_metadata))
            }).collect(),
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
        self.sample_on(group, x, y, ([0; 4], 0))
    }
    fn sample_on(&self, group: usize, x: i32, y: i32, backdrop: (Pixel, u8)) -> Pixel {
        let (mut out, inherited) = backdrop;
        let Some(children) = self.groups.get(group) else {
            return out;
        };
        for &i in children {
            let excluded = inherited | self.opacity[i].3;
            if excluded == blend_channels::ALL {
                continue;
            }
            let before = out;
            if let Kind::Group(children) = self.kinds[i] {
                if passes_backdrop(&self.doc.layers[i].blend, self.opacity[i].2.is_some()) {
                    out = mix(
                        out,
                        self.sample_on(children, x, y, (out, excluded)),
                        self.amount(i, x, y),
                    );
                    out = blend_channels::keep8(before, out, excluded);
                    continue;
                }
            }
            if matches!(self.kinds[i], Kind::Adjustment) {
                if let Some(image) = &self.colors[i] {
                    let source = image.get(x, y);
                    let amount = gated_amount(self.opacity[i].2, out, source, self.amount(i, x, y));
                    out = backdrop_adjust(out, source, amount, &self.doc.layers[i].blend);
                }
            } else {
                if !self.bounds[i].is_some_and(|b| x >= b[0] && y >= b[1] && x < b[2] && y < b[3]) {
                    // A transparent unit still clears hidden RGB when the rounded alpha is zero.
                    if out[3] == 0 {
                        out = [0; 4];
                    }
                    continue;
                }
                let (source, visible) = self.unit(i, x, y, out);
                let amount = self.amount(i, x, y) * visible as f32;
                out = self.compose_layer(i, out, source, amount);
            }
            out = blend_channels::keep8(before, out, excluded);
        }
        out
    }
    fn compose_layer(&self, i: usize, dst: Pixel, src: Pixel, amount: f32) -> Pixel {
        if let Some(fill) = self.opacity[i].1 {
            fill.compose8(dst, src, amount)
        } else {
            let layer = &self.doc.layers[i];
            let mode = layer_blend(&layer.blend, self.doc.psd_rgb8_multiply, layer.kind == "paint", layer.clip_to.is_none());
            compose(dst, src, amount, mode, self.normal[i])
        }
    }
    fn amount(&self, i: usize, x: i32, y: i32) -> f32 {
        let layer = &self.doc.layers[i];
        if layer.mask.as_ref().is_none_or(|mask| !mask.enabled) {
            self.opacity[i].0
        } else {
            self.opacity[i].0
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
            return if crate::effects::canvas_frame(layer) {
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
    fn unit(&self, base: usize, x: i32, y: i32, outside: Pixel) -> (Pixel, f64) {
        let mut pixel = self.raw(base, x, y);
        let mut visible = self.opacity[base].2.map_or(1., |ranges|
            ranges.coverage(outside.map(u16::from), pixel.map(u16::from), 255));
        if visible == 0. {
            pixel[..3].fill(0);
        }
        for &i in &self.clips[base] {
            let excluded = self.opacity[i].3;
            if excluded == blend_channels::ALL {
                continue;
            }
            let (before, before_visible) = (pixel, visible);
            let mut source = self.raw(i, x, y);
            let t = self.amount(i, x, y);
            let t = self.opacity[i].2.map_or(t, |ranges| {
                let scale = f64::from(self.amount(base, x, y) * clip_fill_factor(self.opacity[base].1) as f32);
                t * ranges.coverage_scaled(pixel.map(u16::from), source.map(u16::from), 255, scale * visible) as f32
            });
            if matches!(self.kinds[i], Kind::Adjustment) {
                if self.colors[i].is_none() || visible == 0. {
                    continue;
                }
                pixel = color_adjust(pixel, source, t, &self.doc.layers[i].blend);
            } else {
                if visible != 1. {
                    visible = restore_clip_coverage(visible, u16::from(source[3]), 255, f64::from(t), self.opacity[i].1);
                }
                let alpha = pixel[3];
                let mut opaque = pixel;
                opaque[3] = 255;
                source = self.compose_layer(i, opaque, source, t);
                pixel = [source[0], source[1], source[2], alpha];
            }
            pixel = blend_channels::keep_clipped8(before, pixel, excluded, before_visible, visible);
        }
        (pixel, visible)
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
                linked: true,
                enabled: case % 5 != 0,
                cache_key: id(),
                steps: vec![MaskStep {
                    weight: 1.0,
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
                    weight: 1.0,
                    id: id(),
                    kind: "invert".into(),
                    enabled: true,
                    settings: json!({}).into(),
                });
            }
            let mut adjustment = Layer::new("Adjustment", "adjustment", 9, 7);
            adjustment.opacity = 0.3;
            adjustment.blend = BLENDS[(case + 9) % BLENDS.len()].0.into();
            adjustment.effects.push(Effect {
                weight: 1.0,
                id: id(),
                kind: "adjust".into(),
                enabled: true,
                settings: json!({"brightness":0.15}).into(),
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
