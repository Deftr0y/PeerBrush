//! Compiled native-16 topology; source samples never pass through an 8-bit projection.
use super::{Gray16 as GrayMask, Image16 as Image};
use crate::{
    compositor::{clip_fill_factor, folder_merge_blend, passes_backdrop, restore_clip_coverage},
    engine::Document,
    psd::blend_channels,
    raster::{blend16 as blend, fill_blend::SpecialFill, Pixel16 as Pixel},
};
use std::sync::Arc;
type Masks = [Option<Arc<GrayMask>>];
type Colors = [Option<Arc<Image>>];
fn mix(a: Pixel, b: Pixel, t: f64) -> Pixel {
    let aa = a[3] as f64 / 65535.;
    let ba = b[3] as f64 / 65535.;
    let alpha = aa * (1. - t) + ba * t;
    if alpha <= 0. {
        return [0; 4];
    }
    if a[3] == b[3] {
        return color_adjust(a, b, t, "normal");
    }
    let mut out = [0, 0, 0, (alpha * 65535.).round() as u16];
    for c in 0..3 {
        out[c] = ((a[c] as f64 * aa * (1. - t) + b[c] as f64 * ba * t) / alpha)
            .round()
            .clamp(0., 65535.) as u16;
    }
    out
}
fn color_adjust(a: Pixel, b: Pixel, t: f64, mode: &str) -> Pixel {
    if mode == "normal" {
        let mut out = a;
        for c in 0..3 {
            out[c] = (a[c] as f64 * (1.0 - t) + b[c] as f64 * t)
                .round()
                .clamp(0.0, 65535.0) as u16;
        }
        return out;
    }
    let mut dst = a;
    let mut src = b;
    dst[3] = 65535;
    src[3] = 65535;
    let out = blend(dst, src, t, mode);
    [out[0], out[1], out[2], a[3]]
}
fn backdrop_adjust(a: Pixel, b: Pixel, t: f64, mode: &str) -> Pixel {
    if mode == "normal" {
        mix(a, b, t)
    } else {
        color_adjust(a, b, t, mode)
    }
}
#[derive(Clone, Copy)]
enum Kind {
    Raster,
    Fill,
    Group(usize),
    Adjustment,
}
/// Immutable composition topology, compiled once instead of filtering every layer per pixel.
pub struct Plan16<'a> {
    doc: &'a Document,
    masks: Vec<Option<Arc<GrayMask>>>,
    colors: Vec<Option<Arc<Image>>>,
    groups: Vec<Vec<usize>>,
    parents: std::collections::HashMap<Option<&'a str>, usize>,
    kinds: Vec<Kind>,
    clips: Vec<Vec<usize>>,
    normal: Vec<bool>,
    opacity: Vec<(f64, Option<SpecialFill>, Option<crate::psd::blend_ranges::Ranges>, u8)>,
    bounds: Vec<Option<[i32; 4]>>,
}
impl<'a> Plan16<'a> {
    pub fn new(doc: &'a Document) -> Result<Self, String> {
        Self::new_checked(doc, &|| Ok(()))
    }
    pub(crate) fn new_checked(doc: &'a Document, check: &crate::render::Check<'_>) -> Result<Self, String> {
        super::validate_budget(doc)?;
        let masks = super::prepare_masks_checked(doc, check)?;
        let colors = super::prepare_with_masks_checked(doc, &masks, check)?;
        Ok(Self::with_prepared(doc, &masks, &colors))
    }
    pub fn pixel(&self, x: i32, y: i32) -> Pixel {
        self.sample(self.group(None), x, y)
    }
    pub fn mask(&self, index: usize, x: i32, y: i32, raw: bool) -> f64 {
        let layer = &self.doc.layers[index];
        super::mask::value(
            layer,
            x - layer.x,
            y - layer.y,
            self.masks[index].as_deref(),
            raw,
        )
    }
    pub fn adjustment_input(&self, index: usize, x: i32, y: i32) -> Pixel {
        let layer = &self.doc.layers[index];
        if let Some(base) = layer
            .clip_to
            .as_deref()
            .and_then(|id| self.doc.layers.iter().position(|l| l.id == id))
        {
            let ranged = self.opacity[base].2.is_some();
            let outside = if ranged {
                self.sample_below(self.group(layer.parent.as_deref()), base + 1, x, y,
                    self.group_backdrop(layer.parent.as_deref(), x, y))
            } else {
                [0; 4]
            };
            let (mut pixel, visible) = self.unit_from(base, index + 1, x, y, outside);
            if ranged {
                let scale = self.amount(base, x, y) * visible;
                pixel[3] = (f64::from(pixel[3]) * scale).round() as u16;
            }
            pixel
        } else {
            self.sample_below(
                self.group(layer.parent.as_deref()),
                index + 1,
                x,
                y,
                self.group_backdrop(layer.parent.as_deref(), x, y),
            )
        }
    }

    fn group_backdrop(&self, parent: Option<&str>, x: i32, y: i32) -> (Pixel, u8) {
        let Some((i, folder)) = self
            .doc
            .layers
            .iter()
            .enumerate()
            .find(|(_, l)| Some(l.id.as_str()) == parent)
        else {
            return ([0; 4], 0);
        };
        if !passes_backdrop(&folder.blend, self.opacity[i].2.is_some()) {
            return ([0; 4], 0);
        }
        let backdrop = self.group_backdrop(folder.parent.as_deref(), x, y);
        let out = self.sample_below(
            self.group(folder.parent.as_deref()),
            i + 1,
            x,
            y,
            backdrop,
        );
        (out, backdrop.1 | self.opacity[i].3)
    }

    pub fn with_prepared(doc: &'a Document, masks: &Masks, colors: &Colors) -> Self {
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
            masks: masks.to_vec(),
            colors: colors.to_vec(),
            groups,
            parents,
            kinds,
            clips,
            normal,
            opacity: doc.layers.iter().map(|layer| {
                let (opacity, fill) = crate::psd::fill_opacity::render(layer.opacity, &layer.blend, &layer.psd_metadata);
                (opacity, fill, crate::psd::blend_ranges::ranges(&layer.psd_metadata), blend_channels::excluded(&layer.psd_metadata))
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
        self.sample_below(group, 0, x, y, ([0; 4], 0))
    }
    fn sample_below(&self, group: usize, above: usize, x: i32, y: i32, backdrop: (Pixel, u8)) -> Pixel {
        let (mut out, inherited) = backdrop;
        let Some(children) = self.groups.get(group) else {
            return out;
        };
        for &i in children {
            if i < above {
                continue;
            }
            let excluded = inherited | self.opacity[i].3;
            if excluded == blend_channels::ALL {
                continue;
            }
            let before = out;
            if let Kind::Group(children) = self.kinds[i] {
                if passes_backdrop(&self.doc.layers[i].blend, self.opacity[i].2.is_some()) {
                    out = mix(
                        out,
                        self.sample_below(children, 0, x, y, (out, excluded)),
                        self.amount(i, x, y),
                    );
                    out = blend_channels::keep(before, out, excluded, 65535);
                    continue;
                }
            }
            if matches!(self.kinds[i], Kind::Adjustment) {
                if let Some(image) = &self.colors[i] {
                    let source = image.get(x, y);
                    let amount = self.gated_amount(i, out, source, self.amount(i, x, y));
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
                let (source, visible) = self.unit_from(i, above, x, y, out);
                let amount = self.amount(i, x, y) * visible;
                out = self.compose_layer(i, out, source, amount);
            }
            out = blend_channels::keep(before, out, excluded, 65535);
        }
        out
    }
    fn gated_amount(&self, i: usize, dst: Pixel, src: Pixel, amount: f64) -> f64 {
        self.opacity[i].2.map_or(amount, |ranges| amount * ranges.coverage(dst, src, 65535))
    }
    fn compose_layer(&self, i: usize, dst: Pixel, src: Pixel, amount: f64) -> Pixel {
        if let Some(fill) = self.opacity[i].1 {
            fill.compose(dst, src, amount, 65535)
        } else {
            compose(dst, src, amount, folder_merge_blend(&self.doc.layers[i].blend), self.normal[i])
        }
    }
    fn amount(&self, i: usize, x: i32, y: i32) -> f64 {
        let layer = &self.doc.layers[i];
        if layer.mask.as_ref().is_none_or(|mask| !mask.enabled) {
            self.opacity[i].0
        } else {
            self.opacity[i].0
                * super::mask::value(
                    layer,
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
                layer.color.map(|v| u16::from(v) * 257)
            }
            Kind::Fill | Kind::Adjustment => [0; 4],
            Kind::Raster => layer.pixels.get16(lx, ly),
        }
    }
    fn unit_from(&self, base: usize, above: usize, x: i32, y: i32, outside: Pixel) -> (Pixel, f64) {
        let mut pixel = self.raw(base, x, y);
        let mut visible = self.opacity[base].2.map_or(1., |ranges| ranges.coverage(outside, pixel, 65535));
        if visible == 0. {
            pixel[..3].fill(0);
        }
        for &i in &self.clips[base] {
            if i < above {
                continue;
            }
            let excluded = self.opacity[i].3;
            if excluded == blend_channels::ALL {
                continue;
            }
            let (before, before_visible) = (pixel, visible);
            let mut source = self.raw(i, x, y);
            let t = self.amount(i, x, y);
            let t = self.opacity[i].2.map_or(t, |ranges| {
                let scale = self.amount(base, x, y) * clip_fill_factor(self.opacity[base].1);
                t * ranges.coverage_scaled(pixel, source, 65535, scale * visible)
            });
            if matches!(self.kinds[i], Kind::Adjustment) {
                if self.colors[i].is_none() || visible == 0. {
                    continue;
                }
                pixel = color_adjust(pixel, source, t, &self.doc.layers[i].blend);
            } else {
                if visible != 1. {
                    visible = restore_clip_coverage(visible, source[3], 65535, t, self.opacity[i].1);
                }
                let alpha = pixel[3];
                let mut opaque = pixel;
                opaque[3] = 65535;
                source = self.compose_layer(i, opaque, source, t);
                pixel = [source[0], source[1], source[2], alpha];
            }
            pixel = blend_channels::keep_clipped(before, pixel, excluded, before_visible, visible);
        }
        (pixel, visible)
    }
}
// These identities preserve straight-alpha bytes, including hidden RGB at zero rounded alpha.
fn compose(dst: Pixel, src: Pixel, opacity: f64, mode: &str, normal: bool) -> Pixel {
    if src[3] == 0 || opacity == 0.0 {
        return if dst[3] == 0 { [0; 4] } else { dst };
    }
    if opacity == 1.0 && ((normal && src[3] == 65535) || dst[3] == 0) {
        return src;
    }
    blend(dst, src, opacity, mode)
}
