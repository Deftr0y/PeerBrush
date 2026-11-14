//! Bounded PSD v1 / PSB v2 codecs. Unsupported Photoshop features are never silently rewritten.
pub(crate) mod adjustments;
pub(crate) mod gradient_fill;
pub(crate) mod bulk36;
pub(crate) mod canonical36;
pub(crate) mod clipping;
mod composite;
mod gradient_map;
mod direct_layers;
pub(crate) mod blend_channels;
pub(crate) mod blend_ranges;
pub(crate) mod file_info;
pub(crate) mod fill_opacity;
pub(crate) mod container36;
pub(crate) mod global_metadata;
pub use global_metadata::GlobalMetadata;
pub(crate) mod import36;
mod indexed;
pub(crate) use indexed::{decode_backed, decode_protected, ConventionalComposite, DecodedBacked};
pub(crate) mod manifest36;
pub(crate) mod memory36;
pub(crate) mod native_rows36;
pub mod patterns;
pub(crate) mod read36;
mod row_stream;
pub(crate) mod save36;
pub(crate) mod scene_save36;
pub(crate) mod source_bulk36;
pub(crate) mod standard36;
mod structural;
pub(crate) use structural::{native_manifest_candidate, Format};
use adjustments::{ToneRecord, ToneRecords};
#[cfg(test)]
pub(crate) use structural::tests::fixture as protected_fixture;
mod styles;
mod vector_mask;
use crate::{
    engine::{id, Document, Layer, Mask, MaskStep},
    raster::{check_size, Raster},
};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};

pub(crate) fn mib_progress<'a>(
    progress: &'a dyn Fn(&str, usize, usize) -> Result<(), String>,
    stage: &'a str,
    total_bytes: u64,
) -> impl FnMut(u64) -> Result<(), String> + 'a {
    const MIB: u64 = 1024 * 1024;
    let total = total_bytes.div_ceil(MIB) as usize;
    let mut reported = 0;
    move |bytes| {
        let completed = if bytes == total_bytes {
            total
        } else {
            (bytes / MIB) as usize
        };
        if bytes == 0 || bytes == total_bytes || completed >= reported + 4 {
            progress(stage, completed, total)?;
            reported = completed;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Layout {
    Psd,
    Psb,
}
impl Layout {
    fn large(self) -> bool {
        self == Self::Psb
    }
    fn block<'a>(self, cursor: &mut Cursor<'a>) -> Result<&'a [u8], String> {
        let n = cursor.length(self.large())?;
        cursor.bytes(n)
    }
    fn write_length(self, out: &mut dyn Write, n: u64) -> Result<(), String> {
        if self.large() {
            out.write_all(&n.to_be_bytes()).map_err(|e| e.to_string())
        } else {
            let n = u32::try_from(n).map_err(|_| "PSD section length overflow")?;
            out.write_all(&n.to_be_bytes()).map_err(|e| e.to_string())
        }
    }
    fn tag_large(self, key: &[u8]) -> bool {
        self.large()
            && matches!(
                key,
                b"LMsk"
                    | b"Lr16"
                    | b"Lr32"
                    | b"Layr"
                    | b"Mt16"
                    | b"Mt32"
                    | b"Mtrn"
                    | b"Alph"
                    | b"FMsk"
                    | b"lnk2"
                    | b"FEid"
                    | b"FXid"
                    | b"PxSD"
                    | b"lnk3"
                    | b"lnkE"
                    | b"FELS"
                    | b"pths"
                    | b"extd"
                    | b"extn"
                    | b"cinf"
                    | b"artd"
            )
    }
    fn tag_block<'a>(self, cursor: &mut Cursor<'a>, key: &[u8]) -> Result<&'a [u8], String> {
        let n = cursor.length(self.tag_large(key))?;
        cursor.bytes(n)
    }
}

fn row_lengths<R: Read>(
    input: &mut R,
    count: usize,
    row_bytes: usize,
    layout: Layout,
) -> Result<Vec<usize>, String> {
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let mut bytes = [0; 4];
        let n = if layout.large() {
            input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            u32::from_be_bytes(bytes) as usize
        } else {
            input
                .read_exact(&mut bytes[..2])
                .map_err(|e| e.to_string())?;
            u16::from_be_bytes(bytes[..2].try_into().unwrap()) as usize
        };
        if layout.large() && n > row_bytes.saturating_mul(2).saturating_add(1024) {
            return Err("PSB RLE row exceeds the bounded decode budget".into());
        }
        result.push(n);
    }
    Ok(result)
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("PSD length overflow")?;
        let v = self.data.get(self.pos..end).ok_or("Truncated PSD")?;
        self.pos = end;
        Ok(v)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.bytes(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, String> {
        Ok(u16::from_be_bytes(self.bytes(2)?.try_into().unwrap()))
    }
    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_be_bytes(self.bytes(4)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }
    fn block(&mut self) -> Result<&'a [u8], String> {
        let n = self.u32()? as usize;
        self.bytes(n)
    }
    fn length(&mut self, large: bool) -> Result<usize, String> {
        let n = if large {
            u64::from_be_bytes(self.bytes(8)?.try_into().unwrap())
        } else {
            u64::from(self.u32()?)
        };
        usize::try_from(n).map_err(|_| "Photoshop section length overflow".into())
    }
}

const MAX_LAYER_NAME_UNITS: usize = 10_000;
const MAX_LAYER_NAME_BYTES: usize = 4 + MAX_LAYER_NAME_UNITS * 2 + 2;

fn unicode_layer_name(payload: &[u8], limit: usize) -> Result<(String, bool), String> {
    let mut cursor = Cursor::new(payload);
    let units = cursor.u32()? as usize;
    if units > MAX_LAYER_NAME_UNITS {
        return Err("Photoshop Unicode layer name is too long".into());
    }
    let (units, _) = cursor.bytes(units * 2)?.as_chunks::<2>();
    // Photoshop exports a terminator. Some writers include it in the count.
    if !matches!(&payload[cursor.pos..], [] | [0, 0]) {
        return Err("Unexpected data after the Photoshop Unicode layer name".into());
    }
    let units = units.strip_suffix(&[[0; 2]]).unwrap_or(units);
    let mut name = String::new();
    let mut used = 0;
    let mut truncated = false;
    for character in char::decode_utf16(units.iter().copied().map(u16::from_be_bytes)) {
        let character = character.map_err(|_| "Invalid UTF-16 in the Photoshop Unicode layer name")?;
        if character == '\0' {
            return Err("Unexpected NUL in the Photoshop Unicode layer name".into());
        }
        used += character.len_utf16();
        if used <= limit {
            name.push(character);
        } else {
            truncated = true;
        }
    }
    Ok((name, truncated))
}

fn layer_name_units(name: &str) -> Result<u32, String> {
    let units = name.encode_utf16().count();
    if units > MAX_LAYER_NAME_UNITS || name.contains('\0') {
        return Err("Photoshop layer names require at most 10000 UTF-16 units and cannot contain NUL".into());
    }
    Ok(units as u32)
}

fn legacy_layer_name(name: &str) -> &[u8] {
    &name.as_bytes()[..name.floor_char_boundary(usize::from(u8::MAX))]
}

fn u16b(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes())
}
fn u32b(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes())
}
fn i32b(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes())
}
fn block(out: &mut Vec<u8>, v: &[u8]) {
    u32b(out, v.len() as u32);
    out.extend_from_slice(v);
}
fn hash(bytes: &[u8]) -> u64 {
    let mut state = 0xcbf29ce484222325;
    hash_part(&mut state, bytes);
    state
}
fn hash_part(state: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *state = (*state ^ u64::from(byte)).wrapping_mul(0x100000001b3);
    }
}
fn tag(out: &mut Vec<u8>, key: &[u8; 4], v: &[u8]) {
    out.extend_from_slice(b"8BIM");
    out.extend_from_slice(key);
    block(out, v);
    if v.len() % 2 != 0 {
        out.push(0);
    }
}

fn blend_name(key: &[u8]) -> Option<&'static str> {
    Some(match key {
        b"norm" => "normal",
        b"pass" => "pass_through",
        b"mul " => "multiply",
        b"scrn" => "screen",
        b"over" => "overlay",
        b"dark" => "darken",
        b"lite" => "lighten",
        b"lddg" => "linear_dodge",
        b"lbrn" => "linear_burn",
        b"div " => "color_dodge",
        b"idiv" => "color_burn",
        b"hLit" => "hard_light",
        b"lLit" => "linear_light",
        b"vLit" => "vivid_light",
        b"pLit" => "pin_light",
        b"hMix" => "hard_mix",
        b"dkCl" => "darker_color",
        b"lgCl" => "lighter_color",
        b"hue " => "hue",
        b"sat " => "saturation",
        b"colr" => "color",
        b"lum " => "luminosity",
        b"sLit" => "soft_light",
        b"diff" => "difference",
        b"smud" => "exclusion",
        b"fsub" => "subtract",
        b"fdiv" => "divide",
        _ => return None,
    })
}

fn read_blend(key: &[u8], warnings: &mut Vec<String>) -> &'static str {
    blend_name(key).unwrap_or_else(|| {
        warnings.push(format!(
            "Unsupported Photoshop blend mode: {}",
            String::from_utf8_lossy(key)
        ));
        "normal"
    })
}

fn section_divider(signature: &[u8], payload: &[u8]) -> Result<(u32, Option<&'static str>), String> {
    if signature != b"8BIM" || !matches!(payload.len(), 4 | 12 | 16) {
        return Err("Invalid Photoshop section divider signature or length".into());
    }
    let section = u32::from_be_bytes(payload[..4].try_into().unwrap());
    if section > 3 {
        return Err("Unsupported Photoshop section divider type".into());
    }
    if payload.len() == 4 {
        return Ok((section, None));
    }
    if &payload[4..8] != b"8BIM" {
        return Err("Invalid Photoshop group blend signature".into());
    }
    if payload.len() == 16 && payload[12..] != [0; 4] {
        return Err("Photoshop animation scene groups are not supported".into());
    }
    let blend = blend_name(&payload[8..12]).ok_or("Unsupported Photoshop section divider blend mode")?;
    Ok((section, Some(blend)))
}

// The shared compositor implements Photoshop's default grouped raster clipping.
// More complicated units keep their standard baked appearance until supported.
fn standard_clipping(doc: &Document, clipped_invert: bool) -> bool {
    doc.layers.iter().all(|layer| {
        layer.clip_to.as_ref().is_none_or(|base| {
            (["paint", "fill"].contains(&layer.kind.as_str())
                || clipped_invert && layer.kind == "adjustment")
                && doc
                    .layers
                    .iter()
                    .any(|l| l.id == *base && ["paint", "fill"].contains(&l.kind.as_str()))
        })
    })
}

pub(crate) struct StandardPlan {
    pub composite: bool,
    pub hidden_sources: bool,
}
pub(crate) fn standard_plan(doc: &Document) -> StandardPlan {
    standard_plan_for(doc, true, true, ToneRecords::current(doc.bit_depth))
}

pub(crate) fn standard_styles<'a>(
    doc: &Document, layer: &'a Layer, section: u32, plan: &StandardPlan, patterns: &patterns::StandardPatterns,
) -> Option<styles::StandardStyles<'a>> {
    standard_style_candidate(doc, layer, section, plan)?.bind_patterns(patterns)
}

fn standard_style_candidate<'a>(
    doc: &Document, layer: &'a Layer, section: u32, plan: &StandardPlan,
) -> Option<styles::StandardStyles<'a>> {
    if section != 0 || plan.composite { return None; }
    let color = styles::standard_styles(layer.into(), doc.width, doc.height, doc.psd_global_angle.map(u32::from_be_bytes))?;
    (!doc.layers.iter().any(|child| child.clip_to.as_deref() == Some(layer.id.as_str())))
        .then_some(color)
}

fn tone_source_format(doc: &Document) -> Option<u32> {
    doc.layers.iter().flat_map(|layer| &layer.effects).chain(&doc.filters)
        .filter_map(|effect| {
            if effect.kind == crate::effects::color_lookup::KIND {
                Some(if effect.settings["lut_format"] == "look" { adjustments::LOOK_LOOKUP_FORMAT }
                    else if effect.settings.get("embed_profile").is_some() { adjustments::COMPILED_LOOKUP_FORMAT }
                    else if effect.settings.get("lut_format").is_some() { adjustments::TABLE_LOOKUP_FORMAT }
                    else if effect.settings.get("cube").is_some() { adjustments::CUBE_LOOKUP_FORMAT }
                    else { adjustments::COLOR_LOOKUP_FORMAT })
            } else if crate::effects::supports_tone_preset(&effect.kind) && effect.settings.get("preset").is_some() {
                Some(adjustments::TONE_PRESET_FORMAT)
            } else if crate::effects::levels::byte_composition(&effect.kind, &effect.settings) {
                Some(61)
            } else if crate::effects::curves::byte_composition(&effect.kind, &effect.settings) {
                Some(59)
            } else if crate::effects::curves::uses_photoshop(&effect.kind, &effect.settings) {
                Some(58)
            } else if effect.kind == "threshold" {
                Some(57)
            } else if effect.kind == crate::effects::legacy_brightness::KIND {
                Some(adjustments::LEGACY_BRIGHTNESS_FORMAT)
            } else if effect.kind == crate::effects::modern_brightness::KIND {
                Some(adjustments::MODERN_BRIGHTNESS_FORMAT)
            } else if effect.kind == crate::effects::exposure::KIND {
                Some(adjustments::EXPOSURE_FORMAT)
            } else if effect.kind == crate::effects::channel_mixer::KIND {
                Some(adjustments::CHANNEL_MIXER_FORMAT)
            } else if effect.kind == crate::effects::black_white::KIND {
                Some(adjustments::BLACK_WHITE_FORMAT)
            } else if effect.kind == crate::effects::selective_color::KIND {
                Some(adjustments::SELECTIVE_COLOR_FORMAT)
            } else if effect.kind == crate::effects::vibrance::KIND {
                Some(adjustments::VIBRANCE_FORMAT)
            } else if effect.kind == crate::effects::photo_filter::KIND {
                Some(adjustments::PHOTO_FILTER_FORMAT)
            } else if effect.kind == crate::effects::gradient_map::KIND {
                Some(adjustments::GRADIENT_MAP_FORMAT)
            } else if effect.kind == crate::effects::photoshop_color_balance::KIND {
                Some(adjustments::COLOR_BALANCE_FORMAT)
            } else if effect.kind == crate::effects::photoshop_hue_saturation::KIND {
                Some(adjustments::HUE_SATURATION_FORMAT)
            } else {
                crate::effects::posterize::source_version(&effect.kind, &effect.settings)
                    .map(|version| 54 + u32::from(version))
            }
        }).max()
}

fn standard_plan_for(doc: &Document, clipped_invert: bool, blended_invert: bool, tones: ToneRecords) -> StandardPlan {
    StandardPlan {
        composite: !doc.filters.is_empty()
            || doc.layers.iter().any(|layer| {
                layer.kind == "adjustment"
                    && ((!blended_invert && layer.blend != "normal")
                        || !(adjustments::standard_invert(layer.into(), doc.width, doc.height, clipped_invert)
                            || adjustments::standard_tone(layer.into(), doc.width, doc.height, clipped_invert, tones).is_some()))
            })
            || !standard_clipping(doc, clipped_invert),
        hidden_sources: !doc.filters.is_empty(),
    }
}
pub(crate) fn standard_order(doc: &Document) -> Vec<(&Layer, u32)> {
    fn collect<'a>(doc: &'a Document, parent: Option<&str>, out: &mut Vec<(&'a Layer, u32)>) {
        for layer in doc
            .layers
            .iter()
            .filter(|layer| layer.parent.as_deref() == parent)
        {
            if layer.kind == "group" && crate::effects::active(layer) {
                out.push((layer, 0));
            } else if layer.kind == "group" {
                out.push((layer, folder_section(&layer.psd_metadata)));
                collect(doc, Some(&layer.id), out);
                out.push((layer, 3));
            } else {
                out.push((layer, 0));
            }
        }
    }
    let mut out = vec![];
    collect(doc, None, &mut out);
    out
}
pub(crate) fn standard_bounds(
    doc: &Document, layer: &Layer, section: u32, overlay: Option<styles::StandardStyles<'_>>,
) -> [i32; 4] {
    if section != 0 {
        [0; 4]
    } else if let Some(overlay) = overlay {
        overlay.bounds
    } else if crate::effects::canvas_frame(layer) {
        [0, 0, doc.width as i32, doc.height as i32]
    } else {
        [
            layer.x,
            layer.y,
            layer.x + layer.pixels.width as i32,
            layer.y + layer.pixels.height as i32,
        ]
    }
}

#[derive(Clone, Copy)]
enum MaskOriginal<'a> {
    Resident(&'a Raster),
    Native(crate::engine::native::RasterMetadata<'a>),
}

struct ExtendedMask<'a> {
    bounds: [i32; 4],
    origin: [i32; 2],
    default: u8,
    original: MaskOriginal<'a>,
}

fn extended_mask(
    layer: crate::engine::view::LayerView<'_>,
    current: [i32; 4],
    before_density: bool,
) -> Option<ExtendedMask<'_>> {
    use crate::engine::view::MaskStepView;
    let mask = layer.mask()?;
    let (default, paint) = match mask.step_count().checked_sub(usize::from(before_density))? {
        1 => (255, mask.step(0)?),
        2 => {
            let fill = mask.step(0)?;
            let value = fill.value();
            if fill.kind() != "fill" || !fill.enabled() || fill.weight() != 1.
                || !(0. ..=255.).contains(&value) || value.fract() != 0.
            {
                return None;
            }
            (value as u8, mask.step(1)?)
        }
        _ => return None,
    };
    if paint.kind() != "paint" || !paint.enabled() || paint.weight() != 1. {
        return None;
    }
    let (matrix, shape, original) = match paint {
        MaskStepView::Resident(step) => {
            let retained = step.pixels.retained.as_deref()?;
            if retained.projective.is_some() || !retained.warps.is_empty() {
                return None;
            }
            (
                retained.matrix,
                [retained.pixels.width, retained.pixels.height],
                MaskOriginal::Resident(&retained.pixels),
            )
        }
        MaskStepView::Native(step) => {
            let retained = step.pixels().retained()?;
            if retained.projective().is_some() || !retained.warps().is_empty() {
                return None;
            }
            let pixels = retained.pixels();
            (retained.matrix(), [pixels.width(), pixels.height()], MaskOriginal::Native(pixels))
        }
    };
    let [a, b, c, d, dx, dy] = matrix;
    if [a, b, c, d] != [1., 0., 0., 1.]
        || [dx, dy].into_iter().any(|n| !n.is_finite() || n.abs() > 200_000. || n.fract() != 0.)
    {
        return None;
    }
    let [x, y] = layer.origin();
    let origin = [x.checked_add(dx as i32)?, y.checked_add(dy as i32)?];
    let bounds = [
        current[0].min(origin[0]),
        current[1].min(origin[1]),
        current[2].max(origin[0].checked_add(shape[0] as i32)?),
        current[3].max(origin[1].checked_add(shape[1] as i32)?),
    ];
    if bounds == current || bounds.iter().any(|v| v.unsigned_abs() > 100_000) {
        return None;
    }
    let limits = match original {
        MaskOriginal::Resident(_) => crate::raster::CanvasLimits::Resident,
        MaskOriginal::Native(_) => crate::raster::CanvasLimits::Native,
    };
    limits.check((bounds[2] - bounds[0]) as u32, (bounds[3] - bounds[1]) as u32).ok()?;
    Some(ExtendedMask { bounds, origin, default, original })
}

pub(crate) fn standard_mask_density(layer: crate::engine::view::LayerView<'_>) -> Option<u8> {
    use crate::engine::view::LayerView;
    let vector = match layer {
        LayerView::Resident(layer) => layer.psd_metadata.iter().any(|entry| entry.key == "vmsk"),
        LayerView::Native(layer) => layer.psd_metadata().iter().any(|entry| entry.key == "vmsk"),
    };
    if vector {
        return None;
    }
    let mask = layer.mask()?;
    let step = mask.step(mask.step_count().checked_sub(1)?)?;
    (step.kind() == "density" && step.enabled() && step.weight() == 1.
        && crate::engine::mask_parameter("density", f64::from(step.value())).is_ok()
        && step.settings().is_null()).then_some(step.value() as u8)
}

fn density_mask_default(mask: crate::engine::view::MaskView<'_>) -> u8 {
    // The imported default/paint/density stack has a standard background
    // sample even when its mask is fully in-frame.
    if mask.step_count() == 3
        && mask.step(1).is_some_and(|step| step.kind() == "paint" && step.enabled() && step.weight() == 1.)
        && mask.step(0).is_some_and(|step| step.kind() == "fill" && step.enabled()
            && step.weight() == 1. && step.value() == 0.)
    {
        0
    } else {
        255
    }
}

fn original_mask_sample(default: u8, [red, _, _, alpha]: [u16; 4], depth: u16) -> u16 {
    if depth == 16 {
        let alpha = f64::from(alpha) / 65535.;
        (f64::from(default) * 257. * (1. - alpha) + f64::from(red) * alpha).round() as u16
    } else {
        let alpha = f32::from(crate::raster::project16(alpha)) / 255.;
        (f32::from(default) * (1. - alpha) + f32::from(crate::raster::project16(red)) * alpha)
            .round() as u16
    }
}

fn standard_mask_frame(doc: &Document, layer: &Layer, overlay: Option<styles::StandardStyles<'_>>) -> [i32; 4] {
    if let Some(overlay) = overlay {
        overlay.bounds
    } else if crate::effects::canvas_frame(layer) {
        [0, 0, doc.width as i32, doc.height as i32]
    } else {
        [layer.x, layer.y, layer.x + layer.pixels.width as i32, layer.y + layer.pixels.height as i32]
    }
}

fn in_mask_frame([left, top, right, bottom]: [i32; 4], x: i32, y: i32) -> bool {
    x >= left && y >= top && x < right && y < bottom
}

fn has_letter_spacing(doc: &Document) -> bool {
    doc.layers.iter().any(|layer| {
        matches!(layer.source.as_ref().map(|source| &source.content),
            Some(crate::source::Content::Text { letter_spacing, .. }) if *letter_spacing != 0.)
    })
}

fn has_mask_density(doc: &Document) -> bool {
    doc.layers.iter().filter_map(|layer| layer.mask.as_ref())
        .any(|mask| mask.steps.iter().any(|step| step.kind == "density"))
}

fn has_shaped_text(doc: &Document) -> bool {
    doc.layers.iter().any(|layer| {
        matches!(
            layer.source.as_ref().map(|source| &source.content),
            Some(crate::source::Content::Text {
                layout: crate::source::TextLayout::Shaped,
                ..
            })
        )
    })
}

fn has_paragraph_text(doc: &Document) -> bool {
    doc.layers.iter().any(|layer| matches!(
        layer.source.as_ref().map(|source| &source.content),
        Some(crate::source::Content::Text { layout: crate::source::TextLayout::Paragraph, .. })
    ))
}

fn has_installed_fonts(doc: &Document) -> bool {
    doc.layers.iter().any(|layer| {
        matches!(
            layer.source.as_ref().map(|source| &source.content),
            Some(crate::source::Content::Text {
                font: crate::fonts::TextFont::Installed(_),
                ..
            })
        )
    })
}

// Photoshop stores mask rectangles in document coordinates for both link states.
const MASK_UNLINKED: u8 = 1;
const MASK_DISABLED: u8 = 2;
const STANDARD_MASK_FLAGS: u8 = MASK_UNLINKED | MASK_DISABLED;
const MASK_RENDERED: u8 = 8;
const MASK_PARAMETERS: u8 = 16;
const USER_MASK_DENSITY: u8 = 1;
const USER_MASK_FEATHER: u8 = 2;

fn standard_mask_flags(enabled: bool, linked: bool) -> u8 {
    (if linked { 0 } else { MASK_UNLINKED }) | (if enabled { 0 } else { MASK_DISABLED })
}

#[derive(Clone, Copy)]
struct MaskHeader {
    bounds: [i32; 4],
    rendered_bounds: Option<[i32; 4]>,
    default: u8,
    flags: u8,
    density: Option<u8>,
    has_parameters: bool,
}

fn standard_mask_header(bytes: &[u8]) -> Option<MaskHeader> {
    let stored_flags = *bytes.get(17)?;
    let parameters = stored_flags & MASK_PARAMETERS != 0;
    let (dual, density) = match bytes.len() {
        20 if !parameters && bytes[18..] == [0; 2] => (false, None),
        20 if parameters && bytes[18] == USER_MASK_DENSITY && matches!(bytes[16], 0 | 255) => {
            (false, Some(bytes[19]))
        }
        27 | 28 if parameters && matches!(bytes[16], 0 | 255) => {
            match bytes[18] {
                USER_MASK_FEATHER if f64::from_be_bytes(bytes[19..27].try_into().unwrap()) == 0.
                    && (bytes.len() == 27 || bytes[27] == 0) => (false, None),
                flags if flags == (USER_MASK_DENSITY | USER_MASK_FEATHER) && bytes.len() == 28
                    && f64::from_be_bytes(bytes[20..28].try_into().unwrap()) == 0. => (false, Some(bytes[19])),
                _ => return None,
            }
        }
        36 if !parameters && bytes[18] == stored_flags & !MASK_RENDERED && bytes[19] == bytes[16] => (true, None),
        _ => return None,
    };
    let flags = stored_flags & !((if dual { MASK_RENDERED } else { 0 }) | MASK_PARAMETERS);
    if flags & !STANDARD_MASK_FLAGS != 0 {
        return None;
    }
    let bounds = |offset: usize| std::array::from_fn(|i| {
        let at = offset + i * 4;
        i32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
    });
    Some(MaskHeader {
        bounds: bounds(if dual { 20 } else { 0 }),
        rendered_bounds: dual.then(|| bounds(0)),
        default: bytes[16],
        flags,
        density,
        has_parameters: parameters,
    })
}

fn standard_mask_payload(bounds: [i32; 4], default: u8, flags: u8, dual: bool, density: Option<u8>) -> [u8; 36] {
    let [l, t, r, b] = bounds;
    let mut bytes = [0; 36];
    for (target, coordinate) in bytes[..16].chunks_exact_mut(4).zip([t, l, b, r]) {
        target.copy_from_slice(&coordinate.to_be_bytes());
    }
    bytes[16] = default;
    bytes[17] = flags;
    if dual {
        bytes[17] |= MASK_RENDERED;
        bytes[18] = flags;
        bytes[19] = default;
        bytes.copy_within(..16, 20);
    } else if let Some(density) = density {
        bytes[17] |= MASK_PARAMETERS;
        bytes[18] = USER_MASK_DENSITY;
        bytes[19] = density;
    }
    bytes
}

fn dual_mask(layer: &Layer) -> bool {
    layer.mask.is_some() && layer.psd_metadata.iter().any(|entry| entry.key == "vmsk")
}

fn standard_flag(key: &[u8], bytes: &[u8]) -> Result<(), &'static str> {
    let boolean = bytes.len() == 4 && bytes[0] <= 1 && bytes[1..] == [0; 3];
    match key {
        b"clbl" if boolean => Ok(()),
        b"clbl" => Err("Invalid Blend Clipped Layers As Group (clbl)"),
        b"knko" if bytes == [0; 4] => Ok(()),
        b"knko" => Err("Photoshop knockout blending (knko)"),
        b"iOpa" if fill_opacity::parse(bytes).is_some() => Ok(()),
        b"iOpa" => Err("Photoshop fill opacity (iOpa)"),
        b"lspf" if bytes == [0; 4] || bytes == [0, 0, 0, 7] || bytes == COMPLETE_LAYER_LOCK => Ok(()),
        b"lspf" => Err("Photoshop layer protection flags (lspf)"),
        // Interior effect composition is validated with the complete style.
        // The vector flag alone does not alter ordinary raster layers.
        b"infx" if boolean => Ok(()),
        b"sn2P" if bytes.len() == 4 && u32::from_be_bytes(bytes.try_into().unwrap()) <= 1 => Ok(()),
        _ => Err("Invalid Photoshop layer flag"),
    }
}

/// Non-rendering Photoshop layer information that remains standard PSD data.
/// Only a small, validated allowlist can be emitted by this codec.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerMetadata {
    pub key: String,
    pub data: Vec<u8>,
}

const COMPLETE_LAYER_LOCK: [u8; 4] = 0x8000_0000u32.to_be_bytes();

pub(crate) fn has_complete_layer_lock(entries: &[LayerMetadata]) -> bool {
    entries.iter().any(|entry| entry.key == "lspf")
}

pub(crate) const LAYER_COLOR_LABELS: &[&str] = &[
    "none", "red", "orange", "yellow", "green", "blue", "violet", "gray", "seafoam",
    "indigo", "magenta", "fuchsia",
];

fn parse_layer_color(data: &[u8]) -> Option<&'static str> {
    let [high, low, 0, 0, 0, 0, 0, 0] = *data else {
        return None;
    };
    LAYER_COLOR_LABELS
        .get(usize::from(u16::from_be_bytes([high, low])))
        .copied()
}

pub(crate) fn layer_color_label(entries: &[LayerMetadata]) -> &'static str {
    entries
        .iter()
        .find(|entry| entry.key == "lclr")
        .and_then(|entry| parse_layer_color(&entry.data))
        .unwrap_or("none")
}

pub(crate) fn update_layer_color_label(
    entries: &mut Vec<LayerMetadata>,
    command: &serde_json::Value,
) -> Result<(), String> {
    let Some(value) = command.get("color_label") else {
        return Ok(());
    };
    let (index, label) = LAYER_COLOR_LABELS
        .iter()
        .copied()
        .enumerate()
        .find(|(_, label)| value.as_str() == Some(*label))
        .ok_or("Unknown Photoshop layer color label")?;
    if layer_color_label(entries) == label {
        return Ok(());
    }
    entries.retain(|entry| entry.key != "lclr");
    if index != 0 {
        let mut data = vec![0; 8];
        data[..2].copy_from_slice(&(index as u16).to_be_bytes());
        entries.push(LayerMetadata {
            key: "lclr".into(),
            data,
        });
    }
    validate_metadata(entries)
}

/// The preferred global-mask kind delegates mask values to individual layers.
/// Retain its overlay bytes in standard PSD/PSB data without changing pixels.
/// Other kinds, color spaces and payload extensions remain protected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalMaskSettings {
    pub data: Vec<u8>,
}
impl GlobalMaskSettings {
    pub(crate) fn validate(&self) -> Result<(), String> {
        validate_global_mask_settings(&self.data)
    }
}
fn validate_global_mask_settings(data: &[u8]) -> Result<(), String> {
    if !(13..=16).contains(&data.len())
        || data[..2] != [0, 0]
        || u16::from_be_bytes(data[10..12].try_into().unwrap()) > 100
        || data[12] != 128
        || data[13..].iter().any(|value| *value != 0)
    {
        return Err("Unsupported Photoshop global layer mask settings".into());
    }
    Ok(())
}

fn static_timestamp_metadata(data: &[u8]) -> Result<(), String> {
    if data.len() > 128 {
        return Err("Photoshop timestamp metadata exceeds its bounded size".into());
    }
    let mut r = Cursor::new(data);
    if r.u32()? != 1 || r.bytes(4)? != b"8BIM" || r.bytes(4)? != b"cust" {
        return Err("Only static Photoshop layer timestamps are editable metadata".into());
    }
    if r.u8()? > 1 || r.bytes(3)? != [0; 3] {
        return Err("Invalid Photoshop metadata copy flag or padding".into());
    }
    let descriptor = r.block()?;
    if r.pos != data.len() {
        return Err("Unexpected Photoshop metadata payload".into());
    }
    let mut d = Cursor::new(descriptor);
    if d.u32()? != 16 {
        return Err("Unsupported Photoshop metadata descriptor version".into());
    }
    // Photoshop emits an empty name as either zero UTF-16 units or one NUL.
    let name = d.u32()?;
    if name != 0 && (name != 1 || d.u16()? != 0) {
        return Err("Unexpected Photoshop metadata descriptor name".into());
    }
    if d.u32()? != 8
        || d.bytes(8)? != b"metadata"
        || d.u32()? != 1
        || d.u32()? != 9
        || d.bytes(9)? != b"layerTime"
        || d.bytes(4)? != b"doub"
    {
        return Err("Only the static layerTime timestamp is supported".into());
    }
    let value = f64::from_be_bytes(d.bytes(8)?.try_into().unwrap());
    if !value.is_finite() || value < 0. {
        return Err("Invalid Photoshop layer timestamp".into());
    }
    let padding = (4 - d.pos % 4) % 4;
    if d.bytes(padding)?.iter().any(|v| *v != 0) || d.pos != descriptor.len() {
        return Err("Unexpected Photoshop descriptor padding or entries".into());
    }
    Ok(())
}

fn validate_metadata_entry(entry: &LayerMetadata) -> Result<(), String> {
    let invalid = || {
        format!(
            "Invalid or unsupported Photoshop layer metadata: {}",
            entry.key
        )
    };
    let key = entry.key.strip_suffix(":closing")
        .filter(|key| closing_metadata_key(key)).unwrap_or(&entry.key);
    match key {
        blend_ranges::KEY if !entry.data.is_empty()
            && blend_ranges::parse(&entry.data, 3) == Ok(entry.data.as_slice()) => Ok(()),
        "brst" if blend_channels::parse(&entry.data).is_some_and(|mask| mask != 0) => Ok(()),
        "fxrp" if entry.data.len() == 16 => {
            if entry
                .data
                .chunks_exact(8)
                .all(|v| f64::from_be_bytes(v.try_into().unwrap()).is_finite())
            {
                Ok(())
            } else {
                Err(invalid())
            }
        }
        // Sheet colors are an enum followed by three reserved zero words.
        "lclr" if parse_layer_color(&entry.data).is_some() => Ok(()),
        "lnsr" | "lyid" if entry.data.len() == 4 => Ok(()),
        // A minimum Photoshop reader version is a compatibility marker, not
        // permission to ignore layer sources. Every other feature is validated
        // independently; newer/unknown versions remain protected.
        "lyvr" if entry.data.len() == 4 => {
            let version = u32::from_be_bytes(entry.data.as_slice().try_into().unwrap());
            if (70..=160).contains(&version) && version % 10 == 0 {
                Ok(())
            } else {
                Err(invalid())
            }
        }
        "shmd" => static_timestamp_metadata(&entry.data),
        "vmsk" if vector_mask::inspect(&entry.data).is_ok_and(|mask| mask.is_noop()) => Ok(()),
        "tsly" if entry.data == [1, 0, 0, 0] => Ok(()),
        "vmgm" if matches!(entry.data.as_slice(), [0 | 1, 0, 0, 0]) => Ok(()),
        "iOpa" if fill_opacity::parse(&entry.data).is_some_and(|amount| amount != 255) => Ok(()),
        "clbl" if entry.data == [0; 4] => Ok(()),
        "infx" if standard_flag(b"infx", &entry.data).is_ok() => Ok(()),
        "sn2P" if standard_flag(b"sn2P", &entry.data).is_ok() => Ok(()),
        "lspf" if entry.data == COMPLETE_LAYER_LOCK => Ok(()),
        CLOSED_FOLDER if entry.data == 2u32.to_be_bytes() => Ok(()),
        _ => Err(invalid()),
    }
}

// The editable group owns both standard records, which can have distinct IDs.
const CLOSING_LAYER_ID: &str = "lyid:closing";
const CLOSED_FOLDER: &str = "lsct:closed";
pub(crate) const MAX_LAYER_METADATA: usize = 23;

fn closing_metadata_key(key: &str) -> bool {
    matches!(key, "lyid" | "fxrp" | "lclr" | "lnsr" | "shmd" | "lyvr")
}

const INTERIOR_GROUP_FORMAT: u32 = 74;
const PATTERN_COVERAGE_FORMAT: u32 = 95;
const PIXEL_ASPECT_FORMAT: u32 = 99;
const UNTAGGED_PROFILE_FORMAT: u32 = 100;
const GRID_FORMAT: u32 = 101;
const LAYER_ID_SEED_FORMAT: u32 = 102;
const PRINT_FLAGS_FORMAT: u32 = 103;
const COPYRIGHT_FORMAT: u32 = 104;
const FILE_INFO_FORMAT: u32 = 126;
const MASK_DENSITY_FORMAT: u32 = 127;
const PARAGRAPH_TEXT_FORMAT: u32 = 128;
const EXTENDED_FILE_INFO_FORMAT: u32 = 129;
const PRINT_DESCRIPTOR_FORMAT: u32 = 130;
const CLOSED_FOLDER_FORMAT: u32 = 131;
const PLATFORM_PRINT_FORMAT: u32 = 132;
const MEASUREMENT_SCALE_FORMAT: u32 = 133;
const LEGACY_PRINT_SAMPLER_FORMAT: u32 = 134;
const WATERMARK_URL_LIST_FORMAT: u32 = 135;
const JPEG_QUALITY_RESOURCE_FORMAT: u32 = 136;
const CLOSING_METADATA_FORMAT: u32 = 137;
const LEGACY_PRINT_SETUP_FORMAT: u32 = 138;
const DOT_RANGE_FORMAT: u32 = 139;
const DISABLED_VECTOR_MASK_FORMAT: u32 = 140;
const GLOBAL_ANGLE_FORMAT: u32 = 105;
const GLOBAL_ALTITUDE_FORMAT: u32 = 106;
const COLOR_OVERLAY_FORMAT: u32 = 107;
const PRECISE_COLOR_OVERLAY_FORMAT: u32 = 108;
const BLENDED_COLOR_OVERLAY_FORMAT: u32 = 109;
const MASKED_COLOR_OVERLAY_FORMAT: u32 = 110;
const COLOR_OVERLAY_STATE_FORMAT: u32 = 111;
const COLOR_OVERLAY_LAYER_FORMAT: u32 = 112;
const RASTER_COLOR_OVERLAY_FORMAT: u32 = 113;
const MULTI_COLOR_OVERLAY_FORMAT: u32 = 114;
const HARD_SHADOW_FORMAT: u32 = 115;
const SHADOW_STACK_FORMAT: u32 = 116;
const STROKE_STYLE_FORMAT: u32 = 117;
const GRADIENT_OVERLAY_FORMAT: u32 = 118;
const PATTERN_OVERLAY_FORMAT: u32 = 119;
const MULTI_GRADIENT_OVERLAY_FORMAT: u32 = 120;
pub(crate) const PLACED_PATTERN_OVERLAY_FORMAT: u32 = 121;
const GLOBAL_LIGHT_STYLE_FORMAT: u32 = 122;
const MULTI_INNER_SHADOW_FORMAT: u32 = 123;
const PHOTOSHOP_RGB8_MULTIPLY_FORMAT: u32 = 124;
const MULTI_INSIDE_STROKE_FORMAT: u32 = 125;

pub(crate) fn resident_patterns(
    doc: &Document, plan: &StandardPlan, geometry: patterns::Geometry, source_format: u32, check: &dyn Fn() -> Result<(), String>,
) -> Result<patterns::StandardPatterns, String> {
    if plan.composite || doc.psd_global_metadata.iter().any(|entry| entry.key == "Patt") {
        return Ok(patterns::StandardPatterns::default());
    }
    let pool = crate::large_document::ResidentBudget::new(96 * 1024 * 1024).map_err(|e| e.to_string())?;
    let order = standard_order(doc);
    let fills = order.iter().rev().filter_map(|&(layer, section)| {
        (section == 0).then(|| patterns::standard(layer.into(), doc.width, doc.height, geometry))
            .flatten().map(|fill| fill.png)
    });
    let styles = order.iter().rev().filter_map(|&(layer, section)| {
        (source_format >= PATTERN_OVERLAY_FORMAT).then(|| standard_style_candidate(doc, layer, section, plan))
            .flatten().filter(|style| style.source_format() <= source_format)
            .and_then(styles::StandardStyles::pattern_source)
    });
    patterns::StandardPatterns::prepare(fills.chain(styles), doc.bit_depth, &pool, check)
}

fn has_pattern_coverage(doc: &Document) -> bool {
    doc.layers.iter().flat_map(|layer| &layer.effects).chain(&doc.filters)
        .any(|effect| crate::effects::styles::pattern_coverage(&effect.kind, &effect.settings))
}

fn has_global_light_style(doc: &Document) -> bool {
    doc.layers.iter().flat_map(|layer| &layer.effects).chain(&doc.filters)
        .any(|effect| effect.settings.get("global_light").is_some())
}

fn has_interior_group_flag(doc: &Document) -> bool {
    doc.layers.iter().any(|layer| layer.psd_metadata.iter().any(|entry| entry.key == "infx"))
}

fn has_closing_metadata(doc: &Document) -> bool {
    doc.layers.iter().flat_map(|layer| &layer.psd_metadata)
        .any(|entry| entry.key.ends_with(":closing") && entry.key != CLOSING_LAYER_ID)
}

fn extended_vector_metadata(entries: &[LayerMetadata]) -> bool {
    entries.iter().any(|entry| {
        matches!(entry.key.as_str(), "tsly" | "vmgm")
            || (entry.key == "vmsk"
                && vector_mask::inspect(&entry.data).is_ok_and(|mask| mask.flags != 0))
    })
}

fn has_implicit_disabled_vector_mask(doc: &Document) -> bool {
    doc.layers.iter().flat_map(|layer| &layer.psd_metadata).any(|entry| {
        entry.key == "vmsk" && vector_mask::inspect(&entry.data)
            .is_ok_and(|mask| mask.is_noop() && mask.initially_full.is_none())
    })
}

pub fn validate_metadata(entries: &[LayerMetadata]) -> Result<(), String> {
    if entries.len() > MAX_LAYER_METADATA {
        return Err("Photoshop layer metadata exceeds its bounded tag count".into());
    }
    let mut keys = std::collections::HashSet::new();
    for entry in entries {
        if !keys.insert(entry.key.as_str()) {
            return Err("Duplicate Photoshop layer metadata tag".into());
        }
        validate_metadata_entry(entry)?;
    }
    if !keys.contains("vmsk")
        && entries.iter().any(|entry| entry.key == "vmgm" && entry.data[0] == 1)
    {
        return Err("Photoshop global vector-mask blending requires a preserved no-op vector mask".into());
    }
    Ok(())
}

pub(crate) fn validate_layer_metadata(
    entries: &[LayerMetadata],
    locked: bool,
    group: bool,
) -> Result<(), String> {
    validate_metadata(entries)?;
    if !locked && has_complete_layer_lock(entries) {
        return Err("Photoshop Lock All metadata requires a locked layer".into());
    }
    if !group && entries.iter().any(|entry| entry.key.ends_with(":closing") || entry.key == CLOSED_FOLDER) {
        return Err("Photoshop folder metadata requires a folder".into());
    }
    Ok(())
}

pub(crate) fn folder_is_closed(entries: &[LayerMetadata]) -> bool {
    entries.iter().any(|entry| entry.key == CLOSED_FOLDER)
}

fn folder_section(entries: &[LayerMetadata]) -> u32 {
    if folder_is_closed(entries) { 2 } else { 1 }
}

fn standard_layer_metadata(
    entries: &[LayerMetadata],
    section: u32,
) -> impl Iterator<Item = (&str, &[u8])> {
    entries.iter().filter_map(move |entry| {
        let closing = entry.key.strip_suffix(":closing");
        if section == 3 {
            closing.map(|key| (key, entry.data.as_slice()))
        } else if closing.is_none() && entry.key != CLOSED_FOLDER {
            Some((entry.key.as_str(), entry.data.as_slice()))
        } else {
            None
        }
    })
}

pub(crate) fn validate_photoshop_layer_ids<'a>(
    layers: impl IntoIterator<Item = &'a [LayerMetadata]>,
) -> Result<(), String> {
    let mut ids = std::collections::HashSet::new();
    for entry in layers.into_iter().flatten().filter(|entry| {
        matches!(entry.key.as_str(), "lyid" | CLOSING_LAYER_ID)
    }) {
        if !ids.insert(entry.data.as_slice()) {
            return Err("Duplicate Photoshop layer ID (lyid)".into());
        }
    }
    Ok(())
}

/// Respect Photoshop's per-item copy-on-duplication flag; never clone unique IDs.
pub fn metadata_for_duplicate(entries: &[LayerMetadata]) -> Vec<LayerMetadata> {
    entries
        .iter()
        .filter(|entry| metadata_copies(entry))
        .cloned()
        .collect()
}
pub(crate) fn metadata_copies(entry: &LayerMetadata) -> bool {
    match entry.key.as_str() {
        "lyid" | CLOSING_LAYER_ID => false,
        "shmd" | "shmd:closing" => entry.data.get(12) == Some(&1),
        _ => true,
    }
}

#[derive(Serialize, Deserialize)]
struct Embedded<D = Document> {
    format: u32,
    standard_hash: u64,
    document: D,
}
// Bound decoded bytes and check cancellation before native decompression.
struct EmbeddedReader<'a, R> {
    inner: R,
    remaining: usize,
    control: &'a crate::loading::Control,
}
impl<R: Read> Read for EmbeddedReader<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.control.check().map_err(std::io::Error::other)?;
        let size = bytes.len().min(self.remaining.saturating_add(1));
        let n = self.inner.read(&mut bytes[..size])?;
        if n > self.remaining {
            return Err(std::io::Error::other(
                "Editable PSD source data exceeds its decoded budget",
            ));
        }
        self.remaining -= n;
        Ok(n)
    }
}
fn embedded_source<R: Read>(
    input: R,
    limit: usize,
    control: &crate::loading::Control,
) -> Result<Embedded, serde_json::Error> {
    // Slice parsing avoids per-byte reader overhead. The capped JSON buffer
    // remains separate from the 64 KiB transfer scratch and restored sources.
    let mut reader = EmbeddedReader {
        inner: input,
        remaining: limit,
        control,
    };
    let mut json = Vec::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        let n = reader.read(&mut chunk).map_err(serde_json::Error::io)?;
        if n == 0 {
            break;
        }
        json.extend_from_slice(&chunk[..n]);
    }
    // Reaching the real zlib EOF validates the checksum even at the exact cap.
    // Do not start the indivisible JSON parse if the final read was canceled.
    control
        .check()
        .map_err(std::io::Error::other)
        .map_err(serde_json::Error::io)?;
    #[cfg(test)]
    observe_open_phase("json_inflated", json.len());
    let result = serde_json::from_slice(&json);
    #[cfg(test)]
    observe_open_phase("json_parsed", 0);
    result
}
struct BoundedWriter<'a, W> {
    inner: W,
    written: usize,
    limit: usize,
    check: &'a dyn Fn() -> Result<(), String>,
    error: &'static str,
}
impl<W: Write> Write for BoundedWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        (self.check)().map_err(std::io::Error::other)?;
        if self.written.saturating_add(bytes.len()) > self.limit {
            return Err(std::io::Error::other(self.error));
        }
        let n = self.inner.write(bytes)?;
        self.written += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (self.check)().map_err(std::io::Error::other)?;
        self.inner.flush()
    }
}

struct StandardWriter<'a, W> {
    inner: W,
    bytes: u64,
    hash: u64,
    check: &'a dyn Fn() -> Result<(), String>,
}
impl<'a, W: Write> StandardWriter<'a, W> {
    fn new(inner: W, check: &'a dyn Fn() -> Result<(), String>) -> Self {
        Self {
            inner,
            bytes: 0,
            hash: 0xcbf29ce484222325,
            check,
        }
    }
}
impl<W: Write> Write for StandardWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        (self.check)().map_err(std::io::Error::other)?;
        if self
            .bytes
            .checked_add(bytes.len() as u64)
            .is_none_or(|n| n > 256 * 1024 * 1024)
        {
            return Err(std::io::Error::other(
                "Photoshop output exceeds the initial 256 MiB file limit",
            ));
        }
        let n = self.inner.write(bytes)?;
        self.bytes += n as u64;
        hash_part(&mut self.hash, &bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (self.check)().map_err(std::io::Error::other)?;
        self.inner.flush()
    }
}
fn copy_standard(
    spool: &mut impl Read,
    output: &mut dyn Write,
    mut remaining: u64,
    expected: u64,
    buffer: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let mut state = 0xcbf29ce484222325;
    while remaining != 0 {
        check()?;
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let n = spool
            .read(&mut buffer[..wanted])
            .map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("Photoshop standard spool was truncated".into());
        }
        hash_part(&mut state, &buffer[..n]);
        output.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
        remaining -= n as u64;
    }
    check()?;
    if state != expected {
        return Err("Photoshop standard spool changed while copying".into());
    }
    Ok(())
}

pub fn encode(doc: &Document) -> Result<Vec<u8>, String> {
    encode_layout(doc, Layout::Psd)
}
pub fn encode_psb(doc: &Document) -> Result<Vec<u8>, String> {
    encode_layout(doc, Layout::Psb)
}
pub(crate) fn encode_to<S: Read + Write + Seek, C: Read + Write + Seek>(
    doc: &Document,
    output: &mut dyn Write,
    spool: &mut S,
    scratch: &mut C,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    encode_layout_scratch_to(
        doc,
        Layout::Psd,
        output,
        spool,
        Some(scratch),
        check,
        progress,
    )
}
pub(crate) fn encode_psb_to<S: Read + Write + Seek, C: Read + Write + Seek>(
    doc: &Document,
    output: &mut dyn Write,
    spool: &mut S,
    scratch: &mut C,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    encode_layout_scratch_to(
        doc,
        Layout::Psb,
        output,
        spool,
        Some(scratch),
        check,
        progress,
    )
}
fn check_output_size(doc: &Document, plan: &StandardPlan, patterns: &patterns::StandardPatterns) -> Result<(), String> {
    // Validation bounds every frame and record count; u64 also covers their aggregate.
    let bytes = u64::from(doc.bit_depth / 8);
    let canvas = u64::from(doc.width) * u64::from(doc.height);
    let mut minimum = 40 + 4 * canvas * bytes + patterns.encoded_bytes();
    minimum += doc
        .psd_global_metadata
        .iter()
        .map(|m| 12 + (m.data.len() as u64).div_ceil(4) * 4)
        .sum::<u64>();
    let mut records = usize::from(plan.composite);
    if plan.composite {
        minimum += 4 * canvas * bytes + 8;
    }
    if !plan.composite || plan.hidden_sources {
        for (layer, section) in standard_order(doc) {
            records += 1;
            let overlay = standard_styles(doc, layer, section, plan, patterns);
            let [x, y, right, bottom] = standard_bounds(doc, layer, section, overlay);
            minimum +=
                4 * (u64::from((right - x) as u32) * u64::from((bottom - y) as u32) * bytes + 2);
            if layer.mask.is_some() && section != 3 {
                let frame = standard_mask_frame(doc, layer, overlay);
                let density = standard_mask_density(layer.into()).is_some();
                let [l, t, r, b] = extended_mask(layer.into(), frame, density).map_or(frame, |m| m.bounds);
                let pixels = (r - l) as u64 * (b - t) as u64;
                minimum += pixels * bytes + 2;
            }
        }
    }
    if records == 0 {
        minimum += 4 * canvas * bytes + 8;
    }
    if records > 200 {
        return Err("Too many PSD layer records".into());
    }
    if minimum > 256 * 1024 * 1024 {
        return Err("Uncompressed Photoshop channels alone exceed the 256 MiB file limit; use a smaller canvas or fewer full-frame layers".into());
    }
    Ok(())
}
// The caller has validated the layout and matching native depth. Keep absent
// tiles and padded output outside the raster identical to Raster::get/get16.
fn native_channel_row(pixels: &Raster, y: u32, channel: usize, row: &mut [u8]) {
    row.fill(0);
    if y >= pixels.height {
        return;
    }
    let bytes = if pixels.is16() { 2 } else { 1 };
    let width = (row.len() / bytes).min(pixels.width as usize);
    let stride = crate::raster::TILE as usize;
    let start = (y % crate::raster::TILE) as usize * stride * 4 + channel;
    for x in (0..width).step_by(stride) {
        let key = ((x / stride) as u32, y / crate::raster::TILE);
        let count = (width - x).min(stride);
        if pixels.is16() {
            if let Some(tile) = pixels.samples16.get(&key) {
                for (sample, output) in tile[start..]
                    .iter()
                    .step_by(4)
                    .take(count)
                    .zip(row[x * 2..(x + count) * 2].chunks_exact_mut(2))
                {
                    output.copy_from_slice(&sample.to_be_bytes());
                }
            }
        } else if let Some(tile) = pixels.tiles.get(&key) {
            for (sample, output) in tile[start..]
                .iter()
                .step_by(4)
                .take(count)
                .zip(&mut row[x..x + count])
            {
                *output = *sample;
            }
        }
    }
}

fn encode_layout(doc: &Document, layout: Layout) -> Result<Vec<u8>, String> {
    let mut output = Vec::new();
    let mut spool = std::io::Cursor::new(Vec::new());
    encode_layout_to(doc, layout, &mut output, &mut spool, &|| Ok(()))?;
    Ok(output)
}
fn encode_layout_to<S: Read + Write + Seek>(
    doc: &Document,
    layout: Layout,
    output: &mut dyn Write,
    spool: &mut S,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    encode_layout_progress_to(doc, layout, output, spool, check, &|_, _, _| Ok(()))
}
fn encode_layout_progress_to<S: Read + Write + Seek>(
    doc: &Document,
    layout: Layout,
    output: &mut dyn Write,
    spool: &mut S,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    encode_layout_scratch_to(doc, layout, output, spool, None, check, progress)
}
fn encode_layout_scratch_to<S: Read + Write + Seek>(
    doc: &Document,
    layout: Layout,
    output: &mut dyn Write,
    spool: &mut S,
    scratch: Option<&mut dyn composite::Scratch>,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    progress("Preparing Photoshop sources", 0, 0)?;
    check()?;
    if spool.stream_position().map_err(|e| e.to_string())? != 0
        || spool.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != 0
    {
        return Err("Photoshop standard spool must be empty at position zero".into());
    }
    if doc.read_only {
        return Err(
            "Cannot save an unsupported PSD without explicitly creating a compatible copy".into(),
        );
    }
    validate(doc)?;
    let plan = standard_plan(doc);
    let standard_patterns = resident_patterns(doc, &plan, patterns::Geometry::Rotation, PLACED_PATTERN_OVERLAY_FORMAT, check)?;
    check_output_size(doc, &plan, &standard_patterns)?;
    let high = doc.bit_depth == 16;
    // Effect/bake/profile paths retain their separately materialized behavior.
    let strip_composite = scratch.is_some()
        && !plan.composite
        && doc.icc_profile.is_none()
        && doc.layers.iter().all(|layer| layer.effects.is_empty());
    progress("Preparing layer masks", 0, 0)?;
    check()?;
    let masks = if high {
        vec![None; doc.layers.len()]
    } else {
        doc.layers
            .iter()
            .map(|l| {
                check()?;
                Ok(l.mask
                    .as_ref()
                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height)))
            })
            .collect::<Result<Vec<_>, String>>()?
    };
    progress("Preparing layer effects", 0, 0)?;
    check()?;
    let colors = if high {
        vec![None; doc.layers.len()]
    } else {
        crate::effects::prepare(doc, &masks)?
    };
    progress("Preparing native layer effects", 0, 0)?;
    check()?;
    let colors16 = if high {
        Some(crate::depth16::prepare(doc)?)
    } else {
        None
    };
    progress("Preparing native masks", 0, 0)?;
    check()?;
    let masks16 = if high {
        Some(crate::depth16::prepare_masks(doc)?)
    } else {
        None
    };
    progress("Preparing current composite", 0, 0)?;
    check()?;
    let composite16 = if high && !strip_composite {
        Some(crate::depth16::render(doc)?)
    } else {
        None
    };
    check()?;
    // Unsupported adjustments and clipping units still need a standard bake.
    let baked = if plan.composite {
        let mut layer = Layer::new(
            "PeerBrush composite · editable sources in PeerBrush",
            "paint",
            doc.width,
            doc.height,
        );
        layer.pixels = if let Some(image) = &composite16 {
            Raster::from_rgba16(doc.width, doc.height, &image.words)?
        } else {
            let (_, _, bytes, _) = doc.preview(None, doc.width.max(doc.height), None, false)?;
            Raster::from_rgba(doc.width, doc.height, &bytes)?
        };
        Some(layer)
    } else {
        None
    };
    let mut ordered = Vec::new();
    // Keep standard original raster/mask channels alongside a visible filtered
    // composite. Their root visibility is presentation-only in the saved PSD;
    // private sources retain the actual editable visibility and filter stack.
    let standard_sources = if plan.hidden_sources {
        let mut source = doc.clone();
        source.filters.clear();
        for layer in &mut source.layers {
            if layer.parent.is_none() {
                layer.visible = false;
            }
        }
        Some(source)
    } else {
        None
    };
    if let Some(layer) = &baked {
        ordered.push((layer, 0));
        if let Some(source) = &standard_sources {
            ordered.extend(standard_order(source));
        }
    } else {
        ordered.extend(standard_order(doc));
    }
    let empty_layer = Layer::new("Canvas", "paint", doc.width, doc.height);
    if ordered.is_empty() {
        ordered.push((&empty_layer, 0));
    }
    // PSD stores layer records from bottom to top, including group dividers.
    ordered.reverse();
    if ordered.len() > 200 {
        return Err("Too many PSD layer records".into());
    }
    let mut records = vec![];
    let mut pattern_format = 0;
    let mut style_format = 0;
    let mut channel_bytes = 0u64;
    let mut layer_rows = 0usize;
    for (l, section) in &ordered {
        let standard_style = standard_styles(doc, l, *section, &plan, &standard_patterns);
        let [x, y, right, bottom] = standard_bounds(doc, l, *section, standard_style);
        let (w, h) = ((right - x) as u32, (bottom - y) as u32);
        i32b(&mut records, y);
        i32b(&mut records, x);
        i32b(&mut records, y + h as i32);
        i32b(&mut records, x + w as i32);
        let mask = l.mask.as_ref();
        let has_mask = mask.is_some() && *section != 3;
        let dual = has_mask && dual_mask(l);
        let density = has_mask.then(|| standard_mask_density((*l).into())).flatten();
        let frame = standard_mask_frame(doc, l, standard_style);
        let extended = extended_mask((*l).into(), frame, density.is_some()).filter(|_| has_mask);
        let [mask_x, mask_y, mask_r, mask_b] = extended.as_ref().map_or(frame, |m| m.bounds);
        let (mask_w, mask_h) = ((mask_r - mask_x) as u32, (mask_b - mask_y) as u32);
        let unlinked = has_mask && mask.is_some_and(|m| !m.linked);
        u16b(&mut records, 4 + u16::from(has_mask) + u16::from(dual));
        for ch in [-1i16, 0, 1, 2]
            .into_iter()
            .chain(if has_mask { Some(-2) } else { None })
            .chain(dual.then_some(-3))
        {
            let (cw, ch_height) = if ch <= -2 { (mask_w, mask_h) } else { (w, h) };
            let length = u64::from(cw)
                .checked_mul(u64::from(ch_height))
                .and_then(|n| n.checked_mul(u64::from(doc.bit_depth / 8)))
                .and_then(|n| n.checked_add(2))
                .ok_or("Photoshop channel length overflow")?;
            u16b(&mut records, ch as u16);
            layout.write_length(&mut records, length)?;
            channel_bytes = channel_bytes
                .checked_add(length)
                .ok_or("Photoshop channel length overflow")?;
            layer_rows += ch_height as usize;
        }
        records.extend_from_slice(b"8BIM");
        let blend = if *section == 3 && l.blend == "pass_through" {
            *b"norm"
        } else {
            scene_save36::blend(&l.blend).unwrap_or(*b"norm")
        };
        records.extend_from_slice(&blend);
        records.push((l.opacity * 255.0).round() as u8);
        records.push(u8::from(l.clip_to.is_some()));
        let invert = *section == 0
            && adjustments::standard_invert((*l).into(), doc.width, doc.height, !plan.composite);
        let tone = if *section == 0 {
            adjustments::standard_tone((*l).into(), doc.width, doc.height, !plan.composite, ToneRecords::current(doc.bit_depth))
        } else { None };
        let solid = if *section == 0 && !plan.composite {
            standard_style.filter(|overlay| overlay.fill).map(|_| l.color)
                .or_else(|| adjustments::standard_solid((*l).into(), doc.width, doc.height))
        } else {
            None
        };
        let gradient = if *section == 0 && !plan.composite {
            gradient_fill::standard((*l).into(), doc.width, doc.height)
        } else { None };
        let pattern = if *section == 0 && !plan.composite {
            patterns::standard((*l).into(), doc.width, doc.height, patterns::Geometry::Rotation)
                .and_then(|fill| standard_patterns.record(fill))
        } else { None };
        records.push(
            if l.visible { 0 } else { 2 }
                | u8::from(l.locked && *section != 3)
                | if invert || tone.is_some() || solid.is_some() || gradient.is_some() || pattern.is_some() { 0x18 } else { 0 },
        );
        records.push(0);
        let mut extra = vec![];
        if let Some(mask) = mask.filter(|_| has_mask) {
            let default = extended.as_ref().map_or_else(
                || if density.is_some() { density_mask_default(crate::engine::view::MaskView::Resident(mask)) } else { 255 },
                |m| m.default,
            );
            let m = standard_mask_payload(
                [mask_x, mask_y, mask_r, mask_b], default,
                standard_mask_flags(mask.enabled, mask.linked), dual, density,
            );
            block(&mut extra, &m[..if dual { 36 } else { 20 }]);
        } else {
            u32b(&mut extra, 0);
        }
        let ranges = (*section != 3).then(|| blend_ranges::value(&l.psd_metadata)).flatten();
        block(&mut extra, ranges.as_ref().map_or(&[][..], |pairs| pairs.as_flattened()));
        let name = if *section == 3 {
            "</Layer group>"
        } else {
            &l.name
        };
        let legacy_name = legacy_layer_name(name);
        extra.push(legacy_name.len() as u8);
        extra.extend_from_slice(legacy_name);
        while extra.len() % 4 != 0 {
            extra.push(0);
        }
        let mut unicode = vec![];
        u32b(&mut unicode, layer_name_units(name)?);
        for c in name.encode_utf16() {
            u16b(&mut unicode, c);
        }
        tag(&mut extra, b"luni", &unicode);
        if unlinked {
            // Decouple independent positioning from the legacy effects crossfade.
            tag(&mut extra, b"lmgm", &[0; 4]);
        }
        if invert {
            tag(&mut extra, b"nvrt", &[]);
        }
        if let Some(tone) = tone {
            if let Some((key, bytes)) = tone.companion() {
                tag(&mut extra, &key, bytes);
            }
            let (key, bytes) = tone.parts();
            tag(&mut extra, &key, bytes);
        }
        if let Some(color) = solid {
            tag(&mut extra, b"SoCo", &adjustments::solid_descriptor(color));
        }
        if let Some(overlay) = standard_style {
            styles::write_standard_style_tag(&mut extra, &overlay).map_err(|error| error.to_string())?;
            style_format = style_format.max(overlay.source_format());
        }
        if let Some(settings) = gradient {
            tag(&mut extra, b"GdFl", gradient_fill::descriptor(settings)?.bytes());
        }
        if let Some(fill) = pattern {
            tag(&mut extra, b"PtFl", patterns::descriptor(fill).bytes());
            pattern_format = pattern_format.max(fill.source_format());
        }
        if l.locked && *section != 3 && !has_complete_layer_lock(&l.psd_metadata) {
            tag(&mut extra, b"lspf", &7u32.to_be_bytes());
        }
        if *section != 3 && clipping::grouped(&l.psd_metadata)
            && doc
                .layers
                .iter()
                .any(|n| n.clip_to.as_deref() == Some(&l.id))
        {
            tag(&mut extra, b"clbl", &[1, 0, 0, 0]);
        }
        if *section != 0 {
            let mut s = vec![];
            u32b(&mut s, *section);
            if *section != 3 && l.blend == "pass_through" {
                s.extend_from_slice(b"8BIMpass");
            }
            tag(&mut extra, b"lsct", &s);
        }
        for (key, bytes) in standard_layer_metadata(&l.psd_metadata, *section) {
            if key == blend_ranges::KEY {
                continue;
            }
            let key = key.as_bytes().try_into().map_err(|_| "Invalid Photoshop metadata tag")?;
            tag(&mut extra, key, bytes);
        }
        block(&mut records, &extra);
    }
    let info_raw = (records.len() as u64)
        .checked_add(channel_bytes)
        .and_then(|n| n.checked_add(2))
        .ok_or("Photoshop section length overflow")?;
    let info_length = info_raw
        .checked_add(info_raw % 2)
        .ok_or("Photoshop section length overflow")?;
    let mut prefix = vec![];
    if high {
        layout.write_length(&mut prefix, 0)?; // Primary8-bit layer information is empty.
        block(
            &mut prefix,
            doc.psd_global_mask
                .as_ref()
                .map_or(&[], |mask| mask.data.as_slice()),
        );
        // Photoshop needs the merged transparency marker before the 16-bit layer records.
        prefix.extend(b"8BIMMt16");
        layout.write_length(&mut prefix, 0)?;
        prefix.extend(b"8BIMLr16");
    }
    layout.write_length(&mut prefix, info_length)?;
    let mut standard = StandardWriter::new(
        std::io::BufWriter::with_capacity(64 * 1024, &mut *spool),
        check,
    );
    standard.write_all(&prefix).map_err(|e| e.to_string())?;
    // A negative count identifies the merged fourth channel as transparency.
    standard
        .write_all(&(-(ordered.len() as i16)).to_be_bytes())
        .map_err(|e| e.to_string())?;
    standard.write_all(&records).map_err(|e| e.to_string())?;
    drop(records);
    let mut row = [0u8; crate::raster::MAX_EDGE as usize * 2];
    let mut completed_rows = 0;
    progress("Writing standard layer channels", 0, layer_rows)?;
    for (l, section) in &ordered {
        check()?;
        let standard_style = standard_styles(doc, l, *section, &plan, &standard_patterns);
        let [x, y, right, bottom] = standard_bounds(doc, l, *section, standard_style);
        let (w, h) = ((right - x) as u32, (bottom - y) as u32);
        let mask = l.mask.as_ref();
        let has_mask = mask.is_some() && *section != 3;
        let dual = has_mask && dual_mask(l);
        let density = has_mask.then(|| standard_mask_density((*l).into())).flatten();
        let frame = standard_mask_frame(doc, l, standard_style);
        let extended = extended_mask((*l).into(), frame, density.is_some()).filter(|_| has_mask);
        let [mask_x, mask_y, mask_r, mask_b] = extended.as_ref().map_or(frame, |m| m.bounds);
        let (mask_w, mask_h) = ((mask_r - mask_x) as u32, (mask_b - mask_y) as u32);
        let prefix_layer = density.map(|_| {
            let mut prefix = (**l).clone();
            let mask = prefix.mask.as_mut().expect("Density requires a mask");
            mask.steps.pop();
            mask.cache_key.push_str(":before-density");
            prefix
        });
        let mask_layer = prefix_layer.as_ref().unwrap_or(l);
        let prepared = if has_mask && !high {
            mask_layer.mask.as_ref().and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
        } else {
            None
        };
        let retained = extended.as_ref().map(|mask| match mask.original {
            MaskOriginal::Resident(source) => Ok((mask, source)),
            MaskOriginal::Native(_) => Err("Resident standard mask has no resident original"),
        }).transpose()?;
        let index = doc.layers.iter().position(|n| n.id == l.id);
        // Adjustment raster channels retain their original native samples;
        // The standard adjustment supplies the appearance instead of a baked raster.
        let invert = adjustments::standard_invert((*l).into(), doc.width, doc.height, !plan.composite)
            || adjustments::standard_tone((*l).into(), doc.width, doc.height, !plan.composite, ToneRecords::current(doc.bit_depth)).is_some();
        let original_color = invert || standard_style.is_some();
        let derived = index.filter(|_| !original_color).and_then(|i| colors[i].as_ref());
        let derived16 = index
            .filter(|_| !original_color)
            .and_then(|i| colors16.as_ref().and_then(|c| c[i].as_ref()));
        let prefix16 = prefix_layer.as_ref().filter(|_| high)
            .map(crate::depth16::mask::prepare).transpose()?.flatten();
        let mask16 = if prefix_layer.is_some() {
            prefix16.as_ref()
        } else {
            index.and_then(|i| masks16.as_ref().and_then(|m| m[i].as_ref()))
        };
        for ch in [-1i16, 0, 1, 2]
            .into_iter()
            .chain(if has_mask { Some(-2) } else { None })
            .chain(dual.then_some(-3))
        {
            standard
                .write_all(&0u16.to_be_bytes())
                .map_err(|e| e.to_string())?;
            let is_mask = ch <= -2;
            let (cw, ch_height) = if is_mask { (mask_w, mask_h) } else { (w, h) };
            let raw_native = !is_mask
                && derived.is_none()
                && derived16.is_none()
                && l.kind != "fill"
                && l.pixels.is16() == high;
            for yy in 0..ch_height {
                check()?;
                if raw_native {
                    native_channel_row(
                        &l.pixels,
                        yy,
                        if ch == -1 { 3 } else { ch as usize },
                        &mut row[..cw as usize * usize::from(doc.bit_depth / 8)],
                    );
                } else {
                    for xx in 0..cw {
                        let retained_sample = if is_mask {
                            retained.and_then(|(mask, source)| {
                                let x = xx as i32 + mask_x;
                                let y = yy as i32 + mask_y;
                                if in_mask_frame(frame, x, y) {
                                    return None;
                                }
                                Some(original_mask_sample(
                                    mask.default,
                                    source.get16(x - mask.origin[0], y - mask.origin[1]),
                                    doc.bit_depth,
                                ))
                            })
                        } else {
                            None
                        };
                        if let Some(sample) = retained_sample {
                            if high {
                                row[xx as usize * 2..xx as usize * 2 + 2]
                                    .copy_from_slice(&sample.to_be_bytes());
                            } else {
                                row[xx as usize] = sample as u8;
                            }
                            continue;
                        }
                        if high {
                            let value = if is_mask {
                                (crate::depth16::mask_value_prepared(
                                    mask_layer,
                                    xx as i32 + mask_x - l.x,
                                    yy as i32 + mask_y - l.y,
                                    mask16.map(|m| m.as_ref()),
                                    true,
                                ) * 65535.)
                                    .round()
                                    .clamp(0., 65535.) as u16
                            } else {
                                let p = if let Some(image) = derived16 {
                                    image.get(xx as i32, yy as i32)
                                } else if l.kind == "fill" {
                                    l.color.map(|v| v as u16 * 257)
                                } else {
                                    l.pixels.get16(xx as i32, yy as i32)
                                };
                                p[if ch == -1 { 3 } else { ch as usize }]
                            };
                            row[xx as usize * 2..xx as usize * 2 + 2]
                                .copy_from_slice(&value.to_be_bytes());
                        } else {
                            row[xx as usize] = if is_mask {
                                (mask_layer.mask_value_prepared(
                                    xx as i32 + mask_x - l.x,
                                    yy as i32 + mask_y - l.y,
                                    prepared.as_deref(),
                                    true,
                                ) * 255.)
                                    .round() as u8
                            } else {
                                let p = if let Some(image) = derived {
                                    image.get(xx as i32, yy as i32)
                                } else if l.kind == "fill" {
                                    l.color
                                } else {
                                    l.pixels.get(xx as i32, yy as i32)
                                };
                                p[if ch == -1 { 3 } else { ch as usize }]
                            };
                        }
                    }
                }
                standard
                    .write_all(&row[..cw as usize * usize::from(doc.bit_depth / 8)])
                    .map_err(|e| e.to_string())?;
                completed_rows += 1;
                progress(
                    "Writing standard layer channels",
                    completed_rows,
                    layer_rows,
                )?;
            }
        }
    }
    if info_raw % 2 != 0 {
        standard.write_all(&[0]).map_err(|e| e.to_string())?;
    }
    let mut suffix = vec![];
    if high {
        suffix.resize(((4 - info_length % 4) % 4) as usize, 0);
    } else {
        block(
            &mut suffix,
            doc.psd_global_mask
                .as_ref()
                .map_or(&[], |mask| mask.data.as_slice()),
        );
    }
    for metadata in &doc.psd_global_metadata {
        global_metadata::write(&mut suffix, metadata, layout)?;
    }
    standard.write_all(&suffix).map_err(|e| e.to_string())?;
    standard_patterns.write(&mut standard)?;
    let layer_length = standard.bytes;
    let layer_hash = standard.hash;
    standard.hash = 0xcbf29ce484222325;
    let (w, h) = (doc.width, doc.height);
    let composite_rows = h as usize * 4;
    let mut completed_composite_rows = 0;
    progress("Writing current composite", 0, composite_rows)?;
    standard
        .write_all(&0u16.to_be_bytes())
        .map_err(|e| e.to_string())?;
    if strip_composite {
        composite::write(doc, scratch.unwrap(), &mut standard, check, progress)?;
    } else if let Some(image) = &composite16 {
        if image.width != w || image.height != h || image.words.len() != w as usize * h as usize * 4
        {
            return Err("Native PSD composite must match the full document dimensions".into());
        }
        for c in 0..4 {
            for pixels in image.words.chunks_exact(w as usize * 4) {
                check()?;
                for (x, p) in pixels.chunks_exact(4).enumerate() {
                    let value = if c < 3 {
                        ((p[c] as u64 * p[3] as u64 + 65535 * (65535 - p[3] as u64) + 32767)
                            / 65535) as u16
                    } else {
                        p[3]
                    };
                    row[x * 2..x * 2 + 2].copy_from_slice(&value.to_be_bytes());
                }
                standard
                    .write_all(&row[..w as usize * 2])
                    .map_err(|e| e.to_string())?;
                completed_composite_rows += 1;
                progress(
                    "Writing current composite",
                    completed_composite_rows,
                    composite_rows,
                )?;
            }
        }
    } else {
        check()?;
        let (rw, rh, rgba, _) = doc.preview(None, w.max(h), None, false)?;
        if rw != w || rh != h || rgba.len() != w as usize * h as usize * 4 {
            return Err("PSD composite must match the full document dimensions".into());
        }
        for c in 0..4 {
            for pixels in rgba.chunks_exact(w as usize * 4) {
                check()?;
                for (x, p) in pixels.chunks_exact(4).enumerate() {
                    row[x] = if c < 3 {
                        ((p[c] as u32 * p[3] as u32 + 255 * (255 - p[3] as u32) + 127) / 255) as u8
                    } else {
                        p[3]
                    };
                }
                standard
                    .write_all(&row[..w as usize])
                    .map_err(|e| e.to_string())?;
                completed_composite_rows += 1;
                progress(
                    "Writing current composite",
                    completed_composite_rows,
                    composite_rows,
                )?;
            }
        }
    }
    let composite_length = standard.bytes - layer_length;
    let composite_hash = standard.hash;
    standard.flush().map_err(|e| e.to_string())?;
    drop(standard);
    progress("Writing editable sources", 0, 0)?;
    check()?;
    let advanced_blending_format = doc.layers.iter()
        .map(|layer| blend_ranges::source_format(doc, layer).max(blend_channels::source_format(doc, layer))
            .max(fill_opacity::source_format(layer)))
        .max().unwrap_or(0)
        .max(if has_interior_group_flag(doc) { INTERIOR_GROUP_FORMAT } else { 0 });
    let tone_format = tone_source_format(doc);
    let file_info_format = doc.psd_file_info.as_deref().map(|bytes| file_info::source_format(bytes));
    let embedded = Embedded {
        // Older readers cannot interpret soft selections or smooth curve sources.
        format: if has_implicit_disabled_vector_mask(doc) {
            DISABLED_VECTOR_MASK_FORMAT
        } else if let Some(format) = file_info_format.filter(|format| *format >= LEGACY_PRINT_SETUP_FORMAT) {
            format
        } else if has_closing_metadata(doc) {
            CLOSING_METADATA_FORMAT
        } else if let Some(format) = file_info_format.filter(|format| *format >= PLATFORM_PRINT_FORMAT) {
            format
        } else if doc.layers.iter().any(|layer| folder_is_closed(&layer.psd_metadata)) {
            CLOSED_FOLDER_FORMAT
        } else if let Some(format) = file_info_format.filter(|format| *format > PARAGRAPH_TEXT_FORMAT) {
            format
        } else if has_paragraph_text(doc) {
            PARAGRAPH_TEXT_FORMAT
        } else if has_mask_density(doc) {
            MASK_DENSITY_FORMAT
        } else if doc.psd_file_info.is_some() {
            FILE_INFO_FORMAT
        } else if doc.psd_rgb8_multiply {
            PHOTOSHOP_RGB8_MULTIPLY_FORMAT.max(style_format)
        } else if has_global_light_style(doc) {
            GLOBAL_LIGHT_STYLE_FORMAT.max(style_format)
        } else if style_format != 0 {
            style_format
        } else if doc.psd_global_altitude.is_some() {
            GLOBAL_ALTITUDE_FORMAT
        } else if doc.psd_global_angle.is_some() {
            GLOBAL_ANGLE_FORMAT
        } else if doc.psd_copyright.is_some() {
            COPYRIGHT_FORMAT
        } else if doc.psd_print_flags.is_some() || doc.psd_print_flags_info.is_some() {
            PRINT_FLAGS_FORMAT
        } else if doc.psd_layer_id_seed.is_some() {
            LAYER_ID_SEED_FORMAT
        } else if doc.psd_grid.is_some() {
            GRID_FORMAT
        } else if doc.psd_intentionally_untagged {
            UNTAGGED_PROFILE_FORMAT
        } else if doc.psd_pixel_aspect.is_some() {
            PIXEL_ASPECT_FORMAT
        } else if pattern_format != 0 {
            pattern_format
        } else if has_pattern_coverage(doc) {
            PATTERN_COVERAGE_FORMAT
        } else if let Some(format) = tone_format.filter(|&format| format >= adjustments::LEGACY_BRIGHTNESS_FORMAT) {
            format
        } else if advanced_blending_format != 0 {
            advanced_blending_format
        } else if doc.layers.iter().any(|layer| !clipping::grouped(&layer.psd_metadata)) {
            63
        } else if doc.layers.iter().any(|layer| fill_opacity::value(&layer.psd_metadata) != 255) {
            62
        } else if ordered.iter().any(|(layer, section)| *section != 3 && dual_mask(layer)) {
            tone_format.unwrap_or(60).max(60)
        } else if let Some(format) = tone_format {
            format
        } else if (!plan.composite || plan.hidden_sources) && doc.layers.iter().any(|layer| {
            layer.effects.first().is_some_and(|effect| adjustments::shifted_curve(&effect.settings))
                && matches!(adjustments::standard_tone(layer.into(), doc.width, doc.height,
                    !plan.composite, ToneRecords::current(doc.bit_depth)), Some(ToneRecord::Curves(..)))
        }) {
            54
        } else if doc.layers.iter().flat_map(|layer| &layer.effects).chain(&doc.filters)
            .any(|effect| crate::effects::has_channel_curves(&effect.kind, &effect.settings)) {
            53
        } else if doc.layers.iter().any(|layer| {
            matches!(adjustments::standard_tone(layer.into(), doc.width, doc.height,
                !plan.composite, ToneRecords::for_source_format(52, doc.bit_depth)), Some(ToneRecord::Curves(..)))
        }) {
            52
        } else if doc.layers.iter().any(|layer| extended_vector_metadata(&layer.psd_metadata)) {
            51
        } else if doc.layers.iter().flat_map(|layer| &layer.effects).chain(&doc.filters)
            .any(|effect| crate::effects::levels::channels(&effect.kind, &effect.settings)) {
            50
        } else if doc.layers.iter().flat_map(|layer| &layer.effects).chain(&doc.filters)
            .any(|effect| crate::effects::levels::extended(&effect.kind, &effect.settings)) {
            49
        } else if doc.layers.iter().any(|layer| adjustments::standard_levels(layer.into(), doc.width, doc.height, !plan.composite).is_some()) {
            48
        } else if doc.layers.iter().flat_map(|layer| &layer.effects).chain(&doc.filters)
            .any(|effect| crate::effects::has_levels_output(&effect.kind, &effect.settings)) {
            47
        } else if !plan.composite && doc.layers.iter().any(|layer| {
            gradient_fill::standard(layer.into(), doc.width, doc.height).is_some()
        }) {
            46
        } else if doc.layers.iter().any(|layer| {
            layer.effects.iter().any(|effect| crate::effects::gradient::coverage(&effect.kind, &effect.settings))
        }) {
            45
        } else if doc.layers.iter().any(|layer| {
            layer.blend != "normal"
                && adjustments::standard_invert(layer.into(), doc.width, doc.height, true)
        }) {
            44
        } else if ordered.iter().any(|(layer, section)| {
            *section != 3
                && layer.mask.as_ref().is_some_and(|mask| mask.linked)
                && extended_mask((*layer).into(), standard_mask_frame(doc, layer, None), false).is_some()
        }) {
            43
        } else if doc.layers.iter().any(|layer| layer.psd_metadata.iter().any(|entry| entry.key == "vmsk")) {
            42
        } else if !plan.composite
            && ordered.iter().any(|(layer, section)| {
                *section == 0
                    && adjustments::standard_solid((*layer).into(), doc.width, doc.height).is_some()
            })
        {
            41
        } else if !plan.composite
            && ordered.iter().any(|(layer, section)| {
                *section == 0 && layer.kind == "adjustment" && layer.clip_to.is_some()
            })
        {
            40
        } else if doc
            .layers
            .iter()
            .any(|layer| layer.mask.as_ref().is_some_and(|m| !m.linked))
        {
            39
        } else if !doc.psd_global_metadata.is_empty() {
            37 // Source36 is the separate native tiled container contract.
        } else if doc
            .layers
            .iter()
            .any(|layer| adjustments::standard_invert(layer.into(), doc.width, doc.height, false))
        {
            35
        } else if has_shaped_text(doc) {
            34
        } else if crate::retained::has_warps(doc) {
            33
        } else if crate::retained::has_projective(doc) {
            32
        } else if has_installed_fonts(doc) {
            31
        } else if has_letter_spacing(doc) {
            30
        } else if doc.layers.iter().any(|layer| layer.mask.is_some()) {
            29
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::gradient::smoothed(&e.kind, &e.settings))
        }) {
            28
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::gradient::gridded(&e.kind, &e.settings))
        }) {
            27
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::styles::knockout_shadow(&e.kind, &e.settings))
        }) {
            26
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::styles::pixel_aligned_shadow(&e.kind, &e.settings))
        }) {
            25
        } else if doc
            .layers
            .iter()
            .any(|l| l.psd_metadata.iter().any(|m| m.key == "lyvr"))
        {
            24
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::gradient::positioned(&e.kind, &e.settings))
        }) {
            23
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::styles::separate_bevel_channels(&e.kind, &e.settings))
        }) {
            22
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::contours::extended(&e.kind, &e.settings))
        }) {
            21
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::gradient::extended(&e.kind, &e.settings))
        }) {
            20
        } else if doc.layers.iter().any(|l| {
            l.effects.iter().any(|e| {
                e.kind == "texture" && crate::effects::patterns::active(&e.kind, &e.settings)
            })
        }) {
            19
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::patterns::active(&e.kind, &e.settings))
        }) {
            18
        } else if doc.layers.iter().any(|l| {
            l.source
                .as_ref()
                .is_some_and(|s| matches!(s.content, crate::source::Content::Bezier { .. }))
        }) {
            17
        } else if doc.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::styles::contains(&e.kind))
        }) {
            16
        } else if !doc.filters.is_empty() {
            15
        } else if doc
            .layers
            .iter()
            .any(|l| l.effects.iter().any(|e| e.kind == "channel_clamp"))
        {
            14
        } else if doc
            .layers
            .iter()
            .any(|l| l.effects.iter().any(|e| e.kind == "posterize"))
        {
            13
        } else if doc.layers.iter().any(|l| {
            l.source
                .as_ref()
                .is_some_and(|s| matches!(s.content, crate::source::Content::Svg { .. }))
        }) {
            12
        } else if doc.layers.iter().any(|l| {
            l.effects.iter().any(|e| e.weight != 1.0)
                || l.mask
                    .as_ref()
                    .is_some_and(|m| m.steps.iter().any(|s| s.weight != 1.0))
        }) {
            11
        } else if doc.layers.iter().any(|l| l.blend == "pass_through") {
            10
        } else if crate::retained::has_originals(doc) {
            9
        } else if doc.layers.iter().any(|l| l.source.is_some()) {
            8
        } else if doc.layers.iter().any(|l| {
            ["group", "adjustment"].contains(&l.kind.as_str())
                && (l.x != 0
                    || l.y != 0
                    || l.pixels.width != doc.width
                    || l.pixels.height != doc.height)
        }) {
            7
        } else if doc.selection_coverage.is_some()
            || doc.selection_previous.is_some()
            || doc.layers.iter().any(|l| {
                l.effects.iter().any(|e| {
                    (e.kind == "curves" && e.settings["interpolation"] != "linear")
                        || (e.kind == "liquify"
                            && e.settings["strokes"].as_array().is_some_and(|strokes| {
                                strokes.iter().any(|s| !s["coverage"].is_null())
                            }))
                }) || l.mask.as_ref().is_some_and(|m| {
                    m.steps
                        .iter()
                        .any(|s| s.kind == "curves" && s.settings["interpolation"] != "linear")
                })
            })
        {
            6
        } else if high {
            5
        } else if doc.layers.iter().any(|l| {
            l.kind == "adjustment"
                || l.clip_to.is_some()
                || ![
                    "normal", "multiply", "screen", "overlay", "darken", "lighten",
                ]
                .contains(&l.blend.as_str())
                || l.effects.iter().any(|e| {
                    ["color_balance", "hsl", "bloom", "liquify"].contains(&e.kind.as_str())
                })
        }) {
            4
        } else if doc.layers.iter().any(|l| {
            !l.effects.is_empty()
                || l.mask
                    .as_ref()
                    .is_some_and(|m| m.steps.iter().any(|s| !s.settings.is_null()))
        }) {
            3
        } else if doc.layers.iter().any(|l| {
            l.mask
                .as_ref()
                .is_some_and(|m| m.steps.iter().any(|s| s.kind == "blur"))
        }) {
            2
        } else {
            1
        },
        standard_hash: layer_hash ^ composite_hash,
        document: doc,
    };
    let total = layer_length
        .checked_add(composite_length)
        .ok_or("Photoshop section length overflow")?;
    if spool.stream_position().map_err(|e| e.to_string())? != total
        || spool.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != total
    {
        return Err("Photoshop standard spool length changed".into());
    }
    let (compressed_length, compressed_hash, spool_length) = {
        let mut source = StandardWriter::new(&mut *spool, check);
        // Count the existing standard bytes against the same spool/file limit;
        // the fresh hash covers only the appended compressed source segment.
        source.bytes = total;
        let z = BoundedWriter {
            // Batch the compressor's small fragments before guarded writes too.
            inner: ZlibEncoder::new(
                std::io::BufWriter::with_capacity(64 * 1024, source),
                Compression::fast(),
            ),
            written: 0,
            limit: if high {
                512 * 1024 * 1024
            } else {
                128 * 1024 * 1024
            },
            check,
            error: "Editable PSD source data exceeds its bounded serialization budget",
        };
        // JSON emits tiny fragments; check shared state at bounded input writes
        // instead of contending with the UI for every number and delimiter.
        let mut json = std::io::BufWriter::with_capacity(64 * 1024, z);
        serde_json::to_writer(&mut json, &embedded).map_err(|e| e.to_string())?;
        let z = json.into_inner().map_err(|e| e.to_string())?;
        let mut buffered = z.inner.finish().map_err(|e| e.to_string())?;
        buffered.flush().map_err(|e| e.to_string())?;
        let source = buffered.into_inner().map_err(|e| e.to_string())?;
        check()?;
        (source.bytes - total, source.hash, source.bytes)
    };
    progress("Assembling verified Photoshop output", 0, 0)?;
    check()?;
    let payload_length = compressed_length
        .checked_add(4)
        .and_then(|n| u32::try_from(n).ok())
        .ok_or("PSD resource length overflow")?;
    let mut resource_start = vec![];
    resource_start.extend_from_slice(b"8BIM");
    u16b(&mut resource_start, 4000);
    resource_start.push(12);
    resource_start.extend_from_slice(b"PeerBrush.v1");
    resource_start.push(0);
    u32b(&mut resource_start, payload_length);
    resource_start.extend_from_slice(b"PBR1");
    let mut resource_end = vec![];
    if payload_length % 2 != 0 {
        resource_end.push(0);
    }
    if let Some(resolution) = &doc.psd_resolution {
        resource_end.extend_from_slice(&resolution_resource(resolution));
    }
    if let Some(scale) = &doc.psd_print_scale {
        resource_end.extend_from_slice(&print_scale_resource(scale));
    }
    if let Some(aspect) = &doc.psd_pixel_aspect {
        resource_end.extend_from_slice(&pixel_aspect_resource(aspect));
    }
    if let Some(grid) = &doc.psd_grid {
        resource_end.extend_from_slice(&grid_resource(grid));
    }
    if let Some(seed) = &doc.psd_layer_id_seed {
        resource_end.extend_from_slice(&layer_id_seed_resource(seed));
    }
    if let Some(flags) = &doc.psd_print_flags {
        resource_end.extend_from_slice(&print_flags_resource(flags));
    }
    if let Some(info) = &doc.psd_print_flags_info {
        resource_end.extend_from_slice(&print_flags_info_resource(info));
    }
    if let Some(flag) = &doc.psd_copyright {
        resource_end.extend_from_slice(&copyright_resource(flag));
    }
    if let Some(file_info) = &doc.psd_file_info {
        resource_end.extend_from_slice(file_info);
    }
    if let Some(angle) = &doc.psd_global_angle {
        resource_end.extend_from_slice(&global_angle_resource(angle));
    }
    if let Some(altitude) = &doc.psd_global_altitude {
        resource_end.extend_from_slice(&global_altitude_resource(altitude));
    }
    if doc.psd_intentionally_untagged {
        resource_end.extend_from_slice(UNTAGGED_PROFILE_RECORD);
    }
    let mut header = b"8BPS".to_vec();
    u16b(&mut header, if layout.large() { 2 } else { 1 });
    header.extend_from_slice(&[0; 6]);
    u16b(&mut header, 4);
    u32b(&mut header, h);
    u32b(&mut header, w);
    u16b(&mut header, doc.bit_depth);
    u16b(&mut header, 3);
    u32b(&mut header, 0);
    if doc.srgb_tagged {
        let profile = crate::color_profile::srgb_profile();
        resource_end.extend_from_slice(b"8BIM");
        u16b(&mut resource_end, 1039);
        resource_end.extend([0; 2]);
        block(&mut resource_end, profile);
        if profile.len() % 2 != 0 {
            resource_end.push(0);
        }
    }
    let resource_length = resource_start
        .len()
        .checked_add(
            usize::try_from(compressed_length).map_err(|_| "PSD resource length overflow")?,
        )
        .and_then(|n| n.checked_add(resource_end.len()))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or("PSD resource length overflow")?;
    u32b(&mut header, resource_length);
    let mut out = BoundedWriter {
        inner: output,
        written: 0,
        limit: 256 * 1024 * 1024,
        check,
        error: "Photoshop output exceeds the initial 256 MiB file limit",
    };
    out.write_all(&header).map_err(|e| e.to_string())?;
    out.write_all(&resource_start).map_err(|e| e.to_string())?;
    if spool.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != spool_length {
        return Err("Photoshop standard spool length changed".into());
    }
    spool
        .seek(SeekFrom::Start(total))
        .map_err(|e| e.to_string())?;
    let mut copy = [0u8; 64 * 1024];
    copy_standard(
        spool,
        &mut out,
        compressed_length,
        compressed_hash,
        &mut copy,
        check,
    )?;
    out.write_all(&resource_end).map_err(|e| e.to_string())?;
    layout.write_length(&mut out, layer_length)?;
    if spool.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != spool_length {
        return Err("Photoshop standard spool length changed".into());
    }
    spool.rewind().map_err(|e| e.to_string())?;
    copy_standard(spool, &mut out, layer_length, layer_hash, &mut copy, check)?;
    copy_standard(
        spool,
        &mut out,
        composite_length,
        composite_hash,
        &mut copy,
        check,
    )?;
    if spool.seek(SeekFrom::End(0)).map_err(|e| e.to_string())? != spool_length {
        return Err("Photoshop standard spool length changed".into());
    }
    out.flush().map_err(|e| e.to_string())?;
    check()
}

#[cfg(test)]
fn decode_channel(data: &[u8], w: u32, h: u32, layout: Layout) -> Result<Vec<u8>, String> {
    let count = (w as usize)
        .checked_mul(h as usize)
        .ok_or("Channel overflow")?;
    if count > 32 * 1024 * 1024 {
        return Err("PSD channel exceeds allocation budget".into());
    }
    let mut r = Cursor::new(data);
    let kind = r.u16()?;
    match kind {
        0 => Ok(r.bytes(count)?.to_vec()),
        1 => {
            let lengths = row_lengths(
                &mut std::io::Cursor::new(&data[r.pos..]),
                h as usize,
                w as usize,
                layout,
            )?;
            r.bytes(h as usize * if layout.large() { 4 } else { 2 })?;
            let mut out = Vec::with_capacity(count);
            for n in lengths {
                let row = r.bytes(n)?;
                let start = out.len();
                let mut i = 0;
                while i < row.len() {
                    let n = row[i] as i8;
                    i += 1;
                    match n {
                        0..=127 => {
                            let len = n as usize + 1;
                            if i + len > row.len() {
                                return Err("Invalid RLE packet".into());
                            }
                            out.extend_from_slice(&row[i..i + len]);
                            i += len;
                        }
                        -127..=-1 => {
                            let value = *row.get(i).ok_or("Invalid RLE repeat")?;
                            i += 1;
                            out.resize(out.len() + (-n as i16 + 1) as usize, value);
                        }
                        _ => {}
                    }
                    if out.len() - start > w as usize {
                        return Err("RLE row overflow".into());
                    }
                }
                if out.len() - start != w as usize {
                    return Err("Invalid RLE row width".into());
                }
            }
            Ok(out)
        }
        2 | 3 => {
            let mut out = vec![];
            ZlibDecoder::new(&data[2..])
                .take(count as u64 + 1)
                .read_to_end(&mut out)
                .map_err(|e| e.to_string())?;
            if out.len() != count {
                return Err("Invalid ZIP channel size".into());
            }
            if kind == 3 && w > 0 {
                for row in out.chunks_exact_mut(w as usize) {
                    for i in 1..row.len() {
                        row[i] = row[i].wrapping_add(row[i - 1]);
                    }
                }
            }
            Ok(out)
        }
        _ => Err("Unknown PSD compression".into()),
    }
}
/// Bounded source diagnostics, not decoded editable Photoshop layers.
#[derive(Clone, Debug, serde::Serialize)]
pub struct SourceLayerInfo {
    pub source_index: usize,
    pub parent_index: Option<usize>,
    pub name: String,
    pub kind: String,
    /// Original document coordinates, [left, top, right, bottom].
    pub bounds: [i32; 4],
    pub visible: bool,
    pub has_mask: bool,
    pub tags: Vec<String>,
    pub warnings: Vec<String>,
    pub details_truncated: bool,
}
pub(crate) type ProtectedStructure = memory36::Admitted<Vec<SourceLayerInfo>>;

fn unsupported_layer_source(key: &[u8]) -> Option<&'static str> {
    match key {
        b"TySh" | b"tySh" => Some("PeerBrush cannot preserve this layer as editable Photoshop text."),
        b"SoLd" | b"SoLE" | b"PlLd" | b"plLd" => {
            Some("PeerBrush cannot preserve this layer as a Photoshop Smart Object or placed layer.")
        }
        b"artb" | b"artd" | b"abdd" => Some("Photoshop artboard data requires read-only protection."),
        b"FXid" | b"FEid" => Some("Photoshop filter effects require read-only protection."),
        _ => None,
    }
}

fn diagnostic_text(value: &str, truncated: &mut bool) -> String {
    let text = value.chars().take(1024).collect::<String>();
    *truncated |= text.len() != value.len();
    text
}
struct Record {
    source: SourceLayerInfo,
    layer: Layer,
    channels: Vec<(i16, usize)>,
    bounds: [i32; 4],
    mask_bounds: Option<[i32; 4]>,
    mask_pair: Option<MaskHeader>,
    mask_default: u8,
    mask_density: Option<u8>,
    mask_parameters: bool,
    mask_enabled: bool,
    mask_linked: bool,
    retain_mask_original: bool,
    section: u32,
    clipped: bool,
}

fn mask_overlap(width: u32, height: u32, frame: [u32; 2], offset: [i32; 2]) -> [i32; 4] {
    let [dx, dy] = offset;
    [
        (-dx).clamp(0, width as i32),
        (-dy).clamp(0, height as i32),
        (frame[0] as i32 - dx).clamp(0, width as i32),
        (frame[1] as i32 - dy).clamp(0, height as i32),
    ]
}

fn mask_digest(
    [top, left, _, _]: [i32; 4],
    default: u8,
    depth: u16,
    spatial: bool,
    stream: impl FnOnce(&mut dyn FnMut(u32, &[u8])) -> Result<(), String>,
) -> Result<ring::digest::Digest, String> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let step = usize::from(depth / 8);
    let default = [default; 2];
    stream(&mut |y, row| {
        if !spatial {
            digest.update(row);
            return;
        }
        // Different extents are equivalent only if their nondefault samples
        // match at the same document coordinates, even outside the layer.
        let mut samples = row.chunks_exact(step);
        let differs = |sample: &[u8]| sample != &default[..step];
        let Some(first) = samples.position(differs) else { return; };
        let last = samples.rposition(differs).map_or(first, |index| first + 1 + index);
        let count = last - first + 1;
        for value in [top + y as i32, left + first as i32, count as i32] {
            digest.update(&value.to_be_bytes());
        }
        digest.update(&row[first * step..(last + 1) * step]);
    })?;
    Ok(digest.finish())
}

fn mask_payloads_match(
    rendered: &[u8], user: &[u8], header: MaskHeader, depth: u16, layout: Layout,
    control: &crate::loading::Control,
) -> Result<bool, String> {
    let rendered_bounds = header.rendered_bounds.ok_or("Missing rendered Photoshop mask bounds")?;
    let spatial = rendered_bounds != header.bounds;
    let digest = |data: &[u8], bounds: [i32; 4]| {
        let [top, left, bottom, right] = bounds;
        let width = u32::try_from(right - left).map_err(|_| "Invalid Photoshop mask width")?;
        let height = u32::try_from(bottom - top).map_err(|_| "Invalid Photoshop mask height")?;
        mask_digest(bounds, header.default, depth, spatial, |row| {
            stream_mask_rows(data, width, height, depth, layout, control, true, row)
        })
    };
    Ok(digest(rendered, rendered_bounds)?.as_ref() == digest(user, header.bounds)?.as_ref())
}

const DECODED_STORAGE_BUDGET: &str = "PSD decoded tiles exceed the 512 MiB raster-source budget";

fn decoded_storage(records: &[Record], w: u32, h: u32, depth: u16) -> Result<(), String> {
    let tile = crate::raster::TILE as i64;
    let unit = (tile * tile * 4 * if depth == 16 { 2 } else { 1 }) as u64;
    let mut bytes = 0u64;
    for rec in records {
        let lw = (rec.bounds[3] - rec.bounds[1]) as i64;
        let lh = (rec.bounds[2] - rec.bounds[0]) as i64;
        if rec.channels.iter().any(|(c, _)| (-1..=2).contains(c)) {
            bytes += (((lw + tile - 1) / tile) * ((lh + tile - 1) / tile)) as u64 * unit;
        }
        if rec.channels.iter().any(|(c, _)| *c == -2) {
            if let Some(b) = rec.mask_bounds {
                if b[3] < b[1] || b[2] < b[0] {
                    return Err("Invalid mask bounds".into());
                }
                let frame = if ["group", "adjustment", "fill"].contains(&rec.layer.kind.as_str()) {
                    [w, h]
                } else {
                    [lw as u32, lh as u32]
                };
                let (mw, mh) = ((b[3] - b[1]) as u32, (b[2] - b[0]) as u32);
                let offset = [b[1] - rec.bounds[1], b[0] - rec.bounds[0]];
                let [left, top, right, bottom] = mask_overlap(mw, mh, frame, offset).map(i64::from);
                if right > left && bottom > top {
                    let [dx, dy] = offset.map(i64::from);
                    bytes += (((right + dx + tile - 1) / tile - (left + dx) / tile)
                        * ((bottom + dy + tile - 1) / tile - (top + dy) / tile))
                        as u64
                        * unit;
                }
                if rec.retain_mask_original {
                    let tile = crate::raster::TILE;
                    bytes += u64::from(mw.div_ceil(tile)) * u64::from(mh.div_ceil(tile)) * unit;
                }
            }
        }
        if bytes > 512 * 1024 * 1024 {
            return Err(DECODED_STORAGE_BUDGET.into());
        }
    }
    Ok(())
}

struct HashedReader<'a, R> {
    reader: &'a mut R,
    hash: u64,
}
impl<R: Read> Read for HashedReader<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(out)?;
        for b in &out[..n] {
            self.hash = (self.hash ^ *b as u64).wrapping_mul(0x100000001b3);
        }
        Ok(n)
    }
}
fn stream_composite<R: Read>(
    input: &mut R,
    w: u32,
    h: u32,
    depth: u16,
    count: usize,
    transparency: bool,
    layout: Layout,
    control: &crate::loading::Control,
) -> Result<(Raster, bool), String> {
    let mut kind = [0; 2];
    input.read_exact(&mut kind).map_err(|e| e.to_string())?;
    let kind = u16::from_be_bytes(kind);
    if kind > 3 {
        return Err("Unsupported PSD composite compression".into());
    }
    let lengths = if kind == 1 {
        row_lengths(
            input,
            h as usize * count,
            w as usize * if depth == 16 { 2 } else { 1 },
            layout,
        )?
    } else {
        vec![]
    };
    let mut raster = Raster::new_depth(w, h, depth);
    if kind >= 2 {
        let mut zip = ZlibDecoder::new(input);
        composite_rows(
            &mut zip,
            &mut raster,
            count,
            kind,
            &lengths,
            transparency,
            0,
            true,
            "Decoding saved image",
            control,
        )?;
        let mut extra = [0; 1];
        if zip.read(&mut extra).map_err(|e| e.to_string())? != 0 {
            return Err("Wrong PSD composite size".into());
        }
    } else {
        composite_rows(
            input,
            &mut raster,
            count,
            kind,
            &lengths,
            transparency,
            0,
            true,
            "Decoding saved image",
            control,
        )?;
    }
    restore_saved_composite(raster, control)
}

fn restore_saved_composite(
    mut raster: Raster,
    control: &crate::loading::Control,
) -> Result<(Raster, bool), String> {
    let (w, h, depth) = (raster.width, raster.height, raster.depth);
    control.progress("Resolving saved composite transparency", 0, 1)?;
    // PSD compatibility colors are composited over white. Restore straight RGB in tiles.
    let max = if depth == 16 { 65535i64 } else { 255 };
    let mut white_valid = true;
    for (&(tx, ty), tile) in &mut raster.samples16 {
        control.check()?;
        for (index, pixel) in std::sync::Arc::make_mut(tile)
            .chunks_exact_mut(4)
            .enumerate()
        {
            let valid = restore_white_matte(pixel, max);
            if tx * crate::raster::TILE + index as u32 % crate::raster::TILE < w
                && ty * crate::raster::TILE + index as u32 / crate::raster::TILE < h
            {
                white_valid &= valid;
            }
        }
    }
    for (&(tx, ty), tile) in &mut raster.tiles {
        control.check()?;
        for (index, pixel) in std::sync::Arc::make_mut(tile)
            .chunks_exact_mut(4)
            .enumerate()
        {
            let mut p = pixel.try_into().map(|p: [u8; 4]| p.map(u16::from)).unwrap();
            let valid = restore_white_matte(&mut p, max);
            if tx * crate::raster::TILE + index as u32 % crate::raster::TILE < w
                && ty * crate::raster::TILE + index as u32 / crate::raster::TILE < h
            {
                white_valid &= valid;
            }
            pixel.copy_from_slice(&p.map(|v| v as u8));
        }
    }
    Ok((raster, white_valid))
}

fn restore_white_matte(pixel: &mut [u16], max: i64) -> bool {
    let a = i64::from(pixel[3]);
    let valid = pixel[..3].iter().all(|v| i64::from(*v) >= max - a);
    for v in &mut pixel[..3] {
        *v = if a == 0 {
            0
        } else {
            (((i64::from(*v) + a - max) * max + a / 2) / a).clamp(0, max) as u16
        };
    }
    valid
}
fn composite_rows<R: Read>(
    input: &mut R,
    raster: &mut Raster,
    count: usize,
    kind: u16,
    lengths: &[usize],
    transparency: bool,
    channel: usize,
    initialize: bool,
    stage: &str,
    control: &crate::loading::Control,
) -> Result<(), String> {
    use crate::raster::TILE;
    use std::sync::Arc;
    let (w, h, depth) = (raster.width, raster.height, raster.depth);
    let row_bytes = w as usize * if depth == 16 { 2 } else { 1 };
    let mut row = vec![0; row_bytes];
    let mut packed = vec![];
    for c in 0..count {
        for y in 0..h {
            if y % 16 == 0 {
                control.progress(stage, c * h as usize + y as usize, count * h as usize)?;
            }
            if kind == 1 {
                packed.resize(lengths[c * h as usize + y as usize], 0);
                input.read_exact(&mut packed).map_err(|e| e.to_string())?;
                let (mut i, mut o) = (0, 0);
                while i < packed.len() {
                    let packet = packed[i] as i8;
                    i += 1;
                    match packet {
                        0..=127 => {
                            let n = packet as usize + 1;
                            if i + n > packed.len() || o + n > row.len() {
                                return Err("Invalid PSD RLE literal".into());
                            }
                            row[o..o + n].copy_from_slice(&packed[i..i + n]);
                            i += n;
                            o += n;
                        }
                        -127..=-1 => {
                            let n = (-packet as i16 + 1) as usize;
                            let value = *packed.get(i).ok_or("Invalid PSD RLE repeat")?;
                            i += 1;
                            if o + n > row.len() {
                                return Err("PSD RLE row overflow".into());
                            }
                            row[o..o + n].fill(value);
                            o += n;
                        }
                        _ => {}
                    }
                }
                if o != row.len() {
                    return Err("Invalid PSD RLE row width".into());
                }
            } else {
                input.read_exact(&mut row).map_err(|e| e.to_string())?;
            }
            if kind == 3 {
                if depth == 16 {
                    let mut previous = 0u16;
                    for word in row.chunks_exact_mut(2) {
                        previous = previous.wrapping_add(u16::from_be_bytes([word[0], word[1]]));
                        word.copy_from_slice(&previous.to_be_bytes());
                    }
                } else {
                    for x in 1..row.len() {
                        row[x] = row[x].wrapping_add(row[x - 1]);
                    }
                }
            }
            if c + channel == 3 && !transparency {
                continue;
            }
            for tx in 0..w.div_ceil(TILE) {
                let start = tx * TILE;
                let end = (start + TILE).min(w);
                let index = ((y % TILE) * TILE * 4) as usize;
                if depth == 16 {
                    let tile = raster
                        .samples16
                        .entry((tx, y / TILE))
                        .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
                    let values = Arc::make_mut(tile);
                    for x in start..end {
                        let i = index + ((x - start) * 4) as usize;
                        if initialize && c == 0 {
                            values[i + 3] = 65535;
                        }
                        values[i + c + channel] =
                            u16::from_be_bytes([row[x as usize * 2], row[x as usize * 2 + 1]]);
                    }
                } else {
                    let tile = raster
                        .tiles
                        .entry((tx, y / TILE))
                        .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
                    let values = Arc::make_mut(tile);
                    for x in start..end {
                        let i = index + ((x - start) * 4) as usize;
                        if initialize && c == 0 {
                            values[i + 3] = 255;
                        }
                        values[i + c + channel] = row[x as usize];
                    }
                }
            }
        }
    }
    Ok(())
}

pub fn decode(data: &[u8]) -> Result<Document, String> {
    decode_slice(data, &crate::loading::Control::default())
}
pub(crate) fn decode_slice(
    data: &[u8],
    control: &crate::loading::Control,
) -> Result<Document, String> {
    if data.get(24..26) == Some(&[0, 1]) {
        return indexed::decode(&mut std::io::Cursor::new(data), control);
    }
    decode_sections(&mut std::io::Cursor::new(data), control, borrowed_section)
}
fn stream_layer_channel(
    bytes: &[u8],
    raster: &mut Raster,
    channel: usize,
    initialize: bool,
    layout: Layout,
    control: &crate::loading::Control,
) -> Result<(), String> {
    if bytes.is_empty() && (raster.width == 0 || raster.height == 0) {
        return control.check();
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let mut kind = [0; 2];
    cursor.read_exact(&mut kind).map_err(|e| e.to_string())?;
    let kind = u16::from_be_bytes(kind);
    if kind > 3 {
        return Err("Unsupported PSD channel compression".into());
    }
    if (raster.width == 0 || raster.height == 0) && kind >= 2 {
        let mut inflater = flate2::Decompress::new(true);
        let payload = &bytes[2..];
        let status = inflater
            .decompress(payload, &mut [0; 1], flate2::FlushDecompress::Finish)
            .map_err(|e| e.to_string())?;
        if status != flate2::Status::StreamEnd
            || inflater.total_out() != 0
            || inflater.total_in() != payload.len() as u64
        {
            return Err("Invalid empty Photoshop ZIP channel".into());
        }
        return control.check();
    }
    let mut lengths = vec![];
    if kind == 1 {
        lengths = row_lengths(
            &mut cursor,
            raster.height as usize,
            raster.width as usize * if raster.depth == 16 { 2 } else { 1 },
            layout,
        )?;
    }
    if kind >= 2 {
        let mut zip = ZlibDecoder::new(cursor);
        composite_rows(
            &mut zip,
            raster,
            1,
            kind,
            &lengths,
            true,
            channel,
            initialize,
            "Decoding native layer pixels",
            control,
        )?;
        let mut extra = [0; 1];
        if zip.read(&mut extra).map_err(|e| e.to_string())? != 0 {
            return Err("Wrong PSD channel size".into());
        }
    } else {
        composite_rows(
            &mut cursor,
            raster,
            1,
            kind,
            &lengths,
            true,
            channel,
            initialize,
            "Decoding native layer pixels",
            control,
        )?;
        if (raster.width == 0 || raster.height == 0) && cursor.position() != bytes.len() as u64 {
            return Err("Unexpected data in an empty Photoshop layer channel".into());
        }
    }
    Ok(())
}
#[cfg(test)]
thread_local! {
    static MASK_ROW_OBSERVER: std::cell::RefCell<Option<Box<dyn FnMut(u32, usize, usize)>>> =
        const { std::cell::RefCell::new(None) };
}

fn stream_mask_channel(
    data: &[u8],
    w: u32,
    h: u32,
    pixels: &mut Raster,
    offset: [i32; 2],
    layout: Layout,
    control: &crate::loading::Control,
) -> Result<(), String> {
    let depth = pixels.depth;
    let left = (-i64::from(offset[0])).clamp(0, i64::from(w)) as u32;
    let right = (i64::from(pixels.width) - i64::from(offset[0])).clamp(0, i64::from(w)) as u32;
    stream_mask_rows(data, w, h, depth, layout, control, false, |y, row| {
        let yy = i64::from(y) + i64::from(offset[1]);
        if (0..i64::from(pixels.height)).contains(&yy) {
            for x in left..right {
                let v = if depth == 16 {
                    u16::from_be_bytes([row[x as usize * 2], row[x as usize * 2 + 1]])
                } else {
                    u16::from(row[x as usize]) * 257
                };
                pixels.set16(x as i32 + offset[0], yy as i32, [v, v, v, 65535]);
            }
        }
    })
}

fn stream_mask_rows(
    data: &[u8],
    w: u32,
    h: u32,
    depth: u16,
    layout: Layout,
    control: &crate::loading::Control,
    exact: bool,
    mut output: impl FnMut(u32, &[u8]),
) -> Result<(), String> {
    let high = depth == 16;
    let count = (w as usize)
        .checked_mul(h as usize)
        .ok_or("Channel overflow")?;
    // Retain each native depth's existing mask-plane admission, even though the
    // decoded plane is no longer materialized.
    if high {
        if count.checked_mul(2).ok_or("PSD channel size overflow")? > 256 * 1024 * 1024 {
            return Err("16-bit PSD composite exceeds the 256 MiB decoded-image budget".into());
        }
    } else if count > 32 * 1024 * 1024 {
        return Err("PSD channel exceeds allocation budget".into());
    }
    control.check()?;
    let mut cursor = Cursor::new(data);
    let kind = cursor.u16()?;
    if kind > 3 {
        return Err(if high {
            "Unsupported 16-bit composite compression"
        } else {
            "Unknown PSD compression"
        }
        .into());
    }
    let row_bytes = (w as usize)
        .checked_mul(if high { 2 } else { 1 })
        .ok_or("PSD row size overflow")?;
    let lengths = if kind == 1 {
        let lengths = row_lengths(
            &mut std::io::Cursor::new(&data[cursor.pos..]),
            h as usize,
            row_bytes,
            layout,
        )?;
        cursor.bytes(h as usize * if layout.large() { 4 } else { 2 })?;
        lengths
    } else {
        vec![]
    };
    let mut zip = (kind >= 2).then(|| ZlibDecoder::new(&data[cursor.pos..]));
    let row_padding = kind == 0 && row_bytes % 2 == 1
        && (row_bytes + 1).checked_mul(h as usize) == Some(data.len() - cursor.pos);
    let mut row = vec![0; row_bytes];
    for y in 0..h {
        #[cfg(test)]
        MASK_ROW_OBSERVER.with(|observer| {
            if let Some(mut observe) = observer.take() {
                observe(y, row.len(), lengths.len());
                observer.replace(Some(observe));
            }
        });
        control.check()?;
        if y % 16 == 0 {
            control.progress("Decoding native mask pixels", y as usize, h as usize)?;
        }
        if kind == 1 {
            // Borrow the bounded encoded packet row from the already-held layer
            // section; only the decoded native row needs a transfer buffer.
            let packed = cursor.bytes(lengths[y as usize])?;
            let (mut i, mut o) = (0, 0);
            while i < packed.len() {
                let packet = packed[i] as i8;
                i += 1;
                match packet {
                    0..=127 => {
                        let n = packet as usize + 1;
                        if i + n > packed.len() || o + n > row.len() {
                            return Err("Invalid PSD mask RLE literal".into());
                        }
                        row[o..o + n].copy_from_slice(&packed[i..i + n]);
                        i += n;
                        o += n;
                    }
                    -127..=-1 => {
                        let n = (-packet as i16 + 1) as usize;
                        let value = *packed.get(i).ok_or("Invalid PSD mask RLE repeat")?;
                        i += 1;
                        if o + n > row.len() {
                            return Err("PSD mask RLE row overflow".into());
                        }
                        row[o..o + n].fill(value);
                        o += n;
                    }
                    _ => {}
                }
            }
            if o != row.len() {
                return Err("Invalid PSD mask RLE row width".into());
            }
        } else if let Some(zip) = &mut zip {
            zip.read_exact(&mut row).map_err(|e| e.to_string())?;
        } else {
            row.copy_from_slice(cursor.bytes(row_bytes)?);
            if row_padding {
                cursor.bytes(1)?;
            }
        }
        if kind == 3 {
            if high {
                let mut previous = 0u16;
                for word in row.chunks_exact_mut(2) {
                    previous = previous.wrapping_add(u16::from_be_bytes([word[0], word[1]]));
                    word.copy_from_slice(&previous.to_be_bytes());
                }
            } else {
                for x in 1..row.len() {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
            }
        }
        output(y, &row);
    }
    if let Some(zip) = &mut zip {
        // Reading through EOF also requires zlib's stream end/checksum. A short
        // stream, extra decoded byte or corrupt trailer cannot authorize a mask.
        let mut extra = [0; 1];
        if zip.read(&mut extra).map_err(|e| e.to_string())? != 0 {
            return Err("Invalid ZIP mask channel size".into());
        }
        if exact && zip.total_in() != (data.len() - cursor.pos) as u64 {
            return Err("Extra ZIP mask channel bytes".into());
        }
    } else if exact && cursor.pos != data.len() {
        return Err("Extra Photoshop mask channel bytes".into());
    }
    control.check()
}

/// Stream the composite into native tiles; never buffer the complete encoded file.
pub fn decode_reader<R: Read + std::io::Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
) -> Result<Document, String> {
    indexed::decode(reader, control)
}

pub(crate) fn decode_held_reader<R: Read + Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
    version: crate::file_safety::Version,
) -> Result<(Document, Option<ConventionalComposite>), String> {
    indexed::decode_bound(reader, control, Some(version.sha256()))
}

pub(crate) fn decode_unpaired_autosave<R: Read + Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
    version: crate::file_safety::Version,
) -> Result<Document, String> {
    let document = indexed::decode_unpaired(reader, control, version.sha256())?;
    if document.read_only {
        return Err("Unpaired recovery cannot be restored as editable work; the file is retained".into());
    }
    Ok(document)
}
fn decode_sections<'a, R: Read + std::io::Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
    mut section: impl FnMut(
        &mut R,
        u64,
        bool,
        &crate::loading::Control,
        &str,
    ) -> Result<Cow<'a, [u8]>, String>,
) -> Result<Document, String> {
    use std::io::SeekFrom;
    let length = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if length > 256 * 1024 * 1024 {
        return Err("Photoshop file limit is 256 MiB".into());
    }
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    control.progress("Reading PSD header", 0, 1)?;
    let mut header = [0; 26];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    let mut r = Cursor::new(&header);
    if r.bytes(4)? != b"8BPS" {
        return Err("Invalid Photoshop document signature".into());
    }
    let layout = match r.u16()? {
        1 => Layout::Psd,
        2 => Layout::Psb,
        _ => return Err("Only PSD v1 and PSB v2 are supported".into()),
    };
    r.bytes(6)?;
    let channel_count = r.u16()? as usize;
    let h = r.u32()?;
    let w = r.u32()?;
    check_size(w, h)?;
    let depth = r.u16()?;
    let mode = r.u16()?;
    if ![8, 16].contains(&depth) || mode != 3 || !(3..=4).contains(&channel_count) {
        return Err("PeerBrush opens 8-bit and 16-bit RGB PSD/PSB documents (3 or 4 channels); unsupported Photoshop source features remain protected".into());
    }
    section(reader, length, false, control, "Reading color data")?;
    let resources = section(
        reader,
        length,
        false,
        control,
        "Reading Photoshop resources",
    )?;
    let layer_section = section(
        reader,
        length,
        layout.large(),
        control,
        "Reading layer sources",
    )?;
    let transparency = merged_transparency(&layer_section, depth, layout)?;
    #[cfg(test)]
    observe_open_phase("sections_read", resources.len() + layer_section.len());
    let mut input = HashedReader {
        reader,
        hash: 0xcbf29ce484222325,
    };
    let (merged, merged_white_valid) = stream_composite(
        &mut input,
        w,
        h,
        depth,
        channel_count,
        transparency,
        layout,
        control,
    )?;
    let mut remaining = [0; 65536];
    loop {
        control.check()?;
        if input.read(&mut remaining).map_err(|e| e.to_string())? == 0 {
            break;
        }
    }
    #[cfg(test)]
    observe_open_phase("composite_decoded", 0);
    decode_parts(
        &resources,
        &layer_section,
        w,
        h,
        depth,
        merged,
        merged_white_valid,
        input.hash,
        layout,
        control,
        None,
        false,
        false,
    )
}
fn section_size<R: Read + std::io::Seek>(
    reader: &mut R,
    length: u64,
    large: bool,
) -> Result<usize, String> {
    let mut bytes = [0; 8];
    let size = if large { 8 } else { 4 };
    reader
        .read_exact(&mut bytes[..size])
        .map_err(|e| e.to_string())?;
    let n = if large {
        u64::from_be_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes[..4].try_into().unwrap()) as u64
    };
    let n = usize::try_from(n).map_err(|_| "Photoshop section length overflow")?;
    let position = reader.stream_position().map_err(|e| e.to_string())?;
    if n as u64 > length.saturating_sub(position) {
        return Err("Truncated PSD section".into());
    }
    Ok(n)
}
fn borrowed_section<'a>(
    reader: &mut std::io::Cursor<&'a [u8]>,
    length: u64,
    large: bool,
    control: &crate::loading::Control,
    stage: &str,
) -> Result<Cow<'a, [u8]>, String> {
    let n = section_size(reader, length, large)?;
    let start = usize::try_from(reader.position()).map_err(|_| "PSD length overflow")?;
    let end = start.checked_add(n).ok_or("PSD length overflow")?;
    let data = *reader.get_ref();
    let bytes = data.get(start..end).ok_or("Truncated PSD section")?;
    if n != 0 {
        control.progress(stage, 0, n)?;
    }
    reader.set_position(end as u64);
    control.progress(stage, n, n)?;
    Ok(Cow::Borrowed(bytes))
}
#[cfg(test)]
fn read_section<R: Read + std::io::Seek>(
    reader: &mut R,
    length: u64,
    large: bool,
    control: &crate::loading::Control,
    stage: &str,
) -> Result<Vec<u8>, String> {
    let n = section_size(reader, length, large)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(n)
        .map_err(|_| "PSD section allocation failed")?;
    for offset in (0..n).step_by(1024 * 1024) {
        control.progress(stage, offset, n)?;
        let end = (offset + 1024 * 1024).min(n);
        bytes.resize(end, 0);
        reader
            .read_exact(&mut bytes[offset..end])
            .map_err(|e| e.to_string())?;
    }
    control.progress(stage, n, n)?;
    Ok(bytes)
}
// The compact metadata view is parser input only. These methods always read the
// original indexed source; its standard hash must never use the compact view.
trait IndexedChannels {
    fn uses_backing(&self) -> bool;
    fn reserve_interpreted_bytes(&mut self, bytes: usize) -> Result<(), String>;
    fn backed_layer(
        &mut self,
        record: usize,
        layer: &str,
        raster: &Raster,
        require_binary_alpha: bool,
        control: &crate::loading::Control,
    ) -> Result<bool, String>;
    fn section_hash(&self) -> u64;
    #[cfg(test)]
    fn embedded_len(&self) -> usize;
    fn restore(
        &mut self,
        limit: usize,
        control: &crate::loading::Control,
    ) -> Result<Embedded, String>;
    fn layer(
        &mut self,
        record: usize,
        channel: usize,
        raster: &mut Raster,
        target: usize,
        initialize: bool,
        control: &crate::loading::Control,
    ) -> Result<(), String>;
    fn mask(
        &mut self,
        record: usize,
        channel: usize,
        layer: &str,
        raster: &mut Raster,
        offset: [i32; 2],
        control: &crate::loading::Control,
    ) -> Result<(), String>;
    fn same_mask_samples(
        &self, record: usize, first: usize, second: usize, header: MaskHeader, depth: u16,
        control: &crate::loading::Control,
    ) -> Result<bool, String>;
}
const RESOLUTION_RESOURCE_ID: u16 = 1005;
const GRID_RESOURCE_ID: u16 = 1032;
const UNTAGGED_PROFILE_RESOURCE_ID: u16 = 1041;
const UNTAGGED_PROFILE_RECORD: &[u8; 14] = b"8BIM\x04\x11\0\0\0\0\0\x01\x01\0";
const EFFECTS_VISIBILITY_RESOURCE_ID: u16 = 1042;
const LAYER_ID_SEED_RESOURCE_ID: u16 = 1044;
const PRINT_FLAGS_RESOURCE_ID: u16 = 1011;
const PRINT_FLAGS_INFO_RESOURCE_ID: u16 = 10000;
const COPYRIGHT_RESOURCE_ID: u16 = 1034;
const GLOBAL_ANGLE_RESOURCE_ID: u16 = 1037;
const GLOBAL_ALTITUDE_RESOURCE_ID: u16 = 1049;
const VERSION_INFO_RESOURCE_ID: u16 = 1057;
const PRINT_SCALE_RESOURCE_ID: u16 = 1062;
const PIXEL_ASPECT_RESOURCE_ID: u16 = 1064;
#[cfg(test)]
pub(crate) const TEST_PRINT_SCALE: [u8; 14] = [0, 2, 193, 72, 0, 0, 64, 208, 0, 0, 63, 160, 0, 0];
#[cfg(test)]
pub(crate) const TEST_PIXEL_ASPECT: [u8; 12] = [0, 0, 0, 2, 63, 240, 0, 0, 0, 0, 0, 0];
#[cfg(test)]
pub(crate) const TEST_GRID: [u8; 16] = [0, 0, 0, 1, 0, 0, 2, 64, 0, 0, 2, 64, 0, 0, 0, 0];
#[cfg(test)]
pub(crate) const TEST_LAYER_ID_SEED: [u8; 4] = 74042u32.to_be_bytes();
#[cfg(test)]
pub(crate) const TEST_PRINT_FLAGS: [u8; 9] = [1, 0, 1, 0, 1, 0, 1, 0, 1];
#[cfg(test)]
pub(crate) const TEST_PRINT_FLAGS_INFO: [u8; 10] = [0, 1, 1, 0, 0, 0, 1, 17, 0, 2];
const GUIDE_WARNING: &str = "Photoshop guides or extended grid data remain protected";
const STROKE_ALPHA_PROTECTION: &str = "Photoshop strokes with partial source alpha remain protected";
const DOCUMENT_PATH_WARNING: &str = "Photoshop document paths remain protected";
pub(crate) const CONFLICTING_PROFILE_DECLARATIONS: &str =
    "Photoshop ICC profile conflicts with its intentionally untagged declaration";
pub(crate) const SOURCE_WORKING_PROFILE_MISMATCH: &str =
    "Supplementary Photoshop sources disagree with the standard working profile";

fn read_working_srgb(mut reader: impl Read, bytes: u64) -> Result<bool, String> {
    let length = crate::color_profile::srgb_profile().len();
    if bytes != length as u64 {
        return Ok(false);
    }
    let mut profile = vec![0; length];
    reader.read_exact(&mut profile).map_err(|e| e.to_string())?;
    Ok(crate::color_profile::is_srgb_profile(&profile))
}

fn indexed_working_srgb<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<bool, String> {
    let mut profiles = index.resources.iter().filter(|resource| resource.id == 1039);
    let Some(profile) = profiles.next() else { return Ok(false) };
    if profiles.next().is_some() {
        return Err("Duplicate Photoshop ICC profiles are ambiguous".into());
    }
    if !read_working_srgb(source.range(profile.data, check)?, profile.data.bytes)? {
        return Err("Embedded Photoshop color profile remains protected".into());
    }
    Ok(true)
}

fn unsupported_document_resource(id: u16) -> Option<&'static str> {
    match id {
        1014 | 1017 | 1018 | 1066 => Some("Photoshop Duotone settings remain protected"),
        1021 => Some("Photoshop EPS export options remain protected"),
        1022 => Some("Photoshop Quick Mask information remains protected"),
        1025 | 1088 | 2000..=2997 | 2999 | 3000 => Some(DOCUMENT_PATH_WARNING),
        1043 => Some("Photoshop Spot Halftone settings remain protected"),
        1050 => Some("Photoshop Slices remain protected"),
        1065 => Some("Photoshop Layer Comps remain protected"),
        1067 => Some("Photoshop alternate spot colors remain protected"),
        1070 => Some("Photoshop HDR Toning settings remain protected"),
        1075 => Some("Photoshop Timeline remains protected"),
        1076 => Some("Photoshop animation sheet disclosure remains protected"),
        1078 => Some("Photoshop Onion Skins remain protected"),
        1080 => Some("Photoshop Count Tool information remains protected"),
        7000..=7005 => Some("Photoshop ImageReady variables, data sets or rollover state remain protected"),
        8000 => Some("Photoshop Lightroom workflow data remains protected"),
        _ => None,
    }
}

fn effects_visibility_warning(seen: &mut bool, flag: Option<u8>) -> Option<&'static str> {
    if std::mem::replace(seen, true) {
        return Some("Duplicate Photoshop global effects visibility remains protected");
    }
    match flag {
        Some(1) => None,
        Some(0) => Some("Photoshop document-wide hidden effects remain protected"),
        _ => Some("Invalid Photoshop global effects visibility remains protected"),
    }
}

fn add_resource_warning(warnings: &mut Vec<String>, reason: &str) {
    if !warnings.iter().any(|warning| warning == reason) {
        warnings.push(reason.into());
    }
}

fn indexed_document_resource_warnings<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Vec<String>, String> {
    let mut warnings = Vec::new();
    let mut saw_effects_visibility = false;
    for resource in &index.resources {
        let warning = if resource.id == EFFECTS_VISIBILITY_RESOURCE_ID {
            let flag = if resource.data.bytes == 1 {
                let mut byte = [0];
                source.range(resource.data, check)?.read_exact(&mut byte).map_err(|e| e.to_string())?;
                Some(byte[0])
            } else {
                None
            };
            effects_visibility_warning(&mut saw_effects_visibility, flag)
        } else {
            unsupported_document_resource(resource.id)
        };
        if let Some(reason) = warning {
            add_resource_warning(&mut warnings, reason);
        }
    }
    Ok(warnings)
}

fn record_saved_composite(declared: &mut Option<bool>, data: &[u8]) -> Result<(), String> {
    if declared.is_some() {
        return Err("Duplicate Photoshop Version Info makes the saved composite ambiguous".into());
    }
    *declared = match data.get(..5) {
        Some([0, 0, 0, 1, flag @ (0 | 1)]) => Some(*flag == 1),
        _ => return Err("Invalid Photoshop Version Info composite declaration".into()),
    };
    Ok(())
}

fn indexed_saved_composite<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<bool, String> {
    let mut declared = None;
    for resource in index.resources.iter().filter(|resource| resource.id == VERSION_INFO_RESOURCE_ID) {
        let mut prefix = [0; 5];
        let len = resource.data.bytes.min(5) as usize;
        source.range(resource.data, check)?.read_exact(&mut prefix[..len]).map_err(|e| e.to_string())?;
        record_saved_composite(&mut declared, &prefix[..len])?;
    }
    Ok(declared.unwrap_or(true))
}

pub(crate) fn parse_resolution(data: &[u8]) -> Result<[u8; 16], String> {
    let bytes: [u8; 16] = data.try_into().map_err(|_| "Invalid Photoshop print resolution length")?;
    for axis in bytes.chunks_exact(8) {
        let density = i32::from_be_bytes(axis[..4].try_into().unwrap());
        let density_unit = u16::from_be_bytes(axis[4..6].try_into().unwrap());
        let size_unit = u16::from_be_bytes(axis[6..8].try_into().unwrap());
        if density <= 0 || !matches!(density_unit, 1 | 2) || !(1..=5).contains(&size_unit) {
            return Err("Invalid Photoshop print resolution or display unit".into());
        }
    }
    Ok(bytes)
}

pub(crate) fn parse_print_scale(data: &[u8]) -> Result<[u8; 14], String> {
    let bytes: [u8; 14] = data.try_into().map_err(|_| "Invalid Photoshop print scale length")?;
    let style = u16::from_be_bytes(bytes[..2].try_into().unwrap());
    let [x, y, scale] = [2, 6, 10].map(|i| f32::from_be_bytes(bytes[i..i + 4].try_into().unwrap()));
    if style > 2 || !x.is_finite() || !y.is_finite() || !scale.is_finite() || scale < 0.0
        || (style == 2 && scale == 0.0)
    {
        return Err("Invalid Photoshop print scale or placement".into());
    }
    Ok(bytes)
}

pub(crate) fn parse_pixel_aspect(data: &[u8]) -> Result<[u8; 12], String> {
    let bytes: [u8; 12] = data.try_into().map_err(|_| "Invalid Photoshop pixel aspect ratio length")?;
    let version = u32::from_be_bytes(bytes[..4].try_into().unwrap());
    let ratio = f64::from_be_bytes(bytes[4..].try_into().unwrap());
    if !matches!(version, 1 | 2) || !ratio.is_finite() || ratio <= 0. {
        return Err("Invalid Photoshop pixel aspect ratio".into());
    }
    if ratio != 1. {
        return Err("Photoshop non-square pixel aspect ratio remains protected".into());
    }
    Ok(bytes)
}

fn parse_untagged_profile(data: &[u8]) -> Result<[u8; 1], String> {
    match data {
        [flag @ (0 | 1)] => Ok([*flag]),
        _ => Err("Invalid Photoshop untagged profile declaration".into()),
    }
}

fn fixed_resource<const SIZE: usize>(id: u16, data: &[u8]) -> [u8; SIZE] {
    let mut resource = [0; SIZE];
    resource[..4].copy_from_slice(b"8BIM");
    resource[4..6].copy_from_slice(&id.to_be_bytes());
    resource[8..12].copy_from_slice(&(data.len() as u32).to_be_bytes());
    resource[12..12 + data.len()].copy_from_slice(data);
    resource
}

fn resolution_resource(data: &[u8; 16]) -> [u8; 28] {
    fixed_resource(RESOLUTION_RESOURCE_ID, data)
}

fn print_scale_resource(data: &[u8; 14]) -> [u8; 26] {
    fixed_resource(PRINT_SCALE_RESOURCE_ID, data)
}
fn pixel_aspect_resource(data: &[u8; 12]) -> [u8; 24] {
    fixed_resource(PIXEL_ASPECT_RESOURCE_ID, data)
}
fn grid_resource(data: &[u8; 16]) -> [u8; 28] {
    fixed_resource(GRID_RESOURCE_ID, data)
}

fn layer_id_seed_resource(data: &[u8; 4]) -> [u8; 16] {
    fixed_resource(LAYER_ID_SEED_RESOURCE_ID, data)
}

fn parse_layer_id_seed(data: &[u8]) -> Result<[u8; 4], String> {
    data.try_into().map_err(|_| "Invalid Photoshop layer ID seed length".into())
}

fn print_flags_resource(data: &[u8; 9]) -> [u8; 22] {
    fixed_resource(PRINT_FLAGS_RESOURCE_ID, data)
}

fn print_flags_info_resource(data: &[u8; 10]) -> [u8; 22] {
    fixed_resource(PRINT_FLAGS_INFO_RESOURCE_ID, data)
}

fn copyright_resource(data: &[u8; 1]) -> [u8; 14] {
    fixed_resource(COPYRIGHT_RESOURCE_ID, data)
}

fn parse_copyright(data: &[u8]) -> Result<[u8; 1], String> {
    match data {
        [flag @ (0 | 1)] => Ok([*flag]),
        _ => Err("Invalid Photoshop copyright flag".into()),
    }
}

fn global_angle_resource(data: &[u8; 4]) -> [u8; 16] {
    fixed_resource(GLOBAL_ANGLE_RESOURCE_ID, data)
}

fn parse_global_angle(data: &[u8]) -> Result<[u8; 4], String> {
    let angle: [u8; 4] = data.try_into().map_err(|_| "Invalid Photoshop global angle length")?;
    if u32::from_be_bytes(angle) > 359 {
        return Err("Invalid Photoshop global angle".into());
    }
    Ok(angle)
}

fn global_altitude_resource(data: &[u8; 4]) -> [u8; 16] {
    fixed_resource(GLOBAL_ALTITUDE_RESOURCE_ID, data)
}

fn parse_global_altitude(data: &[u8]) -> Result<[u8; 4], String> {
    data.try_into().map_err(|_| "Invalid Photoshop global altitude length".into())
}

fn parse_print_flags(data: &[u8]) -> Result<[u8; 9], String> {
    let flags: [u8; 9] = data.try_into().map_err(|_| "Invalid Photoshop print flags length")?;
    if flags.iter().any(|flag| *flag > 1) {
        return Err("Invalid Photoshop print flags".into());
    }
    Ok(flags)
}

fn parse_print_flags_info(data: &[u8]) -> Result<[u8; 10], String> {
    let info: [u8; 10] = data.try_into().map_err(|_| "Invalid Photoshop print flags information length")?;
    if info[..2] != [0, 1] || info[2] > 1 || info[3] != 0 {
        return Err("Unsupported Photoshop print flags information".into());
    }
    Ok(info)
}

pub(crate) fn parse_grid(data: &[u8]) -> Result<[u8; 16], String> {
    // shortcut: grid-only records, until canvas edits can carry active guides.
    if data.len() > 16 || data.get(12..16).is_some_and(|count| count != [0; 4]) {
        return Err(GUIDE_WARNING.into());
    }
    let bytes: [u8; 16] = data.try_into().map_err(|_| "Invalid Photoshop grid header length")?;
    if bytes[..4] != 1u32.to_be_bytes() {
        return Err("Invalid Photoshop grid header version".into());
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub(crate) struct DocumentResources {
    pub resolution: Option<[u8; 16]>,
    pub scale: Option<[u8; 14]>,
    pub pixel_aspect: Option<[u8; 12]>,
    pub grid: Option<[u8; 16]>,
    pub layer_id_seed: Option<[u8; 4]>,
    pub print_flags: Option<[u8; 9]>,
    pub print_flags_info: Option<[u8; 10]>,
    pub copyright: Option<[u8; 1]>,
    pub global_angle: Option<[u8; 4]>,
    pub global_altitude: Option<[u8; 4]>,
    pub intentionally_untagged: bool,
}

impl DocumentResources {
    const SOURCE36_FIXED_IDS: [u16; 10] = [
        RESOLUTION_RESOURCE_ID,
        PRINT_SCALE_RESOURCE_ID,
        PIXEL_ASPECT_RESOURCE_ID,
        GRID_RESOURCE_ID,
        LAYER_ID_SEED_RESOURCE_ID,
        PRINT_FLAGS_RESOURCE_ID,
        PRINT_FLAGS_INFO_RESOURCE_ID,
        COPYRIGHT_RESOURCE_ID,
        GLOBAL_ANGLE_RESOURCE_ID,
        GLOBAL_ALTITUDE_RESOURCE_ID,
    ];

    pub(crate) fn validate(self) -> Result<(), String> {
        if let Some(resolution) = &self.resolution {
            parse_resolution(resolution)?;
        }
        if let Some(scale) = &self.scale {
            parse_print_scale(scale)?;
        }
        if let Some(aspect) = &self.pixel_aspect {
            parse_pixel_aspect(aspect)?;
        }
        if let Some(grid) = &self.grid {
            parse_grid(grid)?;
        }
        if let Some(flags) = &self.print_flags {
            parse_print_flags(flags)?;
        }
        if let Some(info) = &self.print_flags_info {
            parse_print_flags_info(info)?;
        }
        if let Some(flag) = &self.copyright {
            parse_copyright(flag)?;
        }
        if let Some(angle) = &self.global_angle {
            parse_global_angle(angle)?;
        }
        Ok(())
    }
}

fn inspect_fixed_resource<R: Read + Seek, const N: usize>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    id: u16,
    name: &str,
    parse: fn(&[u8]) -> Result<[u8; N], String>,
    warnings: &mut Vec<String>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<Option<[u8; N]>, String> {
    check()?;
    let mut resources = index.resources.iter().filter(|r| r.id == id);
    let Some(resource) = resources.next() else { return Ok(None) };
    if resources.next().is_some() {
        warnings.push(format!("Duplicate Photoshop {name} remains protected"));
        return Ok(None);
    }
    if resource.data.bytes != N as u64 {
        warnings.push(format!("Invalid Photoshop {name} length"));
        return Ok(None);
    }
    let mut bytes = [0; N];
    source.range(resource.data, check)?.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    Ok(retain_print_value(parse(&bytes), warnings))
}

fn inspect_indexed_resources<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(DocumentResources, Vec<String>), String> {
    let mut warnings = Vec::new();
    let resolution = inspect_fixed_resource(source, index, RESOLUTION_RESOURCE_ID,
        "print resolution", parse_resolution, &mut warnings, check)?;
    let scale = inspect_fixed_resource(source, index, PRINT_SCALE_RESOURCE_ID,
        "print scale", parse_print_scale, &mut warnings, check)?;
    let pixel_aspect = inspect_fixed_resource(source, index, PIXEL_ASPECT_RESOURCE_ID,
        "pixel aspect ratio", parse_pixel_aspect, &mut warnings, check)?;
    let grid = if index.resources.iter().any(|resource| resource.id == GRID_RESOURCE_ID && resource.data.bytes > 16) {
        warnings.push(GUIDE_WARNING.into());
        None
    } else {
        inspect_fixed_resource(source, index, GRID_RESOURCE_ID,
            "grid header", parse_grid, &mut warnings, check)?
    };
    let layer_id_seed = inspect_fixed_resource(source, index, LAYER_ID_SEED_RESOURCE_ID,
        "layer ID seed", parse_layer_id_seed, &mut warnings, check)?;
    let print_flags = inspect_fixed_resource(source, index, PRINT_FLAGS_RESOURCE_ID,
        "print flags", parse_print_flags, &mut warnings, check)?;
    let print_flags_info = inspect_fixed_resource(source, index, PRINT_FLAGS_INFO_RESOURCE_ID,
        "print flags information", parse_print_flags_info, &mut warnings, check)?;
    let copyright = inspect_fixed_resource(source, index, COPYRIGHT_RESOURCE_ID,
        "copyright flag", parse_copyright, &mut warnings, check)?;
    let global_angle = inspect_fixed_resource(source, index, GLOBAL_ANGLE_RESOURCE_ID,
        "global angle", parse_global_angle, &mut warnings, check)?;
    let global_altitude = inspect_fixed_resource(source, index, GLOBAL_ALTITUDE_RESOURCE_ID,
        "global altitude", parse_global_altitude, &mut warnings, check)?;
    let mut intentionally_untagged = inspect_fixed_resource(source, index, UNTAGGED_PROFILE_RESOURCE_ID,
        "untagged profile declaration", parse_untagged_profile, &mut warnings, check)? == Some([1]);
    if intentionally_untagged && index.resources.iter().any(|resource| resource.id == 1039) {
        warnings.push(CONFLICTING_PROFILE_DECLARATIONS.into());
        intentionally_untagged = false;
    }
    Ok((DocumentResources { resolution, scale, pixel_aspect, grid, layer_id_seed, print_flags,
        print_flags_info, copyright, global_angle, global_altitude, intentionally_untagged }, warnings))
}

fn indexed_resources<R: Read + Seek>(
    source: &row_stream::Source<R>,
    index: &structural::Index,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<DocumentResources, String> {
    if index.resources.iter()
        .filter(|resource| !DocumentResources::SOURCE36_FIXED_IDS.contains(&resource.id)
            && !file_info::supported(resource.id))
        .count() > structural::SOURCE36_BASE_RESOURCES
    {
        return Err("Too many Source36 image resources outside supported document metadata".into());
    }
    let (print, warnings) = inspect_indexed_resources(source, index, check)?;
    match warnings.into_iter().next() {
        Some(reason) => Err(reason),
        None => Ok(print),
    }
}

fn retain_print_value<T>(value: Result<T, String>, warnings: &mut Vec<String>) -> Option<T> {
    match value {
        Ok(value) => Some(value),
        Err(reason) => {
            warnings.push(reason);
            None
        }
    }
}

fn decode_parts(
    resources: &[u8],
    layer_section: &[u8],
    w: u32,
    h: u32,
    depth: u16,
    merged: Raster,
    merged_white_valid: bool,
    composite_hash: u64,
    layout: Layout,
    control: &crate::loading::Control,
    mut streams: Option<&mut dyn IndexedChannels>,
    require_peerbrush_source: bool,
    file_info_oversized: bool,
) -> Result<Document, String> {
    let backed = streams
        .as_ref()
        .is_some_and(|streams| streams.uses_backing());
    let canvas_limits = if backed {
        crate::raster::CanvasLimits::Native
    } else {
        crate::raster::CanvasLimits::Resident
    };
    let mut rr = Cursor::new(resources);
    let mut embedded = None;
    let mut warnings = global_warnings(layer_section, depth, layout)?;
    let (_, global_mask, global_tags) = global_blocks(layer_section, layout)?;
    let mut global_metadata = Vec::<GlobalMetadata>::new();
    for (key, data) in global_tags {
        control.check()?;
        if let Ok(key) = std::str::from_utf8(key) {
            // Duplicates already force protection. Retain at most one bounded
            // value per known key while inspecting the protected source.
            if !global_metadata.iter().any(|old| old.key == key)
                && global_metadata::validate_entry(key, data).is_ok()
            {
                global_metadata.push(GlobalMetadata {
                    key: key.into(),
                    data: data.to_vec(),
                });
            }
        }
    }
    let global_mask_settings = (!global_mask.is_empty()
        && validate_global_mask_settings(global_mask).is_ok())
    .then(|| GlobalMaskSettings {
        data: global_mask.to_vec(),
    });
    let mut saved_composite = None;
    let mut icc_profile = None;
    let mut seen_profile = false;
    let mut intentionally_untagged = false;
    let mut seen_untagged_profile = false;
    let mut srgb_tagged = false;
    let mut psd_resolution = None;
    let mut seen_resolution = false;
    let mut psd_print_scale = None;
    let mut seen_print_scale = false;
    let mut psd_pixel_aspect = None;
    let mut seen_pixel_aspect = false;
    let mut psd_grid = None;
    let mut seen_grid = false;
    let mut psd_layer_id_seed = None;
    let mut seen_layer_id_seed = false;
    let mut psd_print_flags = None;
    let mut seen_print_flags = false;
    let mut psd_print_flags_info = None;
    let mut seen_print_flags_info = false;
    let mut psd_copyright = None;
    let mut seen_copyright = false;
    let mut file_info = Vec::new();
    let mut refused_file_info = file_info_oversized;
    if file_info_oversized {
        add_resource_warning(&mut warnings, "Photoshop File Info resources exceed 1 MiB");
    }
    let mut psd_global_angle = None;
    let mut seen_global_angle = false;
    let mut psd_global_altitude = None;
    let mut seen_global_altitude = false;
    let mut saw_effects_visibility = false;
    while rr.pos < resources.len() {
        let start = rr.pos;
        if rr.bytes(4)? != b"8BIM" {
            return Err("Invalid image resource".into());
        }
        let key = rr.u16()?;
        let n = rr.u8()? as usize;
        rr.bytes(n)?;
        if (n + 1) % 2 != 0 {
            rr.bytes(1)?;
        }
        let payload = rr.block()?;
        let document_warning = if key == EFFECTS_VISIBILITY_RESOURCE_ID {
            effects_visibility_warning(&mut saw_effects_visibility, match payload {
                [flag] => Some(*flag),
                _ => None,
            })
        } else {
            unsupported_document_resource(key)
        };
        if let Some(reason) = document_warning {
            add_resource_warning(&mut warnings, reason);
        }
        if key == 4000 && payload.starts_with(b"PBR1") {
            if embedded.is_some() {
                warnings.push(
                    "Duplicate PeerBrush source resources are protected; using the saved composite"
                        .into(),
                );
            }
            embedded = Some(&payload[4..]);
        }
        // VersionInfo marks a missing compatibility composite explicitly.
        if key == VERSION_INFO_RESOURCE_ID {
            record_saved_composite(&mut saved_composite, payload)?;
        }
        if key == 1039 {
            if seen_profile {
                return Err("Duplicate Photoshop ICC profiles are ambiguous".into());
            }
            seen_profile = true;
            if payload.len() > crate::color_profile::MAX_PROFILE_BYTES {
                return Err("Photoshop ICC profile exceeds the 4 MiB limit".into());
            }
            if crate::color_profile::is_srgb_profile(payload) {
                // Exact built-in working profile: no conversion or semantic
                // change is needed. Similar/unknown profiles stay protected.
                srgb_tagged = true;
            } else if backed {
                return Err("Embedded Photoshop color profile requires compatibility decoding".into());
            } else {
                match crate::color_profile::supported(payload) {
                    Ok(()) => warnings.push("Embedded RGB color profiles use an sRGB preview. Convert to an sRGB copy to edit; original samples and profile remain protected".into()),
                    Err(reason) => warnings.push(format!("Embedded color profiles are protected; this preview is unmanaged: {reason}")),
                }
                icc_profile = Some(std::sync::Arc::new(payload.to_vec()));
            }
        }
        if key == UNTAGGED_PROFILE_RESOURCE_ID {
            if std::mem::replace(&mut seen_untagged_profile, true) {
                intentionally_untagged = false;
                add_resource_warning(&mut warnings, "Duplicate Photoshop untagged profile declaration remains protected");
            } else {
                intentionally_untagged = retain_print_value(parse_untagged_profile(payload), &mut warnings) == Some([1]);
            }
        }
        if key == RESOLUTION_RESOURCE_ID {
            if std::mem::replace(&mut seen_resolution, true) {
                psd_resolution = None;
                warnings.push("Duplicate Photoshop print resolution remains protected".into());
            } else {
                psd_resolution = retain_print_value(parse_resolution(payload), &mut warnings);
            }
        }
        if key == PRINT_SCALE_RESOURCE_ID {
            if std::mem::replace(&mut seen_print_scale, true) {
                psd_print_scale = None;
                warnings.push("Duplicate Photoshop print scale remains protected".into());
            } else {
                psd_print_scale = retain_print_value(parse_print_scale(payload), &mut warnings);
            }
        }
        if key == PIXEL_ASPECT_RESOURCE_ID {
            if std::mem::replace(&mut seen_pixel_aspect, true) {
                psd_pixel_aspect = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop pixel aspect ratio remains protected");
            } else {
                psd_pixel_aspect = retain_print_value(parse_pixel_aspect(payload), &mut warnings);
            }
        }
        if key == GRID_RESOURCE_ID {
            if std::mem::replace(&mut seen_grid, true) {
                psd_grid = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop grid header remains protected");
            } else {
                psd_grid = retain_print_value(parse_grid(payload), &mut warnings);
            }
        }
        if key == LAYER_ID_SEED_RESOURCE_ID {
            if std::mem::replace(&mut seen_layer_id_seed, true) {
                psd_layer_id_seed = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop layer ID seed remains protected");
            } else {
                psd_layer_id_seed = retain_print_value(parse_layer_id_seed(payload), &mut warnings);
            }
        }
        if key == PRINT_FLAGS_RESOURCE_ID {
            if std::mem::replace(&mut seen_print_flags, true) {
                psd_print_flags = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop print flags remain protected");
            } else {
                psd_print_flags = retain_print_value(parse_print_flags(payload), &mut warnings);
            }
        }
        if key == PRINT_FLAGS_INFO_RESOURCE_ID {
            if std::mem::replace(&mut seen_print_flags_info, true) {
                psd_print_flags_info = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop print flags information remains protected");
            } else {
                psd_print_flags_info = retain_print_value(parse_print_flags_info(payload), &mut warnings);
            }
        }
        if key == COPYRIGHT_RESOURCE_ID {
            if std::mem::replace(&mut seen_copyright, true) {
                psd_copyright = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop copyright flag remains protected");
            } else {
                psd_copyright = retain_print_value(parse_copyright(payload), &mut warnings);
            }
        }
        if key == GLOBAL_ANGLE_RESOURCE_ID {
            if std::mem::replace(&mut seen_global_angle, true) {
                psd_global_angle = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop global angle remains protected");
            } else {
                psd_global_angle = retain_print_value(parse_global_angle(payload), &mut warnings);
            }
        }
        if key == GLOBAL_ALTITUDE_RESOURCE_ID {
            if std::mem::replace(&mut seen_global_altitude, true) {
                psd_global_altitude = None;
                add_resource_warning(&mut warnings, "Duplicate Photoshop global altitude remains protected");
            } else {
                psd_global_altitude = retain_print_value(parse_global_altitude(payload), &mut warnings);
            }
        }
        if payload.len() % 2 != 0 {
            rr.bytes(1)?;
        }
        if file_info::supported(key) && !refused_file_info {
            let record = &resources[start..rr.pos];
            let result = if record.len() > file_info::MAX_BYTES - file_info.len() {
                Err("Photoshop File Info resources exceed 1 MiB".into())
            } else {
                file_info.try_reserve_exact(record.len())
                    .map_err(|_| "Photoshop File Info allocation refused")?;
                file_info.extend_from_slice(record);
                file_info::validate(&file_info)
            };
            if let Err(reason) = result {
                file_info.clear();
                refused_file_info = true;
                add_resource_warning(&mut warnings, &reason);
            }
        }
    }
    let psd_file_info = (!file_info.is_empty()).then(|| std::sync::Arc::new(file_info));
    if intentionally_untagged && seen_profile {
        warnings.push(CONFLICTING_PROFILE_DECLARATIONS.into());
        intentionally_untagged = false;
    }
    let has_merged_composite = saved_composite.unwrap_or(true);
    if backed {
        if embedded.is_some()
            || icc_profile.is_some()
            || !has_merged_composite
            || !warnings.is_empty()
        {
            return Err("Photoshop source requires compatibility decoding".into());
        }
    } else {
        control.preview(&merged, icc_profile.as_deref().map(Vec::as_slice))?;
        #[cfg(test)]
        observe_open_phase("preview_ready", 0);
    }
    let high = depth == 16;
    if high && !has_merged_composite {
        return Err("This 16-bit PSD has no saved compatibility image. Re-save a copy in Photoshop with Maximize PSD Compatibility enabled to preview it in PeerBrush".into());
    }
    // Supplementary sources cannot override an unsupported standard resource
    // (for example a profile added without changing layer/composite hashes).
    if let Some(bytes) = embedded.filter(|_| warnings.iter().all(|warning| warning == "Unsupported global Photoshop feature: Patt")) {
        control.progress("Restoring editable sources", 0, 1)?;
        let limit = if high {
            512 * 1024 * 1024
        } else {
            128 * 1024 * 1024
        };
        #[cfg(test)]
        observe_open_phase(
            "json_start",
            streams.as_ref().map_or(bytes.len(), |s| s.embedded_len()),
        );
        let result = if let Some(streams) = streams.as_deref_mut() {
            streams.restore(limit, control)
        } else {
            embedded_source(ZlibDecoder::new(bytes), limit, control).map_err(|e| e.to_string())
        };
        #[cfg(test)]
        observe_open_phase("source_decoded", 0);
        control.check()?;
        if let Ok(e) = result {
            if ((1..=35).contains(&e.format) || (37..=DISABLED_VECTOR_MASK_FORMAT).contains(&e.format))
                && (e.format >= DISABLED_VECTOR_MASK_FORMAT || !has_implicit_disabled_vector_mask(&e.document))
                && e.document.psd_file_info.as_deref().is_none_or(|bytes| e.format >= file_info::source_format(bytes))
                && (e.format >= CLOSING_METADATA_FORMAT || !has_closing_metadata(&e.document))
                && (e.format >= CLOSED_FOLDER_FORMAT || !e.document.layers.iter().any(|layer| folder_is_closed(&layer.psd_metadata)))
                && (e.format >= PARAGRAPH_TEXT_FORMAT || !has_paragraph_text(&e.document))
                && (e.format >= MASK_DENSITY_FORMAT || !has_mask_density(&e.document))
                && (e.format >= PHOTOSHOP_RGB8_MULTIPLY_FORMAT || !e.document.psd_rgb8_multiply)
                && (e.format >= GLOBAL_LIGHT_STYLE_FORMAT || !has_global_light_style(&e.document))
                && (e.format >= GLOBAL_ALTITUDE_FORMAT || e.document.psd_global_altitude.is_none())
                && (e.format >= GLOBAL_ANGLE_FORMAT || e.document.psd_global_angle.is_none())
                && (e.format >= COPYRIGHT_FORMAT || e.document.psd_copyright.is_none())
                && (e.format >= PRINT_FLAGS_FORMAT || (e.document.psd_print_flags.is_none() && e.document.psd_print_flags_info.is_none()))
                && (e.format >= LAYER_ID_SEED_FORMAT || e.document.psd_layer_id_seed.is_none())
                && (e.format >= GRID_FORMAT || e.document.psd_grid.is_none())
                && (e.format >= UNTAGGED_PROFILE_FORMAT || !e.document.psd_intentionally_untagged)
                && (e.format >= PIXEL_ASPECT_FORMAT || e.document.psd_pixel_aspect.is_none())
                && (e.format >= PATTERN_COVERAGE_FORMAT || !has_pattern_coverage(&e.document))
                && (e.format >= INTERIOR_GROUP_FORMAT || !has_interior_group_flag(&e.document))
                && e.document.layers.iter().all(|layer|
                    blend_channels::source_format(&e.document, layer) <= e.format)
                && e.document.layers.iter().all(|layer|
                    blend_ranges::source_format(&e.document, layer) <= e.format)
                && e.document.layers.iter().all(|layer| fill_opacity::source_format(layer) <= e.format)
                && (e.format >= 63 || e.document.layers.iter().all(|layer| clipping::grouped(&layer.psd_metadata)))
                && (e.format >= 62 || e.document.layers.iter().all(|layer| fill_opacity::value(&layer.psd_metadata) == 255))
                && tone_source_format(&e.document).is_none_or(|format| e.format >= format)
                && (e.format >= 53 || e.document.layers.iter().flat_map(|layer| &layer.effects).chain(&e.document.filters)
                    .all(|effect| !crate::effects::has_channel_curves(&effect.kind, &effect.settings)))
                && (e.format >= 51 || e.document.layers.iter().all(|layer|
                    !extended_vector_metadata(&layer.psd_metadata)))
                && (e.format >= 50 || e.document.layers.iter().flat_map(|layer| &layer.effects).chain(&e.document.filters)
                    .all(|effect| !crate::effects::levels::channels(&effect.kind, &effect.settings)))
                && (e.format >= 49 || e.document.layers.iter().flat_map(|layer| &layer.effects).chain(&e.document.filters)
                    .all(|effect| !crate::effects::levels::extended(&effect.kind, &effect.settings)))
                && (e.format >= 47 || e.document.layers.iter().flat_map(|layer| &layer.effects).chain(&e.document.filters)
                    .all(|effect| !crate::effects::has_levels_output(&effect.kind, &effect.settings)))
                && (e.format >= 45 || e.document.layers.iter().all(|layer| {
                    layer.effects.iter().all(|effect| !crate::effects::gradient::coverage(&effect.kind, &effect.settings))
                }))
                && (e.format >= 42 || e.document.layers.iter().all(|layer| {
                    layer.psd_metadata.iter().all(|entry| entry.key != "vmsk")
                }))
                && (e.format >= 37 || e.document.psd_global_metadata.is_empty())
                && (e.format >= 34 || !has_shaped_text(&e.document))
                && (e.format >= 33 || !crate::retained::has_warps(&e.document))
                && (e.format >= 32 || !crate::retained::has_projective(&e.document))
                && (e.format >= 31 || !has_installed_fonts(&e.document))
                && (e.format >= 30 || !has_letter_spacing(&e.document))
                && (e.format >= 29
                    || e.document
                        .layers
                        .iter()
                        .all(|layer| layer.mask.as_ref().is_none_or(|mask| mask.linked)))
                && e.document.width == w
                && e.document.height == h
                && e.document.bit_depth == depth
                && (!high || e.format >= 5)
                && e.standard_hash
                    == streams
                        .as_ref()
                        .map_or_else(|| hash(layer_section), |s| s.section_hash())
                        ^ composite_hash
                && e.document.psd_global_mask == global_mask_settings
                && e.document.psd_global_metadata == global_metadata
                && e.document.psd_resolution == psd_resolution
                && e.document.psd_print_scale == psd_print_scale
                && e.document.psd_pixel_aspect == psd_pixel_aspect
                && e.document.psd_grid == psd_grid
                && e.document.psd_layer_id_seed == psd_layer_id_seed
                && e.document.psd_print_flags == psd_print_flags
                && e.document.psd_print_flags_info == psd_print_flags_info
                && e.document.psd_copyright == psd_copyright
                && e.document.psd_file_info == psd_file_info
                && e.document.psd_global_angle == psd_global_angle
                && e.document.psd_global_altitude == psd_global_altitude
                && e.document.psd_intentionally_untagged == intentionally_untagged
                && e.document.srgb_tagged == srgb_tagged
                && validate(&e.document).is_ok()
                && metadata_sources_match(
                    layer_section, layout, &e.document, e.format, streams.as_deref(), control,
                )
                    == Ok(true)
                && (!e.document.psd_rgb8_multiply || (has_merged_composite && merged_white_valid
                    && imported_style_composite_matches(&e.document, &merged, &|| control.check()) == Ok(true)))
            {
                #[cfg(test)]
                observe_open_phase("source_accepted", 0);
                control.progress("Ready", 1, 1)?;
                return Ok(e.document);
            }
        }
        warnings.push(
            "PeerBrush definitions were changed or removed externally; using saved PSD pixels"
                .into(),
        );
    }
    if require_peerbrush_source {
        return Err("Unpaired recovery has no valid editable PeerBrush source; the file is retained".into());
    }
    let mut source_patterns: Vec<patterns::Source> = vec![];
    for (key, payload) in global_blocks(layer_section, layout)?.2 {
        if matches!(key, b"Patt" | b"Pat2" | b"Pat3") {
            let tag = std::str::from_utf8(key).unwrap();
            let mut admit = |bytes| streams.as_deref_mut().map_or(Ok(()), |input| input.reserve_interpreted_bytes(bytes));
            match patterns::decode_admitted(payload, tag, control, &mut admit) {
                Ok(mut assets) => {
                    let bytes: usize = source_patterns
                        .iter()
                        .map(|p| p.pixels.bytes())
                        .sum::<usize>()
                        + assets.iter().map(|p| p.pixels.bytes()).sum::<usize>();
                    if source_patterns.len() + assets.len() > 16 || bytes > 16 * 1024 * 1024 {
                        warnings.push("Embedded Photoshop patterns exceed the bounded inspection budget; additional assets remain protected".into());
                    } else {
                        for asset in &mut assets {
                            asset.source_index += source_patterns.len();
                        }
                        source_patterns.extend(assets);
                    }
                }
                Err(reason) => {
                    control.check()?;
                    warnings.push(format!("Cannot inspect Photoshop pattern assets ({tag}): {reason}; using the protected saved composite"));
                }
            }
        }
    }
    let mut used_patterns = std::collections::HashSet::new();
    let mut bitmap_bytes = 0usize;
    let mut style_bytes = 0usize;
    let mut records = vec![];
    if !layer_section.is_empty() {
        let info = layer_info(layer_section, depth, layout)?;
        if info.len() >= 2 {
            let mut lr = Cursor::new(info);
            let signed_count = lr.i16()?;
            let count = signed_count.unsigned_abs() as usize;
            if count > 200 {
                return Err("Too many PSD layers for this version".into());
            }
            for record_index in 0..count {
                let mut admit_bitmap = |bytes| {
                    let next = bitmap_bytes.checked_add(bytes)
                        .filter(|&sum| sum <= crate::effects::patterns::DOCUMENT_LIMIT)
                        .ok_or("Bitmap pattern sources exceed the 32 MiB document budget")?;
                    if let Some(input) = streams.as_deref_mut() { input.reserve_interpreted_bytes(bytes)?; }
                    bitmap_bytes = next;
                    Ok(())
                };
                control.progress("Reading layer definitions", record_index, count)?;
                let warning_start = warnings.len();
                let bounds = [lr.i32()?, lr.i32()?, lr.i32()?, lr.i32()?];
                if bounds.iter().any(|v| v.unsigned_abs() > 100000) {
                    return Err("PSD layer coordinates exceed initial limits".into());
                }
                let lh = bounds[2]
                    .checked_sub(bounds[0])
                    .ok_or("Invalid layer bounds")?;
                let lw = bounds[3]
                    .checked_sub(bounds[1])
                    .ok_or("Invalid layer bounds")?;
                if lw < 0
                    || lh < 0
                    || lw > canvas_limits.edge() as i32
                    || lh > canvas_limits.edge() as i32
                    || !backed && lw as i64 * lh as i64 > crate::raster::MAX_PIXELS as i64
                {
                    return Err("PSD layer bounds exceed limits".into());
                }
                let nc = lr.u16()?;
                if nc > 8 {
                    return Err("Unsupported PSD layer channels".into());
                }
                let mut channels = vec![];
                let mut channel_ids = std::collections::HashSet::new();
                for _ in 0..nc {
                    let channel = lr.i16()?;
                    if !channel_ids.insert(channel) {
                        warnings.push(format!("Duplicate Photoshop layer channel: {channel}"));
                    }
                    channels.push((channel, lr.length(layout.large())?));
                    if !(-3..=2).contains(&channel) {
                        warnings.push(format!("Unsupported Photoshop layer channel: {channel}"));
                    }
                }
                if lr.bytes(4)? != b"8BIM" {
                    return Err("Invalid blend signature".into());
                }
                let blendkey = lr.bytes(4)?;
                let mut blend = read_blend(blendkey, &mut warnings);
                let opacity = lr.u8()? as f32 / 255.0;
                let clipping = lr.u8()?;
                if clipping > 1 {
                    warnings.push(format!("Invalid Photoshop clipping flag: {clipping}"));
                }
                let flags = lr.u8()?;
                if flags & !0x1f != 0 {
                    warnings.push("Unsupported Photoshop layer protection or flags".into());
                }
                if lr.u8()? != 0 {
                    warnings.push("Invalid Photoshop layer filler".into());
                }
                let extra = lr.block()?;
                let mut ex = Cursor::new(extra);
                let maskdata = ex.block()?;
                let mut mask_bounds = None;
                let mut mask_default = 255;
                let mut mask_enabled = true;
                let mut mask_linked = true;
                let mut explicit_mask_effect_mode = false;
                let mask_header = standard_mask_header(maskdata);
                if !maskdata.is_empty() && mask_header.is_none() {
                    warnings
                        .push("Advanced or invalid Photoshop mask data is not supported".into());
                }
                if maskdata.len() >= 18 {
                    mask_bounds = Some(if let Some(header) = &mask_header {
                        header.bounds
                    } else {
                        let mut m = Cursor::new(maskdata);
                        [m.i32()?, m.i32()?, m.i32()?, m.i32()?]
                    });
                    if mask_bounds.into_iter()
                        .chain(mask_header.and_then(|header| header.rendered_bounds))
                        .flatten()
                        .any(|v| v.unsigned_abs() > 100000)
                    {
                        return Err("PSD mask coordinates exceed initial limits".into());
                    }
                    if let Some(header) = &mask_header {
                        mask_default = header.default;
                        mask_enabled = header.flags & MASK_DISABLED == 0;
                        mask_linked = header.flags & MASK_UNLINKED == 0;
                    }
                }
                let range_payload = ex.block()?;
                let np = ex.u8()? as usize;
                let mut name = String::from_utf8_lossy(ex.bytes(np)?).to_string();
                let padded = (np + 1 + 3) & !3;
                ex.bytes(padded - np - 1)?;
                let mut section = 0;
                let mut locked = false;
                let mut psd_metadata = vec![];
                let mut imported_styles = vec![];
                let mut legacy_style_tag = None;
                let validate_style_source = |effects: &[crate::effects::Effect]| -> Result<(), String> {
                    if !maskdata.is_empty() || clipping != 0 {
                        if effects.iter().any(|effect| matches!(effect.kind.as_str(), "drop_shadow" | "inner_shadow")) {
                            return Err("Hard Photoshop shadows with masks or clipping remain protected".into());
                        }
                        if effects.iter().any(|effect| effect.kind == "stroke") {
                            return Err("Photoshop strokes with masks or clipping remain protected".into());
                        }
                    }
                    Ok(())
                };
                let mut group_interior_effects = None;
                let mut invert = false;
                let mut tone = None;
                let mut brightness_record = None;
                let mut brightness_descriptor = None;
                let mut adjustment_preset = None;
                let mut solid = None;
                let mut fill_effect = None;
                let mut seen_tags = std::collections::HashSet::new();
                while ex.pos + 12 <= extra.len() {
                    let signature = ex.bytes(4)?;
                    if signature != b"8BIM" && signature != b"8B64" {
                        warnings.push("Unrecognized Photoshop layer tag signature".into());
                        break;
                    }
                    let key = ex.bytes(4)?;
                    let payload = layout.tag_block(&mut ex, key)?;
                    if payload.len() % 2 != 0 {
                        ex.bytes(1)?;
                    }
                    if !seen_tags.insert(key) {
                        warnings.push(format!(
                            "Duplicate Photoshop layer tag: {}",
                            String::from_utf8_lossy(key)
                        ));
                    }
                    match key {
                        b"lmgm" => {
                            if signature == b"8BIM" && payload == [0; 4] && mask_bounds.is_some() {
                                explicit_mask_effect_mode = true;
                            } else {
                                warnings.push("Photoshop layer mask effects compositing (lmgm) remains protected".into());
                            }
                        }
                        b"brit" | b"CgEd" => {
                            let result = if signature != b"8BIM" {
                                Err("Adjustment descriptor requires a standard Photoshop record signature".into())
                            } else if key == b"brit" {
                                if depth == 8 {
                                    adjustments::brightness_record(payload)
                                        .map(|settings| brightness_record = Some(settings))
                                } else {
                                    Err("Photoshop Brightness/Contrast import requires 8-bit RGB".into())
                                }
                            } else {
                                adjustments::tone_descriptor(payload).and_then(|descriptor| {
                                    match descriptor {
                                        adjustments::ToneDescriptor::Brightness(kind, settings) if depth == 8 => {
                                            brightness_descriptor = Some((kind, settings));
                                        }
                                        adjustments::ToneDescriptor::Brightness(..) =>
                                            return Err("Photoshop Brightness/Contrast import requires 8-bit RGB".into()),
                                        adjustments::ToneDescriptor::Preset(kind, preset) =>
                                            adjustment_preset = Some((kind, preset)),
                                    }
                                    Ok(())
                                })
                            };
                            if let Err(reason) = result {
                                warnings.push(format!("Photoshop adjustment record remains protected: {reason}"));
                            }
                        }
                        b"levl" | b"curv" | b"post" | b"thrs" | b"expA" | b"mixr" | b"blwh" | b"selc" | b"vibA" | b"phfl" | b"grdm" | b"blnc" | b"hue2" | b"clrL" => {
                            let (kind, settings) = match key {
                                b"levl" => ("levels", adjustments::levels_settings(payload).and_then(|settings| {
                                    if depth != 8 && crate::effects::levels::byte_composition("levels", &settings) {
                                        return Err("Combined Photoshop Levels require 8-bit RGB".into());
                                    }
                                    Ok(settings)
                                })),
                                b"curv" => ("curves", adjustments::curves_settings(payload).and_then(|settings| {
                                    if depth != 8 && crate::effects::curves::byte_composition("curves", &settings) {
                                        return Err("Combined Photoshop Curves require 8-bit RGB".into());
                                    }
                                    if depth != 8 && crate::effects::curves::uses_photoshop("curves", &settings) {
                                        Err("16-bit Photoshop spline Curves are unverified".into())
                                    } else {
                                        Ok(settings)
                                    }
                                })),
                                b"post" => ("posterize", adjustments::posterize_settings(payload)),
                                b"expA" => (crate::effects::exposure::KIND, if depth == 8 {
                                    adjustments::exposure_settings(payload)
                                } else {
                                    Err("Photoshop Exposure import requires 8-bit RGB".into())
                                }),
                                b"mixr" => (crate::effects::channel_mixer::KIND, if depth == 8 {
                                    adjustments::channel_mixer_settings(payload)
                                } else {
                                    Err("Photoshop Channel Mixer import requires 8-bit RGB".into())
                                }),
                                b"blwh" => (crate::effects::black_white::KIND, if depth == 8 {
                                    adjustments::black_white_settings(payload)
                                } else {
                                    Err("Photoshop Black & White import requires 8-bit RGB".into())
                                }),
                                b"selc" => (crate::effects::selective_color::KIND, if depth == 8 {
                                    adjustments::selective_color_settings(payload)
                                } else {
                                    Err("Photoshop Selective Color import requires 8-bit RGB".into())
                                }),
                                b"vibA" => (crate::effects::vibrance::KIND, if depth == 8 {
                                    adjustments::vibrance_settings(payload)
                                } else {
                                    Err("Photoshop Vibrance import requires 8-bit RGB".into())
                                }),
                                b"phfl" => (crate::effects::photo_filter::KIND, if depth == 8 {
                                    adjustments::photo_filter_settings(payload)
                                } else {
                                    Err("Photoshop Photo Filter import requires 8-bit RGB".into())
                                }),
                                b"grdm" => (crate::effects::gradient_map::KIND, if depth == 8 {
                                    gradient_map::settings(payload)
                                } else {
                                    Err("Photoshop Gradient Map import requires 8-bit RGB".into())
                                }),
                                b"blnc" => (crate::effects::photoshop_color_balance::KIND, if depth == 8 {
                                    adjustments::color_balance_settings(payload)
                                } else {
                                    Err("Photoshop Color Balance import requires 8-bit RGB".into())
                                }),
                                b"hue2" => (crate::effects::photoshop_hue_saturation::KIND, if depth == 8 {
                                    adjustments::hue_saturation_settings(payload)
                                } else {
                                    Err("Photoshop Hue/Saturation import requires 8-bit RGB".into())
                                }),
                                b"clrL" => (crate::effects::color_lookup::KIND, adjustments::color_lookup_settings(payload)),
                                _ => ("threshold", adjustments::threshold_settings(payload)),
                            };
                            if signature != b"8BIM" || tone.is_some() {
                                warnings.push("Invalid or conflicting Photoshop adjustment definitions".into());
                            } else {
                                match settings {
                                    Ok(settings) => tone = Some((kind, settings)),
                                    Err(reason) => warnings.push(format!("Photoshop {kind} remains protected: {reason}")),
                                }
                            }
                        }
                        b"nvrt" => {
                            if signature == b"8BIM" && payload.is_empty() {
                                invert = true;
                            } else {
                                warnings.push("Invalid Photoshop Invert adjustment (nvrt)".into());
                            }
                        }
                        b"GdFl" | b"PtFl" => {
                            let name = if key == b"GdFl" { "Gradient Fill" } else { "Pattern Fill" };
                            let parsed = if signature != b"8BIM" || fill_effect.is_some() {
                                Err("Invalid or conflicting fill record".into())
                            } else if key == b"GdFl" {
                                styles::gradient_fill(payload).map(|effect| (effect, None))
                            } else {
                                styles::pattern_fill(payload, &source_patterns, depth, &mut admit_bitmap)
                                    .map(|(effect, index)| (effect, Some(index)))
                            };
                            match parsed {
                                Ok((effect, pattern)) => {
                                    fill_effect = Some(effect);
                                    if let Some(index) = pattern { used_patterns.insert(index); }
                                }
                                Err(reason) => warnings.push(format!("Photoshop {name} remains protected: {reason}")),
                            }
                        }
                        b"SoCo" => {
                            if signature == b"8BIM" {
                                match styles::solid_fill(payload) {
                                    Ok(color) => solid = Some(color),
                                    Err(reason) => warnings.push(format!("Photoshop Solid Color remains protected: {reason}")),
                                }
                            } else {
                                warnings.push("Invalid Photoshop Solid Color signature".into());
                            }
                        }
                        b"vmsk" | b"vsms" => {
                            match vector_mask::inspect(payload) {
                                Ok(mask) if key == b"vmsk" && signature == b"8BIM" && mask.is_noop() => {
                                    if !psd_metadata.iter().any(|old: &LayerMetadata| old.key == "vmsk") {
                                        psd_metadata.push(LayerMetadata { key: "vmsk".into(), data: payload.to_vec() });
                                    }
                                }
                                Ok(mask) => warnings.push(mask.protection_reason(key)),
                                Err(reason) => warnings.push(format!("Unsupported Photoshop vector mask: {reason}")),
                            }
                        }
                        b"luni" => match unicode_layer_name(payload, MAX_LAYER_NAME_UNITS) {
                            Ok((decoded, _)) if signature == b"8BIM" => name = decoded,
                            Ok(_) => warnings.push("Invalid Photoshop Unicode layer name signature".into()),
                            Err(error) => warnings.push(error),
                        },
                        b"lsct" | b"lsdk" => {
                            if seen_tags.contains(b"lsct".as_slice())
                                && seen_tags.contains(b"lsdk".as_slice())
                            {
                                warnings.push("Conflicting Photoshop section divider tags".into());
                            }
                            match section_divider(signature, payload) {
                                Ok((kind, mode)) => {
                                    section = kind;
                                    if let Some(mode) = mode {
                                        if matches!(section, 1 | 2) {
                                            blend = mode;
                                        } else if mode != blend {
                                            warnings.push("Conflicting Photoshop blend on a non-folder section divider".into());
                                        }
                                    }
                                }
                                Err(reason) => warnings.push(reason),
                            }
                        }
                        b"brst" => match (signature == b"8BIM", blend_channels::parse(payload)) {
                            (true, Some(0)) => {}
                            (true, Some(_)) => psd_metadata.push(LayerMetadata {
                                key: "brst".into(), data: payload.to_vec(),
                            }),
                            _ => warnings.push("Invalid or non-RGB Photoshop blend channels (brst)".into()),
                        },
                        b"fxrp" | b"lclr" | b"lnsr" | b"lyid" | b"shmd" | b"lyvr" | b"tsly" | b"vmgm" => {
                            if payload.len() > 128 {
                                warnings.push(format!(
                                    "Unsupported Photoshop layer metadata: {}",
                                    String::from_utf8_lossy(key)
                                ));
                                continue;
                            }
                            let metadata = LayerMetadata {
                                key: String::from_utf8_lossy(key).into_owned(),
                                data: payload.to_vec(),
                            };
                            if signature == b"8BIM"
                                && validate_metadata_entry(&metadata).is_ok()
                                && !psd_metadata
                                    .iter()
                                    .any(|old: &LayerMetadata| old.key == metadata.key)
                            {
                                psd_metadata.push(metadata);
                            } else {
                                warnings.push(format!(
                                    "Unsupported Photoshop layer metadata: {}",
                                    String::from_utf8_lossy(key)
                                ));
                            }
                        }
                        b"lspf" | b"clbl" | b"infx" | b"knko" | b"iOpa" | b"sn2P" => {
                            if signature != b"8BIM" {
                                warnings.push("Unsupported Photoshop layer flag signature".into());
                            } else if let Err(reason) = standard_flag(key, payload) {
                                warnings.push(format!(
                                    "Unsupported Photoshop setting {}: {reason}; using the saved composite",
                                    String::from_utf8_lossy(key)
                                ));
                            } else if key == b"lspf" {
                                locked = payload != [0; 4];
                                if payload == COMPLETE_LAYER_LOCK {
                                    psd_metadata.push(LayerMetadata { key: "lspf".into(), data: payload.to_vec() });
                                }
                            } else if key == b"infx" {
                                if group_interior_effects.replace(payload[0] == 1).is_some() {
                                    warnings.push("Duplicate Photoshop Blend Interior Effects As Group flag (infx)".into());
                                }
                            } else if key == b"sn2P" {
                                psd_metadata.push(LayerMetadata { key: "sn2P".into(), data: payload.to_vec() });
                            } else if key == b"clbl" && payload == [0; 4] {
                                psd_metadata.push(LayerMetadata { key: "clbl".into(), data: payload.to_vec() });
                            } else if key == b"iOpa"
                                && fill_opacity::parse(payload).is_some_and(|amount| amount != 255)
                            {
                                psd_metadata.push(LayerMetadata { key: "iOpa".into(), data: payload.to_vec() });
                            }
                        }
                        b"lrFX" => legacy_style_tag = Some((signature, payload)),
                        b"lfx2" | b"lmfx" => {
                            let tag_name = String::from_utf8_lossy(key);
                            let detail = if signature != b"8BIM" {
                                Err("invalid effects signature".into())
                            } else {
                                styles::inspect(payload)
                            };
                            match detail {
                                Ok(details) => {
                                    // Supported overlays preserve source alpha and run before
                                    // ordinary raster masks, clipping and layer opacity. Mask
                                    // and clipping decoding still validate every field. The complete
                                    // native composite gate below verifies their combined appearance.
                                    // Hard shadows have stricter source/placement limits below.
                                    // Ordinary layer opacity fades both paint and styles after
                                    // rendering, preserving the original binary casting alpha.
                                    // Reference: psd-tools/composite/composite.py::_EffectCanvas.
                                    let imported = if signature == b"8BIM" && section == 0 && imported_styles.is_empty() {
                                        styles::native_styles(payload, &source_patterns, depth, psd_global_angle.map(u32::from_be_bytes), &mut admit_bitmap).and_then(|(effects, pattern)| {
                                            validate_style_source(&effects)?;
                                            Ok((effects, pattern))
                                        })
                                    } else { Err("Photoshop styles with folders or duplicate blocks remain protected".into()) };
                                    match imported {
                                        Ok((effects, pattern)) => {
                                            imported_styles = effects;
                                            if let Some(index) = pattern { used_patterns.insert(index); }
                                        },
                                        Err(reason) => {
                                            let warning = format!("Photoshop layer styles ({tag_name}) remain protected: {reason}; using the saved composite");
                                            if backed { return Err(warning); }
                                            warnings.push(warning);
                                            warnings.extend(details);
                                        }
                                    }
                                }
                                Err(reason) => {
                                    let warning = format!("Cannot inspect Photoshop layer styles ({tag_name}): {reason}; using the protected saved composite");
                                    if backed { return Err(warning); }
                                    warnings.push(warning);
                                }
                            }
                        }
                        _ => warnings.push(unsupported_layer_source(key).map_or_else(
                            || format!("Unsupported Photoshop feature: {}", String::from_utf8_lossy(key)),
                            str::to_owned,
                        )),
                    }
                }
                if section == 2 {
                    psd_metadata.push(LayerMetadata { key: CLOSED_FOLDER.into(), data: 2u32.to_be_bytes().to_vec() });
                }
                if let Some((signature, payload)) = legacy_style_tag {
                    let has_object_style = seen_tags.iter().any(|key| matches!(*key, b"lfx2" | b"lmfx"));
                    let legacy = if signature == b"8BIM" && section == 0 {
                        styles::legacy_style(payload, has_object_style.then_some(imported_styles.as_slice()), psd_global_angle.map(u32::from_be_bytes))
                            .and_then(|effects| {
                                validate_style_source(&effects)?;
                                Ok(effects)
                            })
                    } else {
                        Err("Legacy effects require a standard ordinary-layer record".into())
                    };
                    match legacy {
                        Ok(effects) => imported_styles.extend(effects),
                        Err(reason) => {
                            let warning = format!("Photoshop legacy layer styles remain protected. {reason}");
                            if backed { return Err(warning); }
                            warnings.push(warning);
                            warnings.push("Unsupported Photoshop feature: lrFX".into());
                        }
                    }
                }
                if backed && !imported_styles.is_empty() {
                    let before = style_bytes;
                    for effect in &imported_styles {
                        crate::effects::regional::metadata_bytes(&effect.settings, 0, &mut style_bytes)?;
                        // The bitmap admission already retained this string.
                        if let Some(png) = effect.settings["bitmap_png"].as_str() {
                            style_bytes -= png.len();
                        }
                    }
                    streams.as_deref_mut().unwrap().reserve_interpreted_bytes(style_bytes - before)?;
                }
                match adjustments::brightness_definition(brightness_record, brightness_descriptor) {
                    Ok(Some(definition)) if tone.is_none() => tone = Some(definition),
                    Ok(Some(_)) => warnings.push("Conflicting Photoshop adjustment definitions".into()),
                    Ok(None) => {}
                    Err(reason) => warnings.push(format!("Photoshop Brightness/Contrast remains protected: {reason}")),
                }
                if let Some((preset_kind, preset)) = adjustment_preset {
                    match &mut tone {
                        Some((kind, settings)) if preset_kind.applies_to(kind) => settings["preset"] = preset,
                        _ => warnings.push("Photoshop preset without a supported matching adjustment remains protected".into()),
                    }
                }
                // Folders and adjustments can carry ranges without RGB image channels.
                let range_channels = if matches!(section, 1..=3) || invert || tone.is_some() {
                    nc.max(4)
                } else {
                    nc
                };
                match blend_ranges::parse(range_payload, range_channels) {
                    Ok([]) => {}
                    Ok(_) if section == 3 => warnings.push(
                        "Photoshop Blend If on a closing folder record remains protected".into(),
                    ),
                    Ok(data) => psd_metadata.push(LayerMetadata {
                        key: blend_ranges::KEY.into(), data: data.to_vec(),
                    }),
                    Err(reason) => warnings.push(reason.into()),
                }
                if ex.pos != extra.len() && extra[ex.pos..].iter().any(|v| *v != 0) {
                    warnings.push(
                        "Unrecognized Photoshop layer data; using the saved composite".into(),
                    );
                }
                let mut retain_mask_original = false;
                if let Some(grouped) = group_interior_effects {
                    if section != 3 {
                        psd_metadata.push(LayerMetadata { key: "infx".into(), data: vec![u8::from(grouped), 0, 0, 0] });
                    } else if grouped {
                        warnings.push("Photoshop Blend Interior Effects As Group on a closing folder record remains protected".into());
                    }
                }
                // The interior-group flag can follow the style block. Only
                // these overlays fit the engine's effect-then-layer-blend order.
                if blend != "normal"
                    && imported_styles.iter().any(|effect| effect.enabled)
                    && !(group_interior_effects == Some(true)
                        && section == 0
                        && blend != "pass_through"
                        && clipping == 0
                        && (solid.is_none() || (fill_effect.is_none()
                            && imported_styles.iter().all(|effect| effect.kind == "color_overlay")))
                        && blend_channels::overlay_stack(imported_styles.iter().map(|effect| effect.kind.as_str())))
                {
                    warnings.push("Non-Normal Photoshop styled layers require grouped overlays on unclipped raster layers or Color Overlays on Solid Color fills (infx)".into());
                }
                if fill_effect.is_some() {
                    if solid.is_some() || !imported_styles.is_empty() || blend == "pass_through" {
                        warnings.push("Photoshop Gradient or Pattern Fill with other fills, styles or pass-through blending remains protected".into());
                    }
                    solid = Some([255; 4]);
                }
                if !mask_linked && !explicit_mask_effect_mode
                    && (section != 0 || invert || tone.is_some() || solid.is_some() || clipping != 0 || !imported_styles.is_empty())
                {
                    warnings.push("Photoshop unlinked mask uses legacy effects compositing; using the saved composite".into());
                }
                if let Some([t, l, b, r]) = mask_bounds.filter(|_| channel_ids.contains(&-2)) {
                    let frame = if section == 1 || section == 2 || invert || tone.is_some() || solid.is_some() {
                        [bounds[0], bounds[1], bounds[0] + h as i32, bounds[1] + w as i32]
                    } else {
                        bounds
                    };
                    let [top, left, bottom, right] = frame;
                    if t < b && l < r && (t < top || l < left || b > bottom || r > right) {
                        let width = r.max(right) - l.min(left);
                        let height = b.max(bottom) - t.min(top);
                        if canvas_limits.check(width as u32, height as u32).is_ok() {
                            retain_mask_original = true;
                        } else {
                            warnings.push("Photoshop off-frame mask exceeds retained-source limits; using the saved composite".into());
                        }
                    }
                }
                if flags & 1 != 0 && !locked {
                    warnings
                        .push("Partial Photoshop transparency protection is not supported".into());
                }
                if invert && tone.is_some() {
                    warnings.push("Conflicting Photoshop adjustment definitions".into());
                }
                if (invert || tone.is_some())
                    && (section != 0
                        || blend == "pass_through"
                        || !imported_styles.is_empty()
                        || (bounds != [0; 4] && bounds != [0, 0, h as i32, w as i32]))
                {
                    warnings.push("Photoshop adjustment with styles, pass-through blending or non-canvas bounds remains protected".into());
                }
                if solid.is_some() && (section != 0 || invert || tone.is_some()
                    || (bounds != [0; 4] && bounds != [0, 0, h as i32, w as i32]))
                {
                    warnings.push("Photoshop fill with other adjustments or non-canvas bounds remains protected".into());
                }
                if section == 0 && flags & 0x18 == 0x18 && !invert && tone.is_none() && solid.is_none() {
                    warnings.push("Photoshop layer pixels do not represent its appearance".into());
                }
                if section != 0 && clipping != 0 {
                    warnings.push("Photoshop clipping involving a folder is not supported".into());
                }
                if section != 0 && !imported_styles.is_empty() {
                    warnings.push("Photoshop folder styles remain protected".into());
                }
                if let Err(reason) = validate_layer_metadata(&psd_metadata, locked, matches!(section, 1 | 2)) {
                    warnings.push(reason);
                }
                let mask_pair = mask_header.filter(|header| header.rendered_bounds.is_some());
                let mask_density = mask_header.and_then(|header| header.density);
                let mask_parameters = mask_header.is_some_and(|header| header.has_parameters);
                if let Some(header) = mask_header.filter(|header| header.has_parameters) {
                    let [top, left, bottom, right] = header.bounds;
                    if section == 3 || !channel_ids.contains(&-2) || channel_ids.contains(&-3)
                        || seen_tags.contains(b"vmsk".as_slice()) || seen_tags.contains(b"vsms".as_slice())
                        || top >= bottom || left >= right
                    {
                        warnings.push("Photoshop raster-mask parameters require a single raster user mask; using the saved composite".into());
                    }
                }
                if (mask_pair.is_some() || channel_ids.contains(&-3))
                    && !(mask_pair.is_some() && channel_ids.contains(&-2) && channel_ids.contains(&-3)
                        && psd_metadata.iter().any(|entry| entry.key == "vmsk"))
                {
                    warnings.push("Photoshop paired mask channels require supported headers and a preserved no-op vector mask".into());
                }
                if section == 3 && psd_metadata.iter().any(|entry|
                    matches!(entry.key.as_str(), "vmsk" | "tsly" | "vmgm" | "iOpa" | "clbl" | "brst"))
                {
                    warnings.push("Photoshop vector or blending metadata on a closing folder record remains protected".into());
                }
                if section == 3 && psd_metadata.iter().any(|entry| entry.key == "sn2P") {
                    warnings.push("Photoshop aligned rendering (sn2P) on a closing folder record remains protected".into());
                }
                if section == 3 && locked {
                    warnings.push("Photoshop layer protection on a closing folder record remains protected".into());
                }
                let mut layer = Layer::new(
                    &name,
                    if section == 1 || section == 2 {
                        "group"
                    } else if invert || tone.is_some() {
                        "adjustment"
                    } else if solid.is_some() {
                        "fill"
                    } else {
                        "paint"
                    },
                    lw as u32,
                    lh as u32,
                );
                layer.pixels = Raster::new_depth(lw as u32, lh as u32, depth);
                layer.x = bounds[1];
                layer.y = bounds[0];
                layer.visible = flags & 2 == 0;
                layer.locked = locked;
                layer.opacity = opacity;
                layer.blend = blend.into();
                layer.psd_metadata = psd_metadata;
                layer.effects = imported_styles;
                if let Some(effect) = fill_effect {
                    layer.effects.push(effect);
                }
                if let Some(color) = solid {
                    layer.color = color;
                }
                if invert {
                    layer.effects.push(crate::effects::Effect {
                        id: id(),
                        kind: "invert".into(),
                        enabled: true,
                        weight: 1.,
                        settings: std::sync::Arc::new(crate::effects::defaults("invert")),
                    });
                }
                if let Some((kind, settings)) = tone {
                    layer.effects.push(crate::effects::Effect {
                        id: id(), kind: kind.into(), enabled: true, weight: 1., settings: settings.into(),
                    });
                }
                let mut details_truncated =
                    seen_tags.len() > 64 || warnings.len() - warning_start > 64;
                let mut tags = std::collections::BTreeSet::new();
                for key in &seen_tags {
                    tags.insert(String::from_utf8_lossy(key).into_owned());
                    if tags.len() > 64 {
                        tags.pop_last();
                    }
                }
                let source = SourceLayerInfo {
                    source_index: record_index,
                    parent_index: None,
                    name: diagnostic_text(&name, &mut details_truncated),
                    kind: if layer.kind == "group" {
                        "group"
                    } else {
                        "layer"
                    }
                    .into(),
                    bounds: [bounds[1], bounds[0], bounds[3], bounds[2]],
                    visible: layer.visible,
                    has_mask: mask_bounds.is_some() || seen_tags.contains(b"vmsk".as_slice())
                        || seen_tags.contains(b"vsms".as_slice()),
                    tags: tags.into_iter().collect(),
                    warnings: warnings[warning_start..]
                        .iter()
                        .take(64)
                        .map(|warning| diagnostic_text(warning, &mut details_truncated))
                        .collect(),
                    details_truncated,
                };
                records.push(Record {
                    source,
                    layer,
                    channels,
                    bounds,
                    mask_bounds,
                    mask_pair,
                    mask_default,
                    mask_density,
                    mask_parameters,
                    mask_enabled,
                    mask_linked,
                    retain_mask_original,
                    section,
                    clipped: clipping == 1,
                });
            }
            // Admit pattern resource blocks only when every asset is retained in a supported
            // native fill or style. Unused assets, duplicate tags and unrelated sources stay protected.
            if !source_patterns.is_empty() && used_patterns.len() == source_patterns.len() {
                warnings.retain(|w| {
                    ![
                        "Unsupported global Photoshop feature: Patt",
                        "Unsupported global Photoshop feature: Pat2",
                        "Unsupported global Photoshop feature: Pat3",
                    ]
                    .contains(&w.as_str())
                });
            }
            if let Err(reason) = validate_photoshop_layer_ids(
                records.iter().map(|record| record.layer.psd_metadata.as_slice()),
            ) {
                warnings.push(reason);
            }
            if !backed && warnings.is_empty() {
                let retained_masks = records.iter().any(|rec| rec.retain_mask_original);
                match decoded_storage(&records, w, h, depth) {
                    Err(reason) if retained_masks && reason == DECODED_STORAGE_BUDGET => {
                        warnings.push(reason);
                    }
                    result => result?,
                }
            }
            if warnings.is_empty() {
                let record_count = records.len();
                for (record_index, rec) in records.iter_mut().enumerate() {
                    control.progress("Decoding native layer pixels", record_index, record_count)?;
                    let paired = rec.channels.iter().position(|(id, _)| *id == -3);
                    if let Some(second) = paired {
                        let first = rec.channels.iter().position(|(id, _)| *id == -2)
                            .ok_or("Paired Photoshop mask is missing its rendered channel")?;
                        let header = rec.mask_pair.ok_or("Paired Photoshop mask header missing")?;
                        let same = if let Some(source) = streams.as_deref_mut() {
                            source.same_mask_samples(record_index, first, second, header, depth, control)
                        } else {
                            let mut payload = Cursor::new(&lr.data[lr.pos..]);
                            let mut rendered = &[][..];
                            let mut user = &[][..];
                            for &(channel, size) in &rec.channels {
                                let bytes = payload.bytes(size)?;
                                match channel {
                                    -2 => rendered = bytes,
                                    -3 => user = bytes,
                                    _ => {}
                                }
                            }
                            mask_payloads_match(
                                rendered, user, header, depth, layout, control,
                            )
                        };
                        if !matches!(same, Ok(true)) {
                            control.check()?;
                            let warning = "Photoshop rendered and real raster mask channels differ or could not be verified; using the saved composite";
                            warnings.push(warning.into());
                            rec.source.warnings.push(warning.into());
                            break;
                        }
                    }
                    let lw = rec.layer.pixels.width;
                    let lh = rec.layer.pixels.height;
                    let mut pixels = Raster::new_depth(lw, lh, depth);
                    let stroke = rec.layer.effects.iter().any(|effect| effect.kind == "stroke");
                    let staged = if backed && lw != 0 && lh != 0 {
                        if rec.section == 3 {
                            return Err("Photoshop folder divider has raster geometry".into());
                        }
                        streams
                            .as_deref_mut()
                            .map(|source| {
                                source.backed_layer(record_index, &rec.layer.id, &pixels, stroke, control)
                            })
                            .transpose()?
                            .unwrap_or(false)
                    } else {
                        false
                    };
                    if backed && stroke && !staged {
                        return Err("Photoshop Stroke requires backed raster channels".into());
                    }
                    let mut initialized = false;
                    for (channel_index, &(ch, size)) in rec.channels.iter().enumerate() {
                        control.check()?;
                        let bytes = if streams.is_some() {
                            &[][..]
                        } else {
                            lr.bytes(size)?
                        };
                        if ch == -2 && paired.is_some() {
                            continue;
                        }
                        if matches!(ch, -2 | -3) {
                            if let Some(b) = rec.mask_bounds {
                                let mw = b[3].checked_sub(b[1]).ok_or("Invalid mask")?;
                                let mh = b[2].checked_sub(b[0]).ok_or("Invalid mask")?;
                                if mw < 0 || mh < 0 {
                                    return Err("Invalid mask bounds".into());
                                }
                                let (pw, ph) =
                                    if ["group", "adjustment", "fill"].contains(&rec.layer.kind.as_str()) {
                                        (w, h)
                                    } else {
                                        (lw, lh)
                                    };
                                let mw = mw as u32;
                                let mh = mh as u32;
                                let offset = [b[1] - rec.bounds[1], b[0] - rec.bounds[0]];
                                let mut pixels = Raster::new_depth(pw, ph, depth);
                                let mut original = if rec.retain_mask_original {
                                    let mut original = crate::retained::Original::from(
                                        &Raster::new_depth(mw, mh, depth),
                                    );
                                    original.matrix[4] = f64::from(offset[0]);
                                    original.matrix[5] = f64::from(offset[1]);
                                    original.validate_with_limits(depth, canvas_limits)?;
                                    Some(Box::new(original))
                                } else {
                                    None
                                };
                                if backed {
                                    pixels.retained = original.take();
                                }
                                let (target, decode_offset) = match original.as_mut() {
                                    Some(original) => (original.pixels.as_mut(), [0; 2]),
                                    None => (&mut pixels, offset),
                                };
                                if let Some(streams) = streams.as_deref_mut() {
                                    streams.mask(
                                        record_index,
                                        channel_index,
                                        &rec.layer.id,
                                        target,
                                        decode_offset,
                                        control,
                                    )?;
                                } else {
                                    stream_mask_channel(
                                        bytes,
                                        mw,
                                        mh,
                                        target,
                                        decode_offset,
                                        layout,
                                        control,
                                    )?;
                                }
                                if let Some(original) = original {
                                    let [left, top, right, bottom] =
                                        mask_overlap(mw, mh, [pw, ph], offset);
                                    for y in top..bottom {
                                        control.check()?;
                                        for x in left..right {
                                            pixels.set16(
                                                x + offset[0],
                                                y + offset[1],
                                                original.pixels.get16(x, y),
                                            );
                                        }
                                    }
                                    pixels.retained = Some(original);
                                }
                                let mut steps = vec![
                                    MaskStep {
                                        weight: 1.0,
                                        id: id(),
                                        kind: "fill".into(),
                                        enabled: true,
                                        value: rec.mask_default as f32,
                                        pixels: Raster::new_depth(pw, ph, depth),
                                        settings: serde_json::Value::Null,
                                    },
                                    MaskStep {
                                        weight: 1.0,
                                        id: id(),
                                        kind: "paint".into(),
                                        enabled: true,
                                        value: 0.0,
                                        pixels,
                                        settings: serde_json::Value::Null,
                                    },
                                ];
                                if let Some(density) = rec.mask_density {
                                    steps.push(MaskStep {
                                        weight: 1.,
                                        id: id(),
                                        kind: "density".into(),
                                        enabled: true,
                                        value: f32::from(density),
                                        pixels: Raster::new_depth(pw, ph, depth),
                                        settings: serde_json::Value::Null,
                                    });
                                }
                                rec.layer.mask = Some(Mask {
                                    linked: rec.mask_linked,
                                    cache_key: crate::engine::id(),
                                    enabled: rec.mask_enabled,
                                    steps,
                                });
                            }
                        } else if (-1..=2).contains(&ch) {
                            if staged {
                                continue;
                            }
                            let c = if ch == -1 { 3 } else { ch as usize };
                            if let Some(streams) = streams.as_deref_mut() {
                                streams.layer(
                                    record_index,
                                    channel_index,
                                    &mut pixels,
                                    c,
                                    !initialized,
                                    control,
                                )?;
                            } else {
                                stream_layer_channel(
                                    bytes,
                                    &mut pixels,
                                    c,
                                    !initialized,
                                    layout,
                                    control,
                                )?;
                            }
                            initialized = true;
                        } else {
                            warnings.push(format!("Unsupported Photoshop layer channel: {ch}"));
                        }
                    }
                    if lw > 0 && lh > 0 {
                        rec.layer.pixels = pixels;
                    } else if ["group", "adjustment", "fill"].contains(&rec.layer.kind.as_str()) {
                        rec.layer.pixels = Raster::new_depth(w, h, depth);
                    }
                    if stroke && !staged {
                        // Inspect existing sparse native tiles, without allocating
                        // another full raster. Keep large-source scans cancellable.
                        let mut partial = false;
                        if high {
                            for tile in rec.layer.pixels.samples16.values() {
                                control.check()?;
                                if tile.chunks_exact(4).any(|p| !matches!(p[3], 0 | 65535)) {
                                    partial = true;
                                    break;
                                }
                            }
                        } else {
                            for tile in rec.layer.pixels.tiles.values() {
                                control.check()?;
                                if tile.chunks_exact(4).any(|p| !matches!(p[3], 0 | 255)) {
                                    partial = true;
                                    break;
                                }
                            }
                        }
                        if partial {
                            warnings.push(STROKE_ALPHA_PROTECTION.into());
                        }
                    }
                }
            }
        }
    }
    let mut doc = Document::new_with_limits(w, h, canvas_limits)?;
    doc.source_patterns = source_patterns;
    doc.bit_depth = depth;
    doc.icc_profile = icc_profile;
    doc.srgb_tagged = srgb_tagged;
    doc.psd_intentionally_untagged = intentionally_untagged;
    doc.psd_resolution = psd_resolution;
    doc.psd_print_scale = psd_print_scale;
    doc.psd_pixel_aspect = psd_pixel_aspect;
    doc.psd_grid = psd_grid;
    doc.psd_layer_id_seed = psd_layer_id_seed;
    doc.psd_print_flags = psd_print_flags;
    doc.psd_print_flags_info = psd_print_flags_info;
    doc.psd_copyright = psd_copyright;
    doc.psd_file_info = psd_file_info;
    doc.psd_global_angle = psd_global_angle;
    doc.psd_global_altitude = psd_global_altitude;
    doc.psd_global_mask = global_mask_settings;
    doc.psd_global_metadata = global_metadata;
    doc.layers.clear();
    let mut parents = Vec::<usize>::new();
    let mut source_indices = std::collections::HashMap::new();
    let mut clipped = std::collections::HashSet::new();
    let imported_mask_parameters = records.iter().any(|record| record.mask_parameters);
    for mut rec in records.into_iter().rev() {
        if rec.section == 3 {
            if let Some(folder) = parents.pop() {
                let metadata = &mut doc.layers[folder].psd_metadata;
                for mut entry in rec.layer.psd_metadata.into_iter().filter(|entry| closing_metadata_key(&entry.key)) {
                    entry.key.push_str(":closing");
                    metadata.push(entry);
                }
                if let Err(reason) = validate_metadata(metadata) {
                    warnings.push(reason);
                }
            } else {
                warnings.push("Unbalanced Photoshop folder boundaries".into());
            }
            continue;
        }
        if rec.clipped {
            clipped.insert(rec.layer.id.clone());
        }
        rec.layer.parent = parents.last().map(|&folder| doc.layers[folder].id.clone());
        rec.source.parent_index = rec
            .layer
            .parent
            .as_ref()
            .and_then(|parent| source_indices.get(parent).copied());
        source_indices.insert(rec.layer.id.clone(), rec.source.source_index);
        doc.source_structure.push(rec.source);
        if rec.layer.kind == "group" {
            parents.push(doc.layers.len());
        }
        doc.layers.push(rec.layer);
    }
    if !parents.is_empty() {
        warnings.push("Unbalanced Photoshop folder boundaries".into());
    }
    // Photoshop clipping flags refer to the nearest non-clipped sibling below.
    // Hidden siblings are still structural bases; folders must not leak bases.
    let mut bases = std::collections::HashMap::new();
    for layer in doc.layers.iter_mut().rev() {
        if clipped.contains(&layer.id) {
            if let Some((base, kind)) = bases.get(&layer.parent) {
                if matches!(layer.kind.as_str(), "paint" | "fill" | "adjustment")
                    && (kind == "paint" || kind == "fill")
                {
                    layer.clip_to = Some(String::clone(base));
                } else {
                    warnings.push(format!(
                        "Unsupported Photoshop clipping: {} needs a raster or Solid Color base and a supported follower",
                        layer.name
                    ));
                }
            } else {
                warnings.push(format!(
                    "Photoshop clipping layer has no base in its folder: {}",
                    layer.name
                ));
            }
        } else {
            bases.insert(layer.parent.clone(), (layer.id.clone(), layer.kind.clone()));
        }
    }
    if let Err(error) = crate::compositor::validate_clipping(&doc) {
        warnings.push(format!(
            "Unsupported Photoshop composition: {error}; using the saved composite"
        ));
    }
    if doc.layers.iter().any(|layer| {
        layer.kind == "paint"
            && layer.blend != "normal"
            && layer.effects.iter().any(|effect| effect.enabled)
            && doc.layers.iter().any(|follower| follower.clip_to.as_deref() == Some(&layer.id))
    }) {
        warnings.push("Non-Normal Photoshop styled clipping bases remain protected".into());
    }
    // Calibrated feature imports require exact visual equivalence.
    // Compare exact standard white-matte RGB and alpha samples. Straight-alpha
    // restoration can shift RGB by a word; it must not reject matching encoded
    // samples or introduce an approximate tolerance. Out-of-range matte samples
    // cannot be recovered after clamping and therefore never authorize editing.
    let multiply_alternate = !backed && has_merged_composite && has_photoshop_multiply_alternate(&doc);
    if !backed && warnings.is_empty()
        && (multiply_alternate || imported_mask_parameters || doc.layers.iter().any(|l| l.kind == "fill" || !l.effects.is_empty()
            || !clipping::grouped(&l.psd_metadata)
            || blend_ranges::bytes(&l.psd_metadata).is_some()
            || blend_channels::excluded(&l.psd_metadata) != 0
            || fill_opacity::value(&l.psd_metadata) != 255
            || matches!(l.blend.as_str(),
                "linear_light" | "vivid_light" | "pin_light" | "hard_mix"
                | "darker_color" | "lighter_color" | "hue" | "saturation" | "color" | "luminosity")
            || l.mask.as_ref().is_some_and(|m| !m.linked
                || m.steps.iter().any(|step| step.pixels.retained.is_some()))
            || l.psd_metadata.iter().any(|entry| matches!(entry.key.as_str(), "vmsk" | "tsly" | "vmgm")))
            || !doc.psd_global_metadata.is_empty())
    {
        control.check()?;
        let matches = if merged_white_valid && (!imported_mask_parameters || has_merged_composite) {
            let check = || control.check();
            imported_style_composite_matches(&doc, &merged, &check).and_then(|matches| {
                if matches {
                    return Ok(true);
                }
                if multiply_alternate {
                    check()?;
                    set_photoshop_multiply(&mut doc, true);
                    let alternate = imported_style_composite_matches(&doc, &merged, &check);
                    if alternate == Ok(true) {
                        return alternate;
                    }
                    set_photoshop_multiply(&mut doc, false);
                    alternate?;
                }
                check()?;
                if use_integer_gradient_grid(&mut doc) {
                    imported_style_composite_matches(&doc, &merged, &check)
                } else {
                    Ok(false)
                }
            })
        } else {
            Ok(false)
        };
        match matches {
            Ok(true) => {},
            Ok(false) => warnings.push("Photoshop layer rendering differs from the native saved composite; original definitions remain protected".into()),
            Err(reason) => warnings.push(format!("Cannot reproduce Photoshop layer composite: {reason}; original definitions remain protected")),
        }
        control.check()?;
    }
    warnings.sort();
    warnings.dedup();
    if backed && (doc.layers.is_empty() || !warnings.is_empty()) {
        return Err("Photoshop source requires compatibility decoding".into());
    }
    if doc.layers.is_empty() || !warnings.is_empty() {
        let mut l = Layer::new("Saved PSD composite", "paint", w, h);
        l.pixels = merged;
        doc.layers = vec![l];
        doc.read_only = !warnings.is_empty();
    }
    if high && doc.read_only {
        warnings.push("16-bit Photoshop source features are read-only and protected. The saved composite retains 16-bit samples; an explicit editable copy flattens its layers without reducing precision".into());
    }
    doc.warnings = warnings;
    if !doc.read_only {
        doc.source_structure.clear();
        doc.source_patterns.clear();
    }
    control.progress("Ready", 1, 1)?;
    Ok(doc)
}

fn needs_integer_gradient_grid(effect: &crate::effects::Effect) -> bool {
    effect.kind == "gradient_overlay" && effect.enabled && effect.settings["pixel_grid"] != true
}

pub(crate) fn has_integer_gradient_alternate(doc: &Document) -> bool {
    doc.layers.iter().flat_map(|layer| &layer.effects).any(needs_integer_gradient_grid)
}

/// One alternate placement for the whole document, after a composite mismatch.
/// Both open paths must compare the complete composite before adopting it.
pub(crate) fn use_integer_gradient_grid(doc: &mut Document) -> bool {
    let mut changed = false;
    for effect in doc.layers.iter_mut().flat_map(|layer| &mut layer.effects) {
        if needs_integer_gradient_grid(effect) {
            std::sync::Arc::make_mut(&mut effect.settings)["pixel_grid"] = serde_json::json!(true);
            changed = true;
        }
    }
    if changed {
        // The alternate must not reuse the first render's derived identities.
        crate::effects::invalidate(doc, &[serde_json::json!({"op":"document.settings"})]);
    }
    changed
}

pub(crate) fn has_photoshop_multiply_alternate(doc: &Document) -> bool {
    doc.bit_depth == 8 && !doc.psd_rgb8_multiply && doc.layers.iter().any(|layer| {
        layer.visible && layer.kind == "paint" && layer.blend == "multiply" && layer.clip_to.is_none()
    })
}

pub(crate) fn set_photoshop_multiply(doc: &mut Document, enabled: bool) {
    doc.psd_rgb8_multiply = enabled;
    crate::effects::invalidate(doc, &[serde_json::json!({"op":"document.settings"})]);
}

fn imported_style_composite_matches(
    doc: &Document, saved: &Raster, check: &crate::render::Check<'_>,
) -> Result<bool, String> {
    if saved.is16() {
        crate::depth16::render_crop_checked(doc, [0, 0, doc.width as i32, doc.height as i32], check)
            .map(|image| same_standard_composite(&image.words, saved))
    } else {
        doc.preview_native8_checked(None, doc.width.max(doc.height), None, false, check)
            .map(|(_, _, pixels, _)| same_standard_composite(&pixels, saved))
    }
}
fn same_standard_composite<T: Copy + Into<u32>>(actual: &[T], saved: &Raster) -> bool {
    if actual.len() != saved.width as usize * saved.height as usize * 4 {
        return false;
    }
    let max = if saved.is16() { 65535u64 } else { 255 };
    let divisor = if saved.is16() { 1 } else { 257 };
    let matte = |color: u64, alpha: u64| (color * alpha + max * (max - alpha) + max / 2) / max;
    actual.chunks_exact(4).enumerate().all(|(index, pixel)| {
        let source = saved
            .get16(
                (index % saved.width as usize) as i32,
                (index / saved.width as usize) as i32,
            )
            .map(|v| u64::from(v) / divisor);
        let alpha = u64::from(pixel[3].into());
        alpha == source[3]
            && (0..3)
                .all(|c| matte(u64::from(pixel[c].into()), alpha) == matte(source[c], source[3]))
    })
}
/// Shared bounded global block parsing: PSB widens specific tagged lengths, not every block.
type GlobalBlocks<'a> = (&'a [u8], &'a [u8], Vec<(&'a [u8], &'a [u8])>);
fn global_blocks(section: &[u8], layout: Layout) -> Result<GlobalBlocks<'_>, String> {
    if section.is_empty() {
        return Ok((&[], &[], vec![]));
    }
    let mut r = Cursor::new(section);
    let primary = layout.block(&mut r)?;
    if r.pos == section.len() {
        return Ok((primary, &[], vec![]));
    }
    let mask = r.block()?; // Global mask length stays 32-bit in PSB.
    let mut tags = Vec::new();
    while r.pos < section.len() {
        if section.len() - r.pos < 12 {
            if section[r.pos..].iter().all(|v| *v == 0) {
                break;
            }
            return Err("Truncated global Photoshop layer information".into());
        }
        let signature = r.bytes(4)?;
        if !matches!(signature, b"8BIM" | b"8B64") {
            return Err("Invalid global Photoshop layer signature".into());
        }
        let key = r.bytes(4)?;
        let payload = layout.tag_block(&mut r, key)?;
        r.bytes((4 - payload.len() % 4) % 4)?;
        if tags.len() >= 4096 {
            return Err("Too many global Photoshop layer blocks".into());
        }
        tags.push((key, payload));
    }
    Ok((primary, mask, tags))
}
fn global_warnings(section: &[u8], depth: u16, layout: Layout) -> Result<Vec<String>, String> {
    let (primary, mask, tags) = global_blocks(section, layout)?;
    let mut warnings = vec![];
    if !mask.is_empty() && validate_global_mask_settings(mask).is_err() {
        warnings
            .push("Photoshop global layer masks are protected; using the saved composite".into());
    }
    let mut seen = std::collections::HashSet::new();
    for (key, data) in tags {
        if !seen.insert(key) {
            warnings.push(format!(
                "Duplicate global Photoshop layer tag: {}",
                String::from_utf8_lossy(key)
            ));
        }
        let supported = (depth == 8 && key == b"Layr")
            || (depth == 16 && key == b"Lr16")
            || (data.is_empty()
                && ((depth == 8 && key == b"Mtrn") || (depth == 16 && key == b"Mt16")))
            || std::str::from_utf8(key)
                .is_ok_and(|key| global_metadata::validate_entry(key, data).is_ok());
        if !supported {
            warnings.push(format!(
                "Unsupported global Photoshop feature: {}",
                String::from_utf8_lossy(key)
            ));
        }
        if (depth == 8 && key == b"Layr" && !primary.is_empty())
            || (depth == 16
                && key == b"Lr16"
                && !matches!(primary, [] | [0, 0] | [0, 0, 0, 0]))
        {
            warnings.push("Conflicting primary and global Photoshop layer records".into());
        }
    }
    Ok(warnings)
}
fn layer_info(section: &[u8], depth: u16, layout: Layout) -> Result<&[u8], String> {
    let (mut info, _, tags) = global_blocks(section, layout)?;
    for (key, payload) in tags {
        if (depth == 16 && key == b"Lr16") || (depth == 8 && key == b"Layr") {
            info = payload;
        }
    }
    Ok(info)
}
// Supplementary editable definitions must retain the actual standard metadata,
// including compatibility markers. The standard checksum alone does not bind
// the private document's per-layer metadata to those bytes.
fn metadata_sources_match(
    section: &[u8],
    layout: Layout,
    doc: &Document,
    format: u32,
    streams: Option<&dyn IndexedChannels>,
    control: &crate::loading::Control,
) -> Result<bool, String> {
    let tones = ToneRecords::for_source_format(format, doc.bit_depth);
    let mut plan = standard_plan_for(doc, format >= 40, format >= 44, tones);
    // Sources through 34 baked every adjustment. Keep that exact layout when
    // validating older saves. Format 35 supports unclipped nvrt, and 40 binds clipping.
    if format < 35 && doc.layers.iter().any(|l| l.kind == "adjustment") {
        plan.composite = true;
    }
    let pattern_geometry = patterns::Geometry::for_source_format(format);
    let patterns = if format >= patterns::STANDARD_FORMAT {
        resident_patterns(doc, &plan, pattern_geometry, format, &|| control.check())?
    } else { patterns::StandardPatterns::default() };
    let mut actual_patterns = global_blocks(section, layout)?.2.into_iter()
        .filter_map(|(key, data)| (key == b"Patt" && !data.is_empty()).then_some(data));
    if actual_patterns.next().unwrap_or_default() != patterns.bytes() || actual_patterns.next().is_some() {
        return Ok(false);
    }
    let mut expected = if !plan.composite || plan.hidden_sources {
        standard_order(doc)
            .into_iter()
            .map(|(layer, section)| {
                let metadata = standard_layer_metadata(&layer.psd_metadata, section)
                    .map(|(key, data)| LayerMetadata { key: key.into(), data: data.to_vec() })
                    .collect();
                if section == 3 {
                    (metadata, None, None, false, false, None, None, None, None, None, None, false, section)
                } else {
                    let standard_style = if format >= COLOR_OVERLAY_FORMAT {
                        standard_styles(doc, layer, section, &plan, &patterns).filter(|overlay| overlay.source_format() <= format)
                    } else { None };
                    (
                        metadata,
                        (format >= 35
                            && (format >= 44 || layer.blend == "normal")
                            && section == 0
                            && adjustments::standard_invert(
                                layer.into(),
                                doc.width,
                                doc.height,
                                format >= 40 && !plan.composite,
                            )).then(|| scene_save36::blend(&layer.blend)).map(|blend| blend.expect("validated standard Invert blend")),
                        layer.mask.as_ref()
                            .filter(|_| format >= 38)
                            .map(|mask| {
                                let frame = standard_mask_frame(doc, layer, standard_style);
                                let density = (format >= MASK_DENSITY_FORMAT)
                                    .then(|| standard_mask_density(layer.into())).flatten();
                                let expanded = extended_mask(layer.into(), frame, density.is_some())
                                    .filter(|_| format >= 43 || format >= 39 && !mask.linked);
                                standard36::Mask {
                                    bounds: expanded.as_ref().map_or(frame, |m| m.bounds),
                                    default: expanded.as_ref().map_or_else(
                                        || if density.is_some() {
                                            density_mask_default(crate::engine::view::MaskView::Resident(mask))
                                        } else { 255 },
                                        |m| m.default,
                                    ),
                                    enabled: mask.enabled,
                                    linked: mask.linked,
                                    dual: format >= 60 && dual_mask(layer),
                                    density,
                                }
                            }),
                        layer.clip_to.is_some(),
                        format >= 40 && doc.layers.iter()
                            .any(|l| l.clip_to.as_deref() == Some(layer.id.as_str())),
                        if format >= 41 && !plan.composite && section == 0 {
                            standard_style.filter(|overlay| overlay.fill).map(|_| layer.color).or_else(|| {
                                adjustments::standard_solid(layer.into(), doc.width, doc.height)
                            })
                        } else {
                            None
                        },
                        standard_style,
                        if format >= 46 && !plan.composite && section == 0 {
                            gradient_fill::standard(layer.into(), doc.width, doc.height)
                        } else { None },
                        if format >= patterns::STANDARD_FORMAT && !plan.composite && section == 0 {
                            patterns::standard(layer.into(), doc.width, doc.height, pattern_geometry)
                                .and_then(|fill| patterns.record(fill))
                        } else { None },
                        if format >= 48 && section == 0 {
                            adjustments::standard_tone(layer.into(), doc.width, doc.height, !plan.composite, tones)
                                .map(|payload| (payload, scene_save36::blend(&layer.blend).expect("validated standard adjustment blend")))
                        } else { None },
                        matches!(section, 1 | 2).then(|| (
                            scene_save36::blend(&layer.blend).expect("validated standard folder blend"),
                            blend_ranges::bytes(&layer.psd_metadata).is_some()
                                || blend_channels::excluded(&layer.psd_metadata) != 0,
                        )),
                        layer.locked,
                        section,
                    )
                }
            })
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    if plan.composite {
        expected.insert(0, (vec![], None, None, false, false, None, None, None, None, None, None, false, 0));
    }
    if expected.is_empty() {
        expected.push((vec![], None, None, false, false, None, None, None, None, None, None, false, 0));
    }
    expected.reverse();
    let info = layer_info(section, doc.bit_depth, layout)?;
    let mut records = Cursor::new(info);
    let count = usize::from(records.i16()?.unsigned_abs());
    if count > 200 || count != expected.len() {
        return Ok(false);
    }
    let mut data_offset = 0usize;
    let mut pairs = vec![];
    for (record_index, (expected, expected_invert, expected_mask, expected_clipped, expected_clipping_base, expected_solid, expected_style, expected_gradient, expected_pattern, expected_tone, expected_folder, expected_locked, expected_section)) in expected.into_iter().enumerate() {
        control.check()?;
        let bounds = records.bytes(16)?;
        if expected_style.is_some_and(|overlay| {
            let [left, top, right, bottom] = overlay.bounds;
            [top, left, bottom, right].into_iter().zip(bounds.chunks_exact(4))
                .any(|(value, bytes)| bytes != value.to_be_bytes())
        }) {
            return Ok(false);
        }
        if (expected_solid.is_some() || expected_gradient.is_some() || expected_pattern.is_some() || expected_tone.is_some())
            && ![0, 0, doc.height as i32, doc.width as i32]
                .into_iter()
                .zip(bounds.chunks_exact(4))
                .all(|(value, bytes)| bytes == value.to_be_bytes())
        {
            return Ok(false);
        }
        let channels = records.u16()?;
        if channels > 8 {
            return Ok(false);
        }
        let mut channel_ids = std::collections::HashSet::new();
        let mut mask_channels = [None, None];
        for index in 0..usize::from(channels) {
            let channel = records.i16()?;
            if !(-3..=2).contains(&channel) || !channel_ids.insert(channel) {
                return Ok(false);
            }
            let bytes = records.length(layout.large())?;
            if matches!(channel, -2 | -3) {
                mask_channels[(-channel - 2) as usize] = Some((index, data_offset, bytes));
            }
            data_offset = data_offset.checked_add(bytes).ok_or("Photoshop channel range overflow")?;
        }
        let attributes = records.bytes(12)?;
        if expected_folder.is_some_and(|(_, advanced)| {
            &attributes[..4] != b"8BIM" || advanced && bounds != [0; 16]
        }) {
            return Ok(false);
        }
        if format >= 40
            && (attributes[9] != u8::from(expected_clipped)
                || expected_invert.is_some_and(|blend| &attributes[..4] != b"8BIM" || attributes[4..8] != blend)
                || expected_style.is_some_and(|overlay| &attributes[..4] != b"8BIM"
                    || attributes[4..8] != overlay.layer_blend || attributes[8] != overlay.layer_opacity
                    || !overlay.fill && (attributes[10] & 0x18 != 0
                        || ![-1, 0, 1, 2].iter().all(|channel| channel_ids.contains(channel))))
                || expected_tone.as_ref().is_some_and(|(_, blend)| &attributes[..4] != b"8BIM" || &attributes[4..8] != blend)
                || (expected_solid.is_some() || expected_gradient.is_some() || expected_pattern.is_some() || expected_tone.is_some()) && attributes[10] & 0x18 != 0x18)
        {
            return Ok(false);
        }
        let extra = records.block()?;
        let mut tags = Cursor::new(extra);
        let mask = tags.block()?;
        if format >= 38 {
            let valid = match expected_mask {
                Some(expected) => {
                    let flags = standard_mask_flags(expected.enabled, expected.linked);
                    let payload = standard_mask_payload(expected.bounds, expected.default, flags, expected.dual, expected.density);
                    mask == &payload[..if expected.dual { 36 } else { 20 }]
                        && channel_ids.contains(&-2)
                        && channel_ids.contains(&-3) == expected.dual
                }
                None => mask.is_empty() && !channel_ids.contains(&-2) && !channel_ids.contains(&-3),
            };
            if !valid {
                return Ok(false);
            }
            if expected_mask.is_some_and(|mask| mask.dual) {
                let header = standard_mask_header(mask).ok_or("Invalid paired Photoshop mask header")?;
                let [Some((first, start, rendered)), Some((second, end, user))] = mask_channels else {
                    return Ok(false);
                };
                if let Some(streams) = streams {
                    if !streams.same_mask_samples(record_index, first, second, header, doc.bit_depth, control)? {
                        return Ok(false);
                    }
                } else {
                    pairs.push((header, start..start + rendered, end..end + user));
                }
            }
        } else if channel_ids.contains(&-3)
            || mask.get(17).is_some_and(|flags| flags & MASK_UNLINKED != 0)
        {
            return Ok(false);
        }
        let Ok(ranges) = blend_ranges::parse(tags.block()?, channels) else {
            return Ok(false);
        };
        let name = usize::from(tags.u8()?);
        tags.bytes(name)?;
        tags.bytes((name + 1).div_ceil(4) * 4 - name - 1)?;
        let mut actual = vec![];
        if !ranges.is_empty() {
            if format < 64 {
                return Ok(false);
            }
            actual.push(LayerMetadata { key: blend_ranges::KEY.into(), data: ranges.to_vec() });
        }
        let mut actual_invert = false;
        let mut actual_tone = false;
        let mut actual_tone_companion = false;
        let mut actual_mask_effect_mode = false;
        let mut actual_clipping = None;
        let mut actual_fill = false;
        let mut actual_solid = false;
        let mut actual_style = false;
        let mut actual_gradient = false;
        let mut actual_pattern = false;
        let mut actual_section = None;
        let mut actual_channels = false;
        let mut actual_locked = None;
        let mut actual_unicode_name = false;
        let mut seen_tags = std::collections::HashSet::new();
        while tags.pos + 12 <= extra.len() {
            let signature = tags.bytes(4)?;
            let key = tags.bytes(4)?;
            // The supplementary source cannot account for additional standard
            // layer features, even if its checksum binds it to this file.
            if signature != b"8BIM" || !seen_tags.insert(key)
                || !(adjustments::is_tone_tag(key)
                    || matches!(key, b"luni" | b"lspf" | b"brst" | b"lsct" | b"lsdk"
                        | b"nvrt" | b"lmgm" | b"SoCo" | b"lfx2" | b"lmfx" | b"lrFX"
                        | b"GdFl" | b"PtFl" | b"clbl" | b"knko" | b"iOpa" | b"fxrp"
                        | b"lclr" | b"lnsr" | b"lyid" | b"shmd" | b"lyvr" | b"vmsk"
                        | b"tsly" | b"vmgm" | b"infx" | b"sn2P"))
            {
                return Ok(false);
            }
            let payload = layout.tag_block(&mut tags, key)?;
            tags.bytes(payload.len() % 2)?;
            if key == b"knko" && standard_flag(key, payload).is_err() {
                return Ok(false);
            }
            if key == b"luni" {
                if signature != b"8BIM" || actual_unicode_name
                    || unicode_layer_name(payload, 0).is_err()
                {
                    return Ok(false);
                }
                actual_unicode_name = true;
            }
            if key == b"lspf" {
                let locked = payload != [0; 4];
                if signature != b"8BIM" || standard_flag(key, payload).is_err()
                    || actual_locked.replace(locked).is_some() || locked != expected_locked
                {
                    return Ok(false);
                }
                if payload == COMPLETE_LAYER_LOCK {
                    actual.push(LayerMetadata { key: "lspf".into(), data: payload.to_vec() });
                }
            }
            if key == b"brst" {
                let Some(excluded) = blend_channels::parse(payload) else {
                    return Ok(false);
                };
                if actual_channels || signature != b"8BIM" || excluded != 0 && format < 70 {
                    return Ok(false);
                }
                actual_channels = true;
                if excluded != 0 {
                    actual.push(LayerMetadata { key: "brst".into(), data: payload.to_vec() });
                }
            }
            if matches!(key, b"lsct" | b"lsdk") {
                let Ok((section, mode)) = section_divider(signature, payload) else {
                    return Ok(false);
                };
                let record_blend = blend_name(&attributes[4..8]);
                let blend_matches = if let Some((expected_blend, advanced)) = expected_folder {
                    let expected_mode = blend_name(&expected_blend);
                    mode.or(record_blend) == expected_mode && (!advanced || record_blend == expected_mode)
                } else {
                    mode.is_none_or(|mode| record_blend == Some(mode))
                };
                if actual_section.replace(section).is_some() || section != expected_section
                    || format < CLOSED_FOLDER_FORMAT && section == 2
                    || !blend_matches
                {
                    return Ok(false);
                }
            }
            if key == b"nvrt" {
                if actual_invert || signature != b"8BIM" || !payload.is_empty() {
                    return Ok(false);
                }
                actual_invert = true;
            }
            if adjustments::is_tone_tag(key) {
                let Some((expected, _)) = &expected_tone else { return Ok(false); };
                if signature != b"8BIM" {
                    return Ok(false);
                }
                let (tag, bytes) = expected.parts();
                if key == tag && payload == bytes && !actual_tone {
                    actual_tone = true;
                } else if !actual_tone_companion
                    && expected.companion().is_some_and(|(tag, bytes)| key == tag && payload == bytes)
                {
                    actual_tone_companion = true;
                } else {
                    return Ok(false);
                }
            }
            if key == b"lmgm" {
                if actual_mask_effect_mode || signature != b"8BIM" || payload != [0; 4] {
                    return Ok(false);
                }
                actual_mask_effect_mode = true;
            }
            if key == b"SoCo" {
                if actual_solid || signature != b"8BIM" || expected_solid.is_none_or(|color| {
                    payload != adjustments::solid_descriptor(color)
                }) {
                    return Ok(false);
                }
                actual_solid = true;
            }
            if matches!(key, b"lfx2" | b"lmfx" | b"lrFX") {
                if actual_style || signature != b"8BIM"
                    || expected_style.as_ref().is_none_or(|overlay| {
                        key != overlay.tag() || !styles::matches_descriptor(overlay, payload)
                    })
                {
                    return Ok(false);
                }
                actual_style = true;
            }
            if key == b"GdFl" {
                let Some(settings) = expected_gradient else { return Ok(false); };
                if actual_gradient || signature != b"8BIM" || payload != gradient_fill::descriptor(settings)?.bytes() {
                    return Ok(false);
                }
                actual_gradient = true;
            }
            if key == b"PtFl" {
                let Some(id) = expected_pattern else { return Ok(false); };
                if actual_pattern || signature != b"8BIM" || payload != patterns::descriptor(id).bytes() { return Ok(false); }
                actual_pattern = true;
            }
            if key == b"clbl" {
                if actual_clipping.is_some() || signature != b"8BIM"
                    || standard_flag(b"clbl", payload).is_err() || payload[0] == 0 && format < 63
                {
                    return Ok(false);
                }
                actual_clipping = Some(payload[0] == 1);
                if payload[0] == 0 {
                    actual.push(LayerMetadata { key: "clbl".into(), data: payload.to_vec() });
                }
            }
            if key == b"iOpa" {
                let Some(amount) = fill_opacity::parse(payload) else {
                    return Ok(false);
                };
                if actual_fill || signature != b"8BIM" || amount != 255 && format < 62 {
                    return Ok(false);
                }
                actual_fill = true;
                if amount != 255 {
                    actual.push(LayerMetadata { key: "iOpa".into(), data: payload.to_vec() });
                }
            }
            if matches!(key, b"fxrp" | b"lclr" | b"lnsr" | b"lyid" | b"shmd" | b"lyvr" | b"vmsk" | b"tsly" | b"vmgm" | b"infx" | b"sn2P") {
                if signature != b"8BIM" || payload.len() > 128 || key == b"infx" && format < INTERIOR_GROUP_FORMAT
                    || expected_section == 3 && key != b"lyid" && format < CLOSING_METADATA_FORMAT
                {
                    return Ok(false);
                }
                actual.push(LayerMetadata {
                    key: std::str::from_utf8(key).unwrap().into(),
                    data: payload.to_vec(),
                });
            }
        }
        if seen_tags.contains(b"lsct".as_slice()) && seen_tags.contains(b"lsdk".as_slice())
            || extra[tags.pos..].iter().any(|v| *v != 0)
            || actual_invert != expected_invert.is_some()
            || actual_tone != expected_tone.is_some()
            || actual_tone_companion != expected_tone.as_ref().is_some_and(|(tone, _)| tone.companion().is_some())
            || actual_solid != expected_solid.is_some()
            || actual_style != expected_style.is_some()
            || actual_gradient != expected_gradient.is_some()
            || actual_pattern != expected_pattern.is_some()
            || expected_folder.is_some() && actual_section.is_none()
            || format >= CLOSED_FOLDER_FORMAT && actual_section.unwrap_or(0) != expected_section
            || actual_mask_effect_mode != expected_mask.is_some_and(|mask| !mask.linked)
            || (attributes[10] & 1 != 0 && actual_locked != Some(true))
            || format >= 40 && (actual_clipping == Some(true)) != (expected_clipping_base && clipping::grouped(&expected))
            || validate_metadata(&actual).is_err()
            || actual.len() != expected.len()
            || actual.iter().any(|m| !expected.contains(m))
        {
            return Ok(false);
        }
    }
    let pixels = &info[records.pos..];
    for (header, rendered, user) in pairs {
        let rendered = pixels.get(rendered).ok_or("Truncated Photoshop rendered mask")?;
        let user = pixels.get(user).ok_or("Truncated Photoshop real mask")?;
        if !mask_payloads_match(rendered, user, header, doc.bit_depth, layout, control)? {
            return Ok(false);
        }
    }
    Ok(true)
}
fn merged_transparency(section: &[u8], depth: u16, layout: Layout) -> Result<bool, String> {
    let (info, _, tags) = global_blocks(section, layout)?;
    let mut alpha = info.len() >= 2 && i16::from_be_bytes(info[..2].try_into().unwrap()) < 0;
    for (key, payload) in tags {
        if (depth == 8 && key == b"Mtrn") || (depth == 16 && key == b"Mt16") {
            alpha = true;
        }
        if ((depth == 16 && key == b"Lr16") || (depth == 8 && key == b"Layr")) && payload.len() >= 2
        {
            alpha |= i16::from_be_bytes(payload[..2].try_into().unwrap()) < 0;
        }
    }
    Ok(alpha)
}

/// Bounded display decode: no 16-bit sources are replaced or marked editable.
#[cfg(test)]
fn decode_planes_16(
    data: &[u8],
    w: u32,
    h: u32,
    count: usize,
    layout: Layout,
) -> Result<Vec<u16>, String> {
    let mut r = Cursor::new(data);
    let kind = r.u16()?;
    let pixels = (w as usize)
        .checked_mul(h as usize)
        .ok_or("PSD composite size overflow")?;
    let row_bytes = (w as usize).checked_mul(2).ok_or("PSD row size overflow")?;
    let plane_bytes = pixels.checked_mul(2).ok_or("PSD channel size overflow")?;
    let total = plane_bytes
        .checked_mul(count)
        .ok_or("PSD composite size overflow")?;
    if total > 256 * 1024 * 1024 {
        return Err("16-bit PSD composite exceeds the 256 MiB decoded-image budget".into());
    }
    let mut planes: Cow<'_, [u8]> = match kind {
        0 => Cow::Borrowed(r.bytes(total)?),
        1 => {
            let rows = (h as usize)
                .checked_mul(count)
                .ok_or("PSD row table overflow")?;
            let lengths = row_lengths(
                &mut std::io::Cursor::new(&data[r.pos..]),
                rows,
                row_bytes,
                layout,
            )?;
            r.bytes(rows * if layout.large() { 4 } else { 2 })?;
            let mut out = Vec::with_capacity(total);
            for length in lengths {
                let row = r.bytes(length)?;
                let start = out.len();
                let mut i = 0;
                while i < row.len() {
                    let packet = row[i] as i8;
                    i += 1;
                    match packet {
                        0..=127 => {
                            let n = packet as usize + 1;
                            if i + n > row.len() || out.len() - start + n > row_bytes {
                                return Err("Invalid 16-bit RLE literal packet".into());
                            }
                            out.extend_from_slice(&row[i..i + n]);
                            i += n;
                        }
                        -127..=-1 => {
                            let n = (-packet as i16 + 1) as usize;
                            let value = *row.get(i).ok_or("Invalid 16-bit RLE repeat packet")?;
                            i += 1;
                            if out.len() - start + n > row_bytes {
                                return Err("16-bit RLE row overflow".into());
                            }
                            out.resize(out.len() + n, value);
                        }
                        _ => {}
                    }
                }
                if out.len() - start != row_bytes {
                    return Err("Invalid 16-bit RLE row width".into());
                }
            }
            Cow::Owned(out)
        }
        2 | 3 => {
            let mut out = vec![0; total + 1];
            let mut zip = flate2::Decompress::new(true);
            let status = zip
                .decompress(&data[2..], &mut out, flate2::FlushDecompress::Finish)
                .map_err(|e| e.to_string())?;
            if status != flate2::Status::StreamEnd || zip.total_out() != total as u64 {
                return Err("Truncated or incorrectly sized 16-bit ZIP composite".into());
            }
            out.truncate(total);
            Cow::Owned(out)
        }
        _ => return Err("Unsupported 16-bit composite compression".into()),
    };
    if kind == 3 && row_bytes > 0 {
        // Prediction deltas are 16-bit big-endian samples, reset at each row/plane.
        for row in planes.to_mut().chunks_exact_mut(row_bytes) {
            let mut previous = 0u16;
            for sample in row.chunks_exact_mut(2) {
                previous = previous.wrapping_add(u16::from_be_bytes([sample[0], sample[1]]));
                sample.copy_from_slice(&previous.to_be_bytes());
            }
        }
    }
    Ok(planes
        .chunks_exact(2)
        .map(|v| u16::from_be_bytes([v[0], v[1]]))
        .collect())
}

#[cfg(test)]
fn decode_channel_16(data: &[u8], w: u32, h: u32, layout: Layout) -> Result<Vec<u16>, String> {
    decode_planes_16(data, w, h, 1, layout)
}
pub fn validate(doc: &Document) -> Result<(), String> {
    if doc.psd_intentionally_untagged && (doc.srgb_tagged || doc.icc_profile.is_some()) {
        return Err(CONFLICTING_PROFILE_DECLARATIONS.into());
    }
    if let Some(resolution) = &doc.psd_resolution {
        parse_resolution(resolution)?;
    }
    if let Some(scale) = &doc.psd_print_scale {
        parse_print_scale(scale)?;
    }
    if let Some(aspect) = &doc.psd_pixel_aspect {
        parse_pixel_aspect(aspect)?;
    }
    if let Some(grid) = &doc.psd_grid {
        parse_grid(grid)?;
    }
    if let Some(flags) = &doc.psd_print_flags {
        parse_print_flags(flags)?;
    }
    if let Some(info) = &doc.psd_print_flags_info {
        parse_print_flags_info(info)?;
    }
    if let Some(flag) = &doc.psd_copyright {
        parse_copyright(flag)?;
    }
    if let Some(bytes) = &doc.psd_file_info {
        file_info::validate(bytes)?;
    }
    if let Some(angle) = &doc.psd_global_angle {
        parse_global_angle(angle)?;
    }
    global_metadata::validate(&doc.psd_global_metadata)?;
    if let Some(mask) = &doc.psd_global_mask {
        mask.validate()?;
    }
    if let Some(profile) = &doc.icc_profile {
        if doc.srgb_tagged {
            return Err(
                "A document cannot have both an original ICC profile and an sRGB tag".into(),
            );
        }
        if !doc.read_only {
            return Err(
                "Profiled sources must stay read-only until explicitly converted to sRGB".into(),
            );
        }
        if profile.len() > crate::color_profile::MAX_PROFILE_BYTES {
            return Err("ICC profile exceeds the 4 MiB limit".into());
        }
    }
    crate::compositor::validate_clipping(doc)?;
    for coverage in [&doc.selection_coverage, &doc.selection_previous]
        .into_iter()
        .flatten()
    {
        coverage.validate()?;
    }
    check_size(doc.width, doc.height)?;
    if !matches!(doc.bit_depth, 8 | 16) {
        return Err("Unsupported document bit depth".into());
    }
    if doc.layers.len() > 100 {
        return Err("Layer limit".into());
    }
    validate_photoshop_layer_ids(doc.layers.iter().map(|layer| layer.psd_metadata.as_slice()))?;
    let mut ids = std::collections::HashSet::new();
    let mut total = 0usize;
    for l in &doc.layers {
        validate_layer_metadata(&l.psd_metadata, l.locked, l.kind == "group")?;
        if let Some(source) = &l.source {
            if l.kind != "paint" {
                return Err("Editable text/vector sources require paint raster projections".into());
            }
            source.validate_structure()?;
        }
        if !ids.insert(l.id.clone()) {
            return Err("Duplicate layer ID".into());
        }
        check_size(l.pixels.width, l.pixels.height)?;
        l.pixels.validate_layout()?;
        if l.pixels.depth != doc.bit_depth {
            return Err("Layer precision does not match the document".into());
        }
        if l.x.unsigned_abs() > 100000
            || l.y.unsigned_abs() > 100000
            || !["paint", "fill", "group", "adjustment"].contains(&l.kind.as_str())
        {
            return Err("Invalid layer coordinates or kind".into());
        }
        if !crate::raster::layer_blends(&l.kind).any(|(mode, _)| mode == l.blend) {
            return Err("Invalid blend mode".into());
        }
        if l.kind == "adjustment"
            && l.mask.is_none()
            && (l.x != 0
                || l.y != 0
                || l.pixels.width != doc.width
                || l.pixels.height != doc.height)
        {
            return Err("Adjustment layers use document coordinates".into());
        }
        if !l.opacity.is_finite() {
            return Err("Invalid opacity".into());
        }
        total += l.pixels.stored_bytes();
        let mut p = l.parent.as_deref();
        let mut depth = 0;
        while let Some(parent) = p {
            let n = doc
                .layers
                .iter()
                .find(|x| x.id == parent && x.kind == "group")
                .ok_or("Invalid group parent")?;
            p = n.parent.as_deref();
            depth += 1;
            if depth > 16 {
                return Err("Cyclic groups".into());
            }
        }
        if l.effects.len() > 32 {
            return Err("Effect stack limit".into());
        }
        for e in &l.effects {
            crate::effects::validate_weight(e.weight)?;
            crate::effects::validate(&e.kind, &e.settings)?;
        }
        if let Some(m) = &l.mask {
            if m.steps.len() > 32 {
                return Err("Too many mask steps".into());
            }
            for s in &m.steps {
                crate::effects::validate_weight(s.weight)?;
                s.pixels.validate_layout()?;
                if s.pixels.depth != doc.bit_depth {
                    return Err("Mask precision does not match the document".into());
                }
                total += s.pixels.stored_bytes();
                let valid = match s.kind.as_str() {
                    "blur" => (0.0..=64.0).contains(&s.value),
                    "levels" => (0.1..=5.0).contains(&s.value),
                    "fill" | "paint" | "invert" => (0.0..=255.0).contains(&s.value),
                    "density" => crate::engine::mask_parameter("density", f64::from(s.value)).is_ok()
                        && s.settings.is_null(),
                    "curves" | "adjust" | "gaussian" => {
                        crate::mask::commands::validate_settings(&s.kind, &s.settings)?;
                        true
                    }
                    _ => false,
                };
                if !s.value.is_finite() || !valid {
                    return Err("Invalid mask step".into());
                }
            }
        }
        if total > 512 * 1024 * 1024 {
            return Err("Document memory limit".into());
        }
    }
    if doc.bit_depth == 16 {
        crate::depth16::validate_budget(doc)?;
    } else {
        crate::mask::validate_budget(&doc.layers)?;
        crate::effects::validate_budget(doc)?;
    }
    Ok(())
}

#[cfg(test)]
mod storage_tests {
    use super::*;

    #[test]
    fn intentionally_untagged_rgb_survives_working_saves_and_conflicting_profiles_are_protected() {
        let mut profile = b"8BIM\x04\x0f\0\0".to_vec();
        let srgb = crate::color_profile::srgb_profile();
        block(&mut profile, srgb);
        if srgb.len() % 2 != 0 {
            profile.push(0);
        }
        let mut invalid = *UNTAGGED_PROFILE_RECORD;
        invalid[12] = 2;
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.psd_intentionally_untagged = true;
            for layout in [Layout::Psd, Layout::Psb] {
                let encoded = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&encoded);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                let resources = cursor.block().unwrap();
                let with_resources = |resources: &[u8]| {
                    let mut result = encoded[..start].to_vec();
                    block(&mut result, resources);
                    result.extend_from_slice(&encoded[cursor.pos..]);
                    result
                };
                let standard = with_resources(UNTAGGED_PROFILE_RECORD);
                for bytes in [&encoded, &standard] {
                    let loaded = decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap();
                    assert!(!loaded.read_only && !loaded.srgb_tagged);
                    assert!(loaded.psd_intentionally_untagged);
                    assert_eq!(loaded.layers[0].pixels, doc.layers[0].pixels);
                }
                for (bytes, reason) in [
                    (with_resources(&[resources, &profile].concat()), CONFLICTING_PROFILE_DECLARATIONS),
                    (with_resources(&[resources, UNTAGGED_PROFILE_RECORD].concat()), "Duplicate Photoshop untagged profile"),
                    (with_resources(&invalid), "Invalid Photoshop untagged profile"),
                ] {
                    let loaded = decode(&bytes).unwrap();
                    assert!(loaded.read_only);
                    assert!(!loaded.psd_intentionally_untagged);
                    assert!(loaded.warnings.iter().any(|warning| warning.contains(reason)));
                    assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                }
            }
        }
    }

    #[test]
    fn square_pixel_aspect_round_trips_and_other_ratios_keep_the_saved_composite() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.psd_pixel_aspect = Some(TEST_PIXEL_ASPECT);
            for layout in [Layout::Psd, Layout::Psb] {
                let encoded = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&encoded);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                let resources = cursor.block().unwrap();
                let replace_resources = |resources: &[u8]| {
                    let mut result = encoded[..start].to_vec();
                    block(&mut result, resources);
                    result.extend_from_slice(&encoded[cursor.pos..]);
                    result
                };
                let mut v1 = TEST_PIXEL_ASPECT;
                v1[3] = 1;
                let standard = replace_resources(&pixel_aspect_resource(&v1));
                for (bytes, aspect) in [(&encoded, TEST_PIXEL_ASPECT), (&standard, v1)] {
                    for loaded in [
                        decode(bytes).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap(),
                    ] {
                        assert!(!loaded.read_only, "{:?}", loaded.warnings);
                        assert_eq!(loaded.psd_pixel_aspect, Some(aspect));
                        assert_eq!(loaded.layers[0].pixels, doc.layers[0].pixels);
                    }
                }
                let mut stretched = TEST_PIXEL_ASPECT;
                stretched[4..].copy_from_slice(&1.25f64.to_be_bytes());
                let mut invalid = TEST_PIXEL_ASPECT;
                invalid[4..].copy_from_slice(&f64::NAN.to_be_bytes());
                for (bytes, reason) in [
                    (replace_resources(&pixel_aspect_resource(&stretched)), "non-square pixel aspect"),
                    (replace_resources(&pixel_aspect_resource(&invalid)), "Invalid Photoshop pixel aspect"),
                    (replace_resources(&[resources, &pixel_aspect_resource(&v1)].concat()), "Duplicate Photoshop pixel aspect"),
                ] {
                    for loaded in [
                        decode(&bytes).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
                    ] {
                        assert!(loaded.read_only);
                        assert!(loaded.psd_pixel_aspect.is_none());
                        assert!(loaded.warnings.iter().any(|warning| warning.contains(reason)));
                        assert_eq!(loaded.bit_depth, depth);
                        assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                    }
                }
            }
        }
    }

    #[test]
    fn print_resolution_round_trips_with_exact_density_and_units() {
        let resolution = [1, 44, 128, 0, 0, 1, 0, 1, 1, 145, 64, 0, 0, 2, 0, 2];
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.psd_resolution = Some(resolution);
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&bytes);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let resource_length = cursor.pos;
                cursor.block().unwrap();
                let standard = |resources: &[u8]| {
                    let mut output = bytes[..resource_length].to_vec();
                    block(&mut output, resources);
                    output.extend_from_slice(&bytes[cursor.pos..]);
                    output
                };
                let resource = resolution_resource(&resolution);
                for encoded in [&bytes, &standard(&resource)] {
                    for loaded in [
                        decode(encoded).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(encoded), &Default::default()).unwrap(),
                    ] {
                        assert!(!loaded.read_only, "{:?}", loaded.warnings);
                        assert_eq!(loaded.psd_resolution, Some(resolution));
                        assert_eq!((loaded.width, loaded.height, loaded.bit_depth), (2, 1, depth));
                        assert_eq!(loaded.layers[0].pixels, doc.layers[0].pixels);
                    }
                }
                let index = structural::index(
                    &mut std::io::Cursor::new(&bytes), structural::Limits::current(), || Ok(()),
                ).unwrap();
                let position = index.resources.iter().find(|r| r.id == RESOLUTION_RESOURCE_ID).unwrap().data.offset as usize;
                let mut changed = bytes.clone();
                changed[position + 2] ^= 1;
                let mut invalid = resource;
                invalid[17] = 3;
                let duplicate = standard(&resource.repeat(16));
                let compact = structural::index_for_decode(
                    &mut std::io::Cursor::new(&duplicate), || Ok(()),
                ).unwrap();
                assert_eq!(compact.resources.iter().filter(|r| r.id == RESOLUTION_RESOURCE_ID).count(), 2);
                for (encoded, reason) in [
                    (changed, None),
                    (duplicate, Some("Duplicate Photoshop print resolution")),
                    (standard(&invalid), Some("Invalid Photoshop print resolution")),
                ] {
                    for loaded in [
                        decode(&encoded).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&encoded), &Default::default()).unwrap(),
                    ] {
                        assert!(loaded.read_only, "{:?}", loaded.warnings);
                        if let Some(reason) = reason {
                            assert!(loaded.psd_resolution.is_none());
                            assert!(loaded.warnings.iter().any(|warning| warning.contains(reason)),
                                "{:?}", loaded.warnings);
                        }
                    }
                }
            }
        }
        assert!(parse_resolution(&resolution[..15]).is_err());
    }

    #[test]
    fn print_scale_round_trips_and_protects_ambiguous_placement() {
        let resource = print_scale_resource(&TEST_PRINT_SCALE);
        let mut invalid = resource;
        invalid[14..18].copy_from_slice(&f32::NAN.to_be_bytes());
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.psd_print_scale = Some(TEST_PRINT_SCALE);
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&bytes);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let length = cursor.pos;
                cursor.block().unwrap();
                let standard = |resources: &[u8]| {
                    let mut output = bytes[..length].to_vec();
                    block(&mut output, resources);
                    output.extend_from_slice(&bytes[cursor.pos..]);
                    output
                };
                let index = structural::index_for_decode(&mut std::io::Cursor::new(&bytes), || Ok(())).unwrap();
                let scale = index.resources.iter().find(|r| r.id == PRINT_SCALE_RESOURCE_ID).unwrap();
                let mut changed = bytes.clone();
                changed[scale.data.offset as usize + 13] = 1;
                let mut changed_scale = TEST_PRINT_SCALE;
                changed_scale[13] = 1;
                for (encoded, expected, protected) in [
                    (bytes.clone(), Some(TEST_PRINT_SCALE), false),
                    (standard(&resource), Some(TEST_PRINT_SCALE), false),
                    (changed, Some(changed_scale), true),
                    (standard(&resource.repeat(16)), None, true),
                    (standard(&invalid), None, true),
                ] {
                    for loaded in [
                        decode(&encoded).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&encoded), &Default::default()).unwrap(),
                    ] {
                        assert_eq!(loaded.read_only, protected, "{:?}", loaded.warnings);
                        assert_eq!(loaded.psd_print_scale, expected);
                        assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                    }
                }
            }
        }
    }

    #[test]
    fn print_flags_round_trip_and_reject_changed_or_malformed_resources() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.psd_print_flags = Some(TEST_PRINT_FLAGS);
            doc.psd_print_flags_info = Some(TEST_PRINT_FLAGS_INFO);
            let flags = print_flags_resource(&TEST_PRINT_FLAGS);
            let info = print_flags_info_resource(&TEST_PRINT_FLAGS_INFO);
            for layout in [Layout::Psd, Layout::Psb] {
                let encoded = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&encoded);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let replace_resources = |resources: &[u8]| {
                    let mut result = encoded[..start].to_vec();
                    block(&mut result, resources);
                    result.extend_from_slice(&encoded[cursor.pos..]);
                    result
                };
                for bytes in [encoded.clone(), replace_resources(&[flags, info].concat())] {
                    for loaded in [
                        decode(&bytes).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
                    ] {
                        assert!(!loaded.read_only, "{:?}", loaded.warnings);
                        assert_eq!(loaded.psd_print_flags, Some(TEST_PRINT_FLAGS));
                        assert_eq!(loaded.psd_print_flags_info, Some(TEST_PRINT_FLAGS_INFO));
                        assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                        let layer = loaded.layers[0].id.clone();
                        let edited = crate::engine::Engine::preview_edits(loaded, &[
                            serde_json::json!({"op":"layer.duplicate","layers":[layer]}),
                        ]).unwrap();
                        let saved = decode(&encode_layout(&edited, layout).unwrap()).unwrap();
                        assert_eq!(saved.psd_print_flags, Some(TEST_PRINT_FLAGS));
                        assert_eq!(saved.psd_print_flags_info, Some(TEST_PRINT_FLAGS_INFO));
                    }
                }
                let index = structural::index_for_decode(&mut std::io::Cursor::new(&encoded), || Ok(())).unwrap();
                for (id, record, invalid_at) in [
                    (PRINT_FLAGS_RESOURCE_ID, flags, 12),
                    (PRINT_FLAGS_INFO_RESOURCE_ID, info, 15),
                ] {
                    let resource = index.resources.iter().find(|resource| resource.id == id).unwrap();
                    let mut changed = encoded.clone();
                    changed[resource.data.offset as usize + 2] ^= 1;
                    let mut invalid = record;
                    invalid[invalid_at] = 2;
                    for bytes in [changed, replace_resources(&record.repeat(2)), replace_resources(&invalid)] {
                        for loaded in [
                            decode(&bytes).unwrap(),
                            decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
                        ] {
                            assert!(loaded.read_only, "{:?}", loaded.warnings);
                            assert!(!loaded.warnings.is_empty());
                            assert_eq!(loaded.bit_depth, depth);
                            assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn photoshop_file_info_survives_standard_and_embedded_round_trips() {
        let file_info = std::sync::Arc::new(file_info::fixture());
        for depth in [8, 16] {
            let mut doc = Document::new_depth(3, 2, depth).unwrap();
            doc.layers[0].pixels.set16(1, 1, [12347, 23459, 34571, 65535]);
            doc.psd_file_info = Some(file_info.clone());
            for layout in [Layout::Psd, Layout::Psb] {
                let encoded = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&encoded);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let standard = |resources: &[u8]| {
                    let mut output = encoded[..start].to_vec();
                    block(&mut output, resources);
                    output.extend_from_slice(&encoded[cursor.pos..]);
                    output
                };
                for input in [&encoded, &standard(&file_info)] {
                    let loaded = decode_reader(&mut std::io::Cursor::new(input), &Default::default()).unwrap();
                    assert!(!loaded.read_only, "{:?}", loaded.warnings);
                    assert_eq!(loaded.psd_file_info.as_ref(), Some(&file_info));
                    assert_eq!(loaded.layers[0].pixels.get16(1, 1), doc.layers[0].pixels.get16(1, 1));
                    let id = loaded.layers[0].id.clone();
                    let edited = crate::engine::Engine::preview_edits(loaded, &[
                        serde_json::json!({"op":"layer.update","layer":id,"opacity":0.5}),
                    ]).unwrap();
                    let saved = encode_layout(&edited, layout).unwrap();
                    assert_eq!(decode(&saved).unwrap().psd_file_info.as_ref(), Some(&file_info));
                }
                let index = structural::index_for_decode(&mut std::io::Cursor::new(&encoded), || Ok(())).unwrap();
                for id in [1001, 1008, 1009, 1010, 1012, 1013, 1015, 1016, 1019, 1030, 1035, 1038, 1040, 1051, 1054, 1071, 1073, 1074, 1082, 1083, 1084, 1085] {
                    assert_eq!(index.resources.iter().filter(|record| record.id == id).count(), 1);
                }
                let mut changed = encoded.clone();
                let xmp = index.resources.iter().find(|record| record.id == 1060).unwrap();
                changed[xmp.data.offset as usize + 1] ^= 1;
                assert!(decode(&changed).unwrap().read_only);
                for id in [1019, 1040, 1085] {
                    let resource = index.resources.iter().find(|record| record.id == id).unwrap();
                    let mut changed = encoded.clone();
                    changed[resource.data.offset as usize] ^= 1;
                    assert!(decode(&changed).unwrap().read_only);
                }
                let downgraded = adjustments::tests::replace_sources(&encoded, layout, &doc, LEGACY_PRINT_SETUP_FORMAT);
                let mut source = Cursor::new(&downgraded[start..]);
                let resources = [source.block().unwrap(), file_info.as_slice()].concat();
                assert!(decode(&standard(&resources)).unwrap().read_only);
                assert!(decode(&standard(&file_info.repeat(2))).unwrap().read_only);
                let wrong_dot_range = standard(&fixed_resource::<14>(1019, &[12]));
                for loaded in [
                    decode(&wrong_dot_range).unwrap(),
                    decode_reader(&mut std::io::Cursor::new(&wrong_dot_range), &Default::default()).unwrap(),
                ] {
                    assert!(loaded.read_only);
                    assert!(loaded.warnings.iter().any(|warning| warning.contains("resource 1019")));
                    assert_eq!(loaded.layers[0].pixels.get16(1, 1), doc.layers[0].pixels.get16(1, 1));
                }
                let mut malformed = file_info.as_ref().clone();
                let input = standard(&file_info);
                let caption_index = structural::index_for_decode(&mut std::io::Cursor::new(&input), || Ok(())).unwrap();
                for (id, offset) in [(1012, 10), (1016, 26)] {
                    let resource = caption_index.resources.iter().find(|record| record.id == id).unwrap();
                    let mut malformed = file_info.as_ref().clone();
                    malformed[resource.data.offset as usize - start + offset] = 0xff;
                    let input = standard(&malformed);
                    let loaded = decode_reader(&mut std::io::Cursor::new(&input), &Default::default()).unwrap();
                    assert!(loaded.read_only);
                    assert!(loaded.warnings.iter().any(|warning| warning.contains(&format!("resource {id}"))));
                }
                let jpeg_quality = caption_index.resources.iter().find(|record| record.id == 1030).unwrap();
                let mut invalid_quality = file_info.as_ref().clone();
                invalid_quality[jpeg_quality.data.offset as usize - start + 5] = 0;
                let invalid_quality = standard(&invalid_quality);
                for loaded in [
                    decode(&invalid_quality).unwrap(),
                    decode_reader(&mut std::io::Cursor::new(&invalid_quality), &Default::default()).unwrap(),
                ] {
                    assert!(loaded.read_only);
                    assert!(loaded.warnings.iter().any(|warning| warning.contains("resource 1030")));
                    assert_eq!(loaded.layers[0].pixels.get16(1, 1), doc.layers[0].pixels.get16(1, 1));
                }
                let caption = caption_index.resources.iter().find(|record| record.id == 1008).unwrap();
                let length_position = caption.data.offset as usize - start - 4;
                malformed[length_position] += 1;
                assert!(decode(&standard(&malformed)).unwrap().read_only);
                let border = caption_index.resources.iter().find(|record| record.id == 1009).unwrap();
                let mut malformed = file_info.as_ref().clone();
                malformed[border.data.offset as usize - start + 5] = 6;
                assert!(decode(&standard(&malformed)).unwrap().read_only);
                let urls = caption_index.resources.iter().find(|record| record.id == 1054).unwrap();
                let mut changed = file_info.as_ref().clone();
                changed[urls.data.offset as usize - start + 3] = 1;
                let input = standard(&changed);
                for loaded in [
                    decode(&input).unwrap(),
                    decode_reader(&mut std::io::Cursor::new(&input), &Default::default()).unwrap(),
                ] {
                    assert!(loaded.read_only);
                    assert!(loaded.warnings.iter().any(|warning| warning.contains("URL List")));
                    assert_eq!(loaded.bit_depth, depth);
                }
                for id in [1074, 1082] {
                    let descriptor = caption_index.resources.iter().find(|record| record.id == id).unwrap();
                    let mut malformed = file_info.as_ref().clone();
                    malformed[descriptor.data.offset as usize - start + 3] = 15;
                    assert!(decode(&standard(&malformed)).unwrap().read_only);
                }
                for id in [1038, 1073] {
                    let sampler = caption_index.resources.iter().find(|record| record.id == id).unwrap();
                    let mut changed = file_info.as_ref().clone();
                    changed[sampler.data.offset as usize - start + 7] = 1;
                    let input = standard(&changed);
                    for loaded in [
                        decode(&input).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&input), &Default::default()).unwrap(),
                    ] {
                        assert!(loaded.read_only);
                        assert!(loaded.warnings.iter().any(|warning| warning.contains("color sampler")));
                        assert_eq!(loaded.bit_depth, depth);
                        assert_eq!(loaded.layers[0].pixels.get16(1, 1), doc.layers[0].pixels.get16(1, 1));
                    }
                }
            }
        }
    }

    #[test]
    fn copyright_and_global_lighting_resources_bind_to_the_editable_source() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            let flag = [u8::from(depth == 16)];
            doc.psd_copyright = Some(flag);
            let resource = copyright_resource(&flag);
            let angle = if depth == 16 { 359u32 } else { 0 }.to_be_bytes();
            doc.psd_global_angle = Some(angle);
            let light = global_angle_resource(&angle);
            let altitude = if depth == 16 { 45u32 } else { 0 }.to_be_bytes();
            doc.psd_global_altitude = Some(altitude);
            let elevation = global_altitude_resource(&altitude);
            for layout in [Layout::Psd, Layout::Psb] {
                let encoded = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&encoded);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let standard = |resources: &[u8]| {
                    let mut result = encoded[..start].to_vec();
                    block(&mut result, resources);
                    result.extend_from_slice(&encoded[cursor.pos..]);
                    result
                };
                let read = |bytes: &[u8]| [
                    decode(bytes).unwrap(),
                    decode_reader(&mut std::io::Cursor::new(bytes), &Default::default()).unwrap(),
                ];
                for bytes in [&encoded, &standard(&[resource.as_slice(), &light, &elevation].concat())] {
                    for loaded in read(bytes) {
                        assert!(!loaded.read_only, "{:?}", loaded.warnings);
                        assert_eq!(loaded.psd_copyright, Some(flag));
                        assert_eq!(loaded.psd_global_angle, Some(angle));
                        assert_eq!(loaded.psd_global_altitude, Some(altitude));
                        assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                        let layer = loaded.layers[0].id.clone();
                        let edited = crate::engine::Engine::preview_edits(loaded, &[
                            serde_json::json!({"op":"layer.update","layer":layer,"opacity":0.5}),
                        ]).unwrap();
                        let reopened = decode(&encode_layout(&edited, layout).unwrap()).unwrap();
                        assert!(!reopened.read_only);
                        assert_eq!(reopened.psd_copyright, Some(flag));
                        assert_eq!(reopened.psd_global_angle, Some(angle));
                        assert_eq!(reopened.psd_global_altitude, Some(altitude));
                    }
                }
                assert!(read(&standard(&[])).iter().all(|doc| {
                    doc.psd_copyright.is_none() && doc.psd_global_angle.is_none()
                        && doc.psd_global_altitude.is_none()
                }));
                let index = structural::index_for_decode(&mut std::io::Cursor::new(&encoded), || Ok(())).unwrap();
                let offset = index.resources.iter().find(|r| r.id == COPYRIGHT_RESOURCE_ID).unwrap().data.offset;
                let mut changed = encoded.clone();
                changed[offset as usize] ^= 1;
                let wrong_length = fixed_resource::<14>(COPYRIGHT_RESOURCE_ID, &[0, 0]);
                let mut changed_light = encoded.clone();
                let at = index.resources.iter().find(|r| r.id == GLOBAL_ANGLE_RESOURCE_ID).unwrap().data.offset as usize;
                changed_light[at + 3] ^= 1;
                let mut changed_elevation = encoded.clone();
                let at = index.resources.iter().find(|r| r.id == GLOBAL_ALTITUDE_RESOURCE_ID).unwrap().data.offset as usize;
                changed_elevation[at + 3] ^= 1;
                for bytes in [
                    changed, standard(&resource.repeat(2)), standard(&copyright_resource(&[2])), standard(&wrong_length),
                    changed_light, standard(&light.repeat(2)), standard(&global_angle_resource(&360u32.to_be_bytes())),
                    standard(&fixed_resource::<16>(GLOBAL_ANGLE_RESOURCE_ID, &[0; 3])),
                    changed_elevation, standard(&elevation.repeat(2)),
                    standard(&fixed_resource::<16>(GLOBAL_ALTITUDE_RESOURCE_ID, &[0; 3])),
                ] {
                    for loaded in read(&bytes) {
                        assert!(loaded.read_only, "{:?}", loaded.warnings);
                        assert_eq!(loaded.bit_depth, depth);
                        assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                    }
                }
            }
        }
    }

    #[test]
    fn grid_headers_round_trip_and_bind_the_supplementary_document() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.psd_grid = Some(TEST_GRID);
            for layout in [Layout::Psd, Layout::Psb] {
                let encoded = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&encoded);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let replace_resources = |resources: &[u8]| {
                    let mut result = encoded[..start].to_vec();
                    block(&mut result, resources);
                    result.extend_from_slice(&encoded[cursor.pos..]);
                    result
                };
                let grid = grid_resource(&TEST_GRID);
                let standard = replace_resources(&grid);
                let index = structural::index_for_decode(&mut std::io::Cursor::new(&encoded), || Ok(())).unwrap();
                let resource = index.resources.iter().find(|resource| resource.id == GRID_RESOURCE_ID).unwrap();
                let mut changed_grid = TEST_GRID;
                changed_grid[7] += 1;
                let mut changed = encoded.clone();
                changed[resource.data.offset as usize + 7] = changed_grid[7];
                let mut invalid = TEST_GRID;
                invalid[3] = 2;
                for (bytes, grid, read_only) in [
                    (encoded.clone(), Some(TEST_GRID), false),
                    (standard, Some(TEST_GRID), false),
                    (changed, Some(changed_grid), true),
                    (replace_resources(&grid.repeat(2)), None, true),
                    (replace_resources(&grid_resource(&invalid)), None, true),
                ] {
                    for loaded in [
                        decode(&bytes).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
                    ] {
                        assert_eq!(loaded.read_only, read_only, "{:?}", loaded.warnings);
                        assert_eq!(loaded.psd_grid, grid);
                        assert_eq!(loaded.bit_depth, depth);
                        assert_eq!(loaded.layers[0].pixels.get16(0, 0), doc.layers[0].pixels.get16(0, 0));
                    }
                }
            }
        }
    }

    #[test]
    fn unsupported_document_resources_protect_standard_and_supplementary_psd_sources() {
        let resource = |id: u16, payload: &[u8]| {
            let mut output = b"8BIM".to_vec();
            u16b(&mut output, id);
            output.extend_from_slice(&[0, 0]);
            block(&mut output, payload);
            if payload.len() % 2 != 0 {
                output.push(0);
            }
            output
        };
        let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
        compressor.write_all(b"{\"chunks\":[]}").unwrap();
        let native = resource(4000, &[b"PBR1".as_slice(), &compressor.finish().unwrap()].concat());
        let mut guides = TEST_GRID.to_vec();
        guides[15] = 1;
        guides.extend_from_slice(&32u32.to_be_bytes());
        guides.push(0);
        let guided = resource(GRID_RESOURCE_ID, &guides);
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            let sample = [12347, 23459, 34571, 65535];
            doc.layers[0].pixels.set16(0, 0, sample);
            let expected = doc.layers[0].pixels.get16(0, 0);
            for layout in [Layout::Psd, Layout::Psb] {
                let original = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&original);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let resource_position = cursor.pos;
                let embedded = cursor.block().unwrap();
                let replace_resources = |resources: &[u8]| {
                    let mut output = original[..resource_position].to_vec();
                    block(&mut output, resources);
                    output.extend_from_slice(&original[cursor.pos..]);
                    output
                };
                assert!(native_manifest_candidate(&mut std::io::Cursor::new(replace_resources(&native)), || Ok(())).unwrap());
                let visible = resource(EFFECTS_VISIBILITY_RESOURCE_ID, &[1]);
                let native_visible = replace_resources(&[native.as_slice(), &visible].concat());
                assert!(native_manifest_candidate(&mut std::io::Cursor::new(native_visible), || Ok(())).unwrap());
                let native_grid = replace_resources(&[native.as_slice(), &grid_resource(&TEST_GRID)].concat());
                assert!(native_manifest_candidate(&mut std::io::Cursor::new(native_grid), || Ok(())).unwrap());
                let loaded = decode(&replace_resources(&[embedded, &visible].concat())).unwrap();
                assert!(!loaded.read_only, "{:?}", loaded.warnings);
                assert_eq!(loaded.layers[0].pixels.get16(0, 0), expected);

                let protected = [
                    1014, 1017, 1018, 1021, 1025, 1043, 1050, 1065, 1066, 1067, 1070, 1075,
                    1080, 1088, 2000, 2997, 2999, 3000,
                    7000, 7001, 7002, 7003, 7004, 7005, 8000,
                ].into_iter()
                    .map(|id| (resource(id, &[0; 26]), unsupported_document_resource(id).unwrap()))
                    .chain([
                        (resource(1022, &[0, 1, 0]), "Photoshop Quick Mask"),
                        (resource(1076, &16u32.to_be_bytes()), "Photoshop animation sheet disclosure"),
                        (resource(1078, &16u32.to_be_bytes()), "Photoshop Onion Skins"),
                        (resource(EFFECTS_VISIBILITY_RESOURCE_ID, &[0]), "document-wide hidden effects"),
                        (resource(EFFECTS_VISIBILITY_RESOURCE_ID, &[1, 1]), "Invalid Photoshop global effects visibility"),
                        (visible.repeat(2), "Duplicate Photoshop global effects visibility"),
                        (guided.clone(), GUIDE_WARNING),
                    ]);
                for (protected, reason) in protected {
                    let native_with_protection = replace_resources(&[native.as_slice(), &protected].concat());
                    assert!(!native_manifest_candidate(&mut std::io::Cursor::new(native_with_protection), || Ok(())).unwrap());
                    for records in [protected.clone(), [embedded, &protected].concat()] {
                        let encoded = replace_resources(&records);
                        for loaded in [
                            decode(&encoded).unwrap(),
                            decode_reader(&mut std::io::Cursor::new(&encoded), &Default::default()).unwrap(),
                        ] {
                            assert!(loaded.read_only);
                            assert!(loaded.warnings.iter().any(|warning| warning.contains(reason)), "{:?}", loaded.warnings);
                            assert_eq!(loaded.bit_depth, depth);
                            assert_eq!(loaded.layers[0].pixels.get16(0, 0), expected);
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn nested_folder_dividers_keep_their_own_ids_and_closed_state() {
        use crate::engine::Engine;
        use serde_json::json;

        let metadata = |key: &str, value: u32| LayerMetadata {
            key: key.into(),
            data: value.to_be_bytes().to_vec(),
        };
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            let mut outer = Layer::new("Outer", "group", 2, 1);
            let mut inner = Layer::new("Inner", "group", 2, 1);
            outer.psd_metadata = vec![metadata("lyid", 21), metadata(CLOSED_FOLDER, 2), metadata(CLOSING_LAYER_ID, 22)];
            outer.psd_metadata.push(metadata("lyvr:closing", 70));
            inner.psd_metadata = vec![metadata("lyid", 31), metadata(CLOSING_LAYER_ID, 32)];
            inner.parent = Some(outer.id.clone());
            doc.layers[0].parent = Some(inner.id.clone());
            doc.layers[0].psd_metadata.push(metadata("lyid", 17));
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.layers.splice(0..0, [outer, inner]);
            doc.ensure_depth();
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&bytes);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let resource_length = cursor.pos;
                cursor.block().unwrap();
                let mut standard = bytes[..resource_length].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[cursor.pos..]);
                let index = structural::index(
                    &mut std::io::Cursor::new(&standard), structural::Limits::current(), || Ok(()),
                ).unwrap();
                assert_eq!(index.layers.len(), 5);
                for (record, expected) in index.layers.iter().zip([22u32, 32, 17, 31, 21]) {
                    let mut tags = record.tags.iter().filter(|tag| tag.key == *b"lyid");
                    let at = tags.next().unwrap().data.offset as usize;
                    assert!(tags.next().is_none());
                    assert_eq!(&standard[at..at + 4], &expected.to_be_bytes());
                }
                let sections = index.layers.iter().filter_map(|record| {
                    let divider = record.tags.iter().find(|tag| tag.key == *b"lsct")?;
                    let at = divider.data.offset as usize;
                    Some(u32::from_be_bytes(standard[at..at + 4].try_into().unwrap()))
                }).collect::<Vec<_>>();
                assert_eq!(sections, [3, 3, 1, 2]);
                let closing_version = index.layers[0].tags.iter().find(|tag| tag.key == *b"lyvr").unwrap();
                let at = closing_version.data.offset as usize;
                assert_eq!(&standard[at..at + 4], &70u32.to_be_bytes());
                let outer_divider = index.layers[4].tags.iter().find(|tag| tag.key == *b"lsct").unwrap();
                let at = outer_divider.data.offset as usize;
                let mut nested = standard.clone();
                nested[at - 8..at - 4].copy_from_slice(b"lsdk");
                let rebound = adjustments::tests::replace_sources(&nested, layout, &doc, CLOSING_METADATA_FORMAT);
                for encoded in [&bytes, &standard, &nested, &rebound] {
                    let reopened = decode(encoded).unwrap();
                    assert!(!reopened.read_only, "{:?}", reopened.warnings);
                    assert_eq!(reopened.layers.len(), 3);
                    for (actual, expected) in reopened.layers.iter().zip(&doc.layers) {
                        assert_eq!(actual.psd_metadata, expected.psd_metadata);
                    }
                    assert_eq!(reopened.layers[1].parent.as_ref(), Some(&reopened.layers[0].id));
                    assert_eq!(reopened.layers[2].parent.as_ref(), Some(&reopened.layers[1].id));
                    assert_eq!(reopened.layers[2].pixels, doc.layers[2].pixels);
                    let duplicate = Engine::preview_edits(reopened.clone(), &[
                        json!({"op":"layer.duplicate","layers":[reopened.layers[0].id]}),
                    ]).unwrap();
                    assert_eq!(duplicate.layers.len(), 6);
                    let copies = duplicate.layers.iter().filter(|layer| !reopened.layers.iter().any(|old| old.id == layer.id));
                    assert_eq!(copies.flat_map(|layer| &layer.psd_metadata).collect::<Vec<_>>(), [
                        &metadata(CLOSED_FOLDER, 2), &metadata("lyvr:closing", 70),
                    ]);
                }
                assert_eq!(decode(&rebound).unwrap().layers[0].id, doc.layers[0].id);
                let mut mismatched = doc.clone();
                mismatched.layers[0].blend = "multiply".into();
                let rebound = adjustments::tests::replace_sources(&nested, layout, &mismatched, CLOSING_METADATA_FORMAT);
                assert!(decode(&rebound).unwrap().read_only);
                let mut invalid = nested;
                invalid[at..at + 4].copy_from_slice(&4u32.to_be_bytes());
                let reopened = decode(&invalid).unwrap();
                assert!(reopened.read_only);
                assert_eq!(reopened.bit_depth, depth);
                assert_eq!(reopened.layers[0].pixels.get16(0, 0), doc.layers[2].pixels.get16(0, 0));
                assert!(reopened.warnings.iter().any(|warning| warning.contains("section divider type")));
                let mut forged = doc.clone();
                forged.layers[0].psd_metadata.remove(1);
                let rebound = adjustments::tests::replace_sources(&bytes, layout, &forged, CLOSING_METADATA_FORMAT);
                assert!(decode(&rebound).unwrap().read_only);
                forged.layers[0].psd_metadata[1] = metadata(CLOSING_LAYER_ID, 23);
                let rebound = adjustments::tests::replace_sources(&bytes, layout, &forged, CLOSING_METADATA_FORMAT);
                assert!(decode(&rebound).unwrap().read_only);
                forged.layers[0].psd_metadata[1] = metadata(CLOSING_LAYER_ID, 31);
                assert!(encode_layout(&forged, layout).is_err());
            }
        }
        assert!(validate_layer_metadata(&[metadata(CLOSING_LAYER_ID, 9)], false, false).is_err());
    }

    #[test]
    fn supplementary_sources_cannot_hide_unhandled_standard_layer_tags() {
        for (depth, layout) in [(8, Layout::Psd), (16, Layout::Psb)] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            let sample = doc.layers[0].pixels.get16(0, 0);
            doc.layers[0].psd_metadata.push(LayerMetadata { key: "lyid".into(), data: vec![1, 0, 0, 0] });
            let bytes = encode_layout(&doc, layout).unwrap();
            let index = structural::index_for_decode(&mut std::io::Cursor::new(&bytes), || Ok(())).unwrap();
            let tag = index.layers[0].tags.iter().find(|tag| tag.key == *b"lyid").unwrap();
            let at = tag.data.offset as usize - 8;
            doc.layers[0].psd_metadata.clear();
            for (key, reason) in [(b"TySh", "editable Photoshop text"), (b"knko", "knockout blending")] {
                let mut changed = bytes.clone();
                changed[at..at + 4].copy_from_slice(key);
                let changed = adjustments::tests::replace_sources(&changed, layout, &doc, CLOSING_METADATA_FORMAT);
                for loaded in [decode(&changed), decode_reader(&mut std::io::Cursor::new(&changed), &Default::default())] {
                    let loaded = loaded.unwrap();
                    assert!(loaded.read_only);
                    assert!(loaded.warnings.iter().any(|warning| warning.contains(reason)));
                    assert_eq!(loaded.bit_depth, depth);
                    assert_eq!(loaded.layers[0].pixels.get16(0, 0), sample);
                }
            }
        }
    }

    #[test]
    fn photoshop_layer_ids_survive_standard_and_private_round_trips_without_copying() {
        use crate::engine::Engine;
        use serde_json::json;

        let layer_id = |id: u32| LayerMetadata { key: "lyid".into(), data: id.to_be_bytes().to_vec() };
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers.push(Layer::new("Second", "paint", 2, 1));
            doc.ensure_depth();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.layers[0].psd_metadata.push(layer_id(17));
            doc.layers[1].psd_metadata.push(layer_id(41));
            doc.psd_layer_id_seed = Some(TEST_LAYER_ID_SEED);
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&bytes);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let resource_length = cursor.pos;
                cursor.block().unwrap();
                let replace_resources = |resources: &[u8]| {
                    let mut result = bytes[..resource_length].to_vec();
                    block(&mut result, resources);
                    result.extend_from_slice(&bytes[cursor.pos..]);
                    result
                };
                let seed = layer_id_seed_resource(&TEST_LAYER_ID_SEED);
                let mut standard = replace_resources(&seed);
                let index = structural::index(
                    &mut std::io::Cursor::new(&standard), structural::Limits::current(), || Ok(()),
                ).unwrap();
                let offsets: Vec<_> = index.layers.iter().map(|record| {
                    let mut ids = record.tags.iter().filter(|tag| tag.key == *b"lyid");
                    let at = ids.next().unwrap().data.offset as usize;
                    assert!(ids.next().is_none());
                    at
                }).collect();
                assert_eq!(&standard[offsets[0]..offsets[0] + 4], &41u32.to_be_bytes());
                assert_eq!(&standard[offsets[1]..offsets[1] + 4], &17u32.to_be_bytes());
                for encoded in [&bytes, &standard] {
                    let reopened = decode(encoded).unwrap();
                    assert!(!reopened.read_only, "{:?}", reopened.warnings);
                    assert_eq!(reopened.psd_layer_id_seed, Some(TEST_LAYER_ID_SEED));
                    for (actual, expected) in reopened.layers.iter().zip(&doc.layers) {
                        assert_eq!(actual.psd_metadata, expected.psd_metadata);
                        assert_eq!(actual.pixels, expected.pixels);
                    }
                    let original = &reopened.layers[0];
                    let copied = Engine::preview_edits(reopened.clone(), &[
                        json!({"op":"layer.duplicate","layers":[original.id]}),
                    ]).unwrap();
                    assert_eq!(copied.layers.len(), 3);
                    assert_eq!(copied.psd_layer_id_seed, Some(TEST_LAYER_ID_SEED));
                    assert_eq!(copied.layers.iter().filter(|layer| layer.psd_metadata.contains(&layer_id(17))).count(), 1);
                    assert!(copied.layers.iter().find(|layer| layer.id != original.id
                        && layer.id != reopened.layers[1].id).unwrap().psd_metadata.is_empty());
                }
                let saved = structural::index_for_decode(&mut std::io::Cursor::new(&bytes), || Ok(())).unwrap();
                let saved_seed = saved.resources.iter().find(|r| r.id == LAYER_ID_SEED_RESOURCE_ID).unwrap();
                let seed_offset = saved_seed.data.offset as usize;
                assert_eq!(&bytes[seed_offset..seed_offset + 4], &TEST_LAYER_ID_SEED);
                let mut changed = bytes.clone();
                changed[seed_offset + 3] ^= 1;
                let mut short = seed;
                short[11] = 3;
                short[15] = 0;
                for (encoded, reason) in [
                    (changed, "definitions were changed"),
                    (replace_resources(&seed.repeat(2)), "Duplicate Photoshop layer ID seed"),
                    (replace_resources(&short), "Invalid Photoshop layer ID seed length"),
                ] {
                    for loaded in [
                        decode(&encoded).unwrap(),
                        decode_reader(&mut std::io::Cursor::new(&encoded), &Default::default()).unwrap(),
                    ] {
                        assert!(loaded.read_only);
                        assert!(loaded.warnings.iter().any(|warning| warning.contains(reason)), "{:?}", loaded.warnings);
                        assert_eq!(loaded.bit_depth, depth);
                    }
                }
                let mut forged = doc.clone();
                forged.layers[0].psd_metadata[0] = layer_id(19);
                let rebound = adjustments::tests::replace_sources(&bytes, layout, &forged, adjustments::LOOK_LOOKUP_FORMAT);
                assert!(decode(&rebound).unwrap().read_only);
                standard[offsets[0]..offsets[0] + 4].copy_from_slice(&17u32.to_be_bytes());
                let protected = decode(&standard).unwrap();
                assert!(protected.read_only && protected.warnings.iter().any(|warning| warning.contains("Duplicate Photoshop layer ID")));
            }
        }
        assert!(validate_metadata(&[LayerMetadata { key: "lyid".into(), data: vec![0; 3] }]).is_err());
    }

    #[test]
    fn photoshop_lock_all_round_trips_and_cannot_be_overridden_by_private_sources() {
        use crate::engine::Engine;
        use serde_json::json;

        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.layers[0].locked = true;
            doc.layers[0].psd_metadata.push(LayerMetadata {
                key: "lspf".into(),
                data: COMPLETE_LAYER_LOCK.to_vec(),
            });
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut cursor = Cursor::new(&bytes);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let resource_length = cursor.pos;
                cursor.block().unwrap();
                let mut standard = bytes[..resource_length].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[cursor.pos..]);
                let index = structural::index(
                    &mut std::io::Cursor::new(&standard), structural::Limits::current(), || Ok(()),
                ).unwrap();
                let mut locks = index.layers[0].tags.iter().filter(|tag| tag.key == *b"lspf");
                let at = locks.next().unwrap().data.offset as usize;
                assert!(locks.next().is_none());
                assert_eq!(&standard[at..at + 4], &COMPLETE_LAYER_LOCK);
                for encoded in [&bytes, &standard] {
                    let reopened = decode(encoded).unwrap();
                    assert!(!reopened.read_only, "{:?}", reopened.warnings);
                    assert!(reopened.layers[0].locked);
                    assert_eq!(reopened.layers[0].psd_metadata, doc.layers[0].psd_metadata);
                    assert_eq!(reopened.layers[0].pixels, doc.layers[0].pixels);
                    let target = reopened.layers[0].id.clone();
                    assert!(Engine::preview_edits(reopened.clone(), &[json!({
                        "op":"layer.update", "layer":target, "name":"Refused",
                    })]).is_err());
                    let unlocked = Engine::preview_edits(reopened, &[json!({
                        "op":"layer.update", "layer":target, "locked":false,
                    })]).unwrap();
                    assert!(!unlocked.layers[0].locked);
                    assert!(unlocked.layers[0].psd_metadata.is_empty());
                }
                let mut forged = doc.clone();
                forged.layers[0].locked = false;
                forged.layers[0].psd_metadata.clear();
                let rebound = adjustments::tests::replace_sources(&bytes, layout, &forged, adjustments::LOOK_LOOKUP_FORMAT);
                assert!(decode(&rebound).unwrap().read_only);
                standard[at..at + 4].copy_from_slice(&4u32.to_be_bytes());
                assert!(decode(&standard).unwrap().read_only);
            }
        }
    }

    #[test]
    fn modern_color_labels_reopen_from_standard_psd_and_psb_records() {
        use crate::engine::Engine;
        use serde_json::json;

        for depth in [8, 16] {
            let mut original = Document::new_depth(2, 1, depth).unwrap();
            original.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            let target = original.layers[0].id.clone();
            let pixels = original.layers[0].pixels.clone();
            let composite = original.export_png().unwrap();
            for (index, label) in [(9, "indigo"), (10, "magenta"), (11, "fuchsia")] {
                let doc = Engine::preview_edits(original.clone(), &[json!({
                    "op":"layer.update", "layer":target, "color_label":label,
                })]).unwrap();
                assert_eq!(doc.export_png().unwrap(), composite);
                for layout in [Layout::Psd, Layout::Psb] {
                    let encoded = encode_layout(&doc, layout).unwrap();
                    let mut cursor = Cursor::new(&encoded);
                    cursor.bytes(26).unwrap();
                    cursor.block().unwrap();
                    let resource_length = cursor.pos;
                    cursor.block().unwrap();
                    let mut standard = encoded[..resource_length].to_vec();
                    u32b(&mut standard, 0);
                    standard.extend_from_slice(&encoded[cursor.pos..]);
                    let structure = structural::index(
                        &mut std::io::Cursor::new(&standard), structural::Limits::current(), || Ok(()),
                    ).unwrap();
                    let tag = structure.layers[0].tags.iter().find(|tag| tag.key == *b"lclr").unwrap();
                    let at = tag.data.offset as usize;
                    assert_eq!(&standard[at..at + 8], &[0, index, 0, 0, 0, 0, 0, 0]);
                    for bytes in [&encoded, &standard] {
                        let reopened = decode(bytes).unwrap();
                        assert!(!reopened.read_only, "{:?}", reopened.warnings);
                        assert_eq!(layer_color_label(&reopened.layers[0].psd_metadata), label);
                        assert_eq!(reopened.layers[0].pixels, pixels);
                    }
                    standard[at + 1] = 12;
                    assert!(decode(&standard).unwrap().read_only);
                }
                let cleared = Engine::preview_edits(doc, &[json!({
                    "op":"layer.update", "layer":target, "color_label":"none",
                })]).unwrap();
                assert!(cleared.layers[0].psd_metadata.is_empty());
            }
        }
    }

    #[test]
    fn blend_if_round_trips_standard_ranges_and_guards_the_saved_composite() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            let target = doc.layers[0].id.clone();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            doc.layers[0].pixels.set16(1, 0, [45679, 23459, 34571, 65535]);
            let original = doc.layers[0].pixels.clone();
            let pairs = [blend_ranges::DEFAULT, [128, 128, 255, 255, 0, 0, 255, 255]];
            let doc = crate::engine::Engine::preview_edits(doc, &[serde_json::json!({
                "op":"layer.update", "layer":target, "blend_if":pairs,
            })]).unwrap();
            let incompatible = serde_json::json!({"op":"effect.add","layer":target,"kind":"invert"});
            assert!(crate::engine::Engine::preview_edits(doc.clone(), &[incompatible]).is_err());
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let reopened = decode(&bytes).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].pixels, original);

                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let start = file.pos;
                file.block().unwrap();
                let mut standard = bytes[..start].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[file.pos..]);
                let index = structural::index(
                    &mut std::io::Cursor::new(&standard), structural::Limits::current(), || Ok(()),
                ).unwrap();
                let span = index.layers[0].blending_ranges;
                let offset = span.offset as usize;
                assert_eq!(span.bytes, 32);
                assert_eq!(&standard[offset..offset + 16], pairs.as_flattened());
                assert!(index.layers[0].tags.iter().all(|tag| tag.key != blend_ranges::TAG));
                let reopened = decode_reader(&mut std::io::Cursor::new(&standard), &Default::default()).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].pixels, original);
                assert_eq!(blend_ranges::bytes(&reopened.layers[0].psd_metadata), Some(&pairs.as_flattened()[..]));

                standard[offset + 8..offset + 10].fill(250);
                assert!(decode(&standard).unwrap().read_only);
            }
        }
    }

    #[test]
    fn blend_if_scales_coverage_without_replacing_special_fill_strength() {
        use crate::engine::Engine;
        use serde_json::json;
        for depth in [8, 16] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            let base = &mut doc.layers[0];
            base.pixels.set16(0, 0, [16448, 16448, 16448, 65535]);
            let mut top = Layer::new("Gated", "paint", 2, 1);
            top.pixels = Raster::new_depth(2, 1, depth);
            for x in 0..2 {
                top.pixels.set16(x, 0, [32896, 32896, 32896, 65535]);
            }
            let target = top.id.clone();
            doc.layers.insert(0, top);
            for (blend, rgb) in [
                ("multiply", if depth == 8 { 56 } else { 14392 }),
                ("linear_dodge", if depth == 8 { 96 } else { 24705 }),
            ] {
                let edited = Engine::preview_edits(doc.clone(), &[json!({
                    "op":"layer.update","layer":target,"blend":blend,"fill_opacity":128,
                    "blend_if":[blend_ranges::DEFAULT, [128,129,255,255,0,0,255,255]],
                })]).unwrap();
                let (max, src, alpha) = if depth == 8 { (255, 128, 64) } else { (65535, 32896, 16448) };
                let actual: Vec<u16> = if depth == 8 {
                    edited.preview_native8(None, 2, None, false).unwrap().2
                        .into_iter().map(u16::from).collect()
                } else {
                    crate::depth16::render(&edited).unwrap().words
                };
                assert_eq!(actual, [rgb, rgb, rgb, max, src, src, src, alpha], "{depth}-bit {blend}");
            }
        }
    }

    #[test]
    fn clipping_group_fill_round_trips_with_its_original_masking_alpha() {
        for depth in [8, 16] {
            let (doc, expected) = fill_opacity::clipped_fixture(depth);
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let start = file.pos;
                file.block().unwrap();
                let mut standard = bytes[..start].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[file.pos..]);
                let decoded = decode_reader(
                    &mut std::io::Cursor::new(&standard), &Default::default(),
                ).unwrap();
                assert!(!decoded.read_only, "{:?}", decoded.warnings);
                assert_eq!(decoded.layers[0].clip_to.as_deref(), Some(decoded.layers[1].id.as_str()));
                assert_eq!(decoded.layers[0].blend, "linear_light");
                for (actual, original) in decoded.layers.iter().zip(&doc.layers) {
                    assert_eq!(fill_opacity::value(&actual.psd_metadata), 128);
                    assert_eq!(actual.pixels, original.pixels);
                }
                let actual = if depth == 8 {
                    decoded.preview_native8(None, 2, None, false).unwrap().2
                        .into_iter().map(u16::from).collect()
                } else {
                    crate::depth16::render(&decoded).unwrap().words
                };
                assert_eq!(actual, [expected, [0; 4]].concat());
            }
        }
    }

    #[test]
    fn standard_fill_opacity_preserves_source_samples_and_checks_the_saved_composite() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(1, 1, depth).unwrap();
            let target = doc.layers[0].id.clone();
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
            let original = doc.layers[0].pixels.get16(0, 0);
            let commands = [serde_json::json!({
                "op":"layer.update", "layer":target, "opacity":128. / 255., "fill_opacity":128
            })];
            let doc = crate::engine::Engine::preview_edits(doc, &commands).unwrap();
            assert_eq!(doc.layers[0].pixels.get16(0, 0), original);
            let refused = serde_json::json!({"op":"effect.add","layer":target,"kind":"invert"});
            assert!(crate::engine::Engine::preview_edits(doc.clone(), &[refused]).is_err());
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                assert!(!decode(&bytes).unwrap().read_only);
                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let start = file.pos;
                file.block().unwrap();
                let mut standard = bytes[..start].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&bytes[file.pos..]);
                let index = structural::index(
                    &mut std::io::Cursor::new(&standard),
                    structural::Limits::current(),
                    || Ok(()),
                ).unwrap();
                let layer = &index.layers[0];
                assert_eq!(layer.opacity, 128);
                let tag = layer.tags.iter().find(|tag| tag.key == *b"iOpa").unwrap();
                let offset = tag.data.offset as usize;
                assert_eq!(&standard[offset..offset + 4], &[128, 0, 0, 0]);
                let reopened = decode_reader(
                    &mut std::io::Cursor::new(&standard), &Default::default(),
                ).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                let layer = &reopened.layers[0];
                assert_eq!(fill_opacity::value(&layer.psd_metadata), 128);
                assert_eq!(layer.pixels.get16(0, 0), original);
                standard[offset] = 64;
                assert!(decode(&standard).unwrap().read_only);
            }
        }
    }

    #[test]
    fn contrast_modes_keep_their_native_channels_and_require_the_exact_composite() {
        let modes = [
            ("linear_light", *b"lLit", [128, 164, 8], [32768, 20515, 498]),
            ("vivid_light", *b"vLit", [128, 78, 127], [32768, 11924, 32767]),
            ("pin_light", *b"pLit", [128, 124, 18], [32768, 10502, 998]),
            ("hard_mix", *b"hMix", [255, 0, 255], [65535, 0, 65535]),
        ];
        for (mode, key, expected8, expected16) in modes {
            check_layer_blend(
                mode,
                key,
                8,
                [128, 40, 246].map(|value| value * 257),
                [128, 190, 9].map(|value| value * 257),
                expected8.map(|value| value * 257),
            );
            check_layer_blend(
                mode,
                key,
                16,
                [32768, 10013, 65036],
                [32768, 38019, 499],
                expected16,
            );
        }
    }

    #[test]
    fn whole_rgb_modes_preserve_gamut_and_native_low_bits_in_standard_layers() {
        let modes = [
            ("darker_color", *b"dkCl", [238, 25, 233], [32767, 32768, 32769]),
            ("lighter_color", *b"lgCl", [14, 242, 90], [1001, 65000, 4121]),
            ("hue", *b"hue ", [255, 89, 251], [32767, 32769, 32767]),
            ("saturation", *b"sat ", [23, 236, 94], [10464, 38000, 65535]),
            ("color", *b"colr", [255, 89, 251], [0, 55038, 2683]),
            ("luminosity", *b"lum ", [0, 178, 59], [39103, 39104, 39105]),
        ];
        for (mode, key, expected8, expected16) in modes {
            check_layer_blend(
                mode,
                key,
                8,
                [14, 242, 90].map(|value| value * 257),
                [238, 25, 233].map(|value| value * 257),
                expected8.map(|value| value * 257),
            );
            check_layer_blend(
                mode,
                key,
                16,
                [32767, 32768, 32769],
                [1001, 65000, 4121],
                expected16,
            );
        }
    }

    fn check_layer_blend(
        mode: &str,
        key: [u8; 4],
        depth: u16,
        base: [u16; 3],
        source: [u16; 3],
        expected: [u16; 3],
    ) {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        let [r, g, b] = base;
        doc.layers[0].pixels.set16(0, 0, [r, g, b, 65535]);
        let mut top = Layer::new("Blend", "paint", 1, 1);
        top.blend = mode.into();
        top.pixels = Raster::new_depth(1, 1, depth);
        let [r, g, b] = source;
        top.pixels.set16(0, 0, [r, g, b, 65535]);
        doc.layers.insert(0, top);
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&doc, layout).unwrap();
            let mut file = Cursor::new(&bytes);
            file.bytes(26).unwrap();
            file.block().unwrap();
            let resource_length = file.pos;
            file.block().unwrap();
            let mut standard = bytes[..resource_length].to_vec();
            u32b(&mut standard, 0);
            standard.extend_from_slice(&bytes[file.pos..]);
            let index = structural::index(
                &mut std::io::Cursor::new(&standard),
                structural::Limits::current(),
                || Ok(()),
            )
            .unwrap();
            assert_eq!(index.layers[1].blend_mode, key);
            let merged = index.composite.span.offset as usize;
            assert_eq!(&standard[merged..merged + 2], &[0, 0]);
            let [r, g, b] = expected;
            let samples = [r, g, b, 65535];
            let expected_bytes: Vec<_> = if depth == 8 {
                samples.map(|word| (word / 257) as u8).to_vec()
            } else {
                samples.into_iter().flat_map(u16::to_be_bytes).collect()
            };
            assert_eq!(&standard[merged + 2..], expected_bytes);
            for decoded in [
                decode(&standard).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&standard), &Default::default())
                    .unwrap(),
            ] {
                assert!(!decoded.read_only, "{:?}", decoded.warnings);
                assert_eq!(decoded.layers[0].blend, mode);
                let [r, g, b] = source;
                assert_eq!(decoded.layers[0].pixels.get16(0, 0), [r, g, b, 65535]);
            }
            standard[merged + 2 + usize::from(depth == 16)] ^= 1;
            assert!(decode(&standard).unwrap().read_only);
        }
    }

    #[test]
    fn duplicate_source_resources_never_hide_unsupported_definitions_in_either_order() {
        for depth in [8, 16] {
            let doc = streamed_fixture(depth);
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let resource_position = file.pos;
                let resources = file.block().unwrap();
                let standard_position = file.pos;
                assert_eq!(&resources[..20], b"8BIM\x0f\xa0\x0cPeerBrush.v1\0");
                let payload_length =
                    u32::from_be_bytes(resources[20..24].try_into().unwrap()) as usize;
                let mut json = Vec::new();
                ZlibDecoder::new(&resources[28..24 + payload_length])
                    .read_to_end(&mut json)
                    .unwrap();
                let mut future: Embedded = serde_json::from_slice(&json).unwrap();
                future.format = u32::MAX;
                let mut compressor = ZlibEncoder::new(Vec::new(), Compression::fast());
                serde_json::to_writer(&mut compressor, &future).unwrap();
                let payload = [b"PBR1".as_slice(), &compressor.finish().unwrap()].concat();
                let mut future_resource = resources[..20].to_vec();
                block(&mut future_resource, &payload);
                if payload.len() % 2 != 0 {
                    future_resource.push(0);
                }
                // The fixture has partial alpha. Protected standard previews
                // unmatte the stored white compatibility samples, so use the
                // unsupported resource alone as the existing fallback oracle.
                let mut reference = bytes[..resource_position].to_vec();
                block(&mut reference, &future_resource);
                reference.extend_from_slice(&bytes[standard_position..]);
                let reference = decode(&reference).unwrap();
                assert!(reference.read_only);
                assert_eq!(reference.bit_depth, depth);
                let expected = reference.export_png().unwrap();
                for selected in [
                    [future_resource.as_slice(), resources].concat(),
                    [resources, future_resource.as_slice()].concat(),
                    [resources, resources].concat(),
                ] {
                    let mut changed = bytes[..resource_position].to_vec();
                    block(&mut changed, &selected);
                    changed.extend_from_slice(&bytes[standard_position..]);
                    for restored in [
                        decode(&changed).unwrap(),
                        decode_reader(
                            &mut std::io::Cursor::new(&changed),
                            &crate::loading::Control::default(),
                        )
                        .unwrap(),
                    ] {
                        assert!(restored.read_only);
                        assert!(restored
                            .warnings
                            .iter()
                            .any(|warning| warning
                                .starts_with("Duplicate PeerBrush source resources")));
                        assert_eq!(restored.bit_depth, depth);
                        assert_eq!(restored.export_png().unwrap(), expected);
                        assert!(encode_layout(&restored, layout).is_err());
                    }
                }
            }
        }
    }

    #[test]
    fn source_resource_framing_keeps_native_sources_history_and_standard_bytes() {
        for depth in [8, 16] {
            for tagged in [false, true] {
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = streamed_fixture(depth);
                engine.resident_state_mut().expect("resident fixture").document.srgb_tagged = tagged;
                let layer = &mut engine.resident_state_mut().expect("resident fixture").document.layers[0];
                layer.pixels.retained =
                    Some(Box::new(crate::retained::Original::from(&layer.pixels)));
                let mask = &mut layer.mask.as_mut().unwrap().steps[0].pixels;
                mask.retained = Some(Box::new(crate::retained::Original::from(mask)));
                let layer_id = layer.id.clone();
                for opacity in [0.5, 1.0, 0.25] {
                    engine.edit(
                        "human",
                        &[serde_json::json!({"op":"layer.update","layer":layer_id,"opacity":opacity})],
                        Some(engine.resident_state().expect("resident fixture").document.revision),
                        None,
                        "Resource fixture opacity",
                    ).unwrap();
                }
                engine.undo("human").unwrap();
                assert_eq!((engine.resident_state().expect("resident fixture").undo.len(), engine.resident_state().expect("resident fixture").redo.len()), (2, 1));
                assert_eq!(engine.resident_state().expect("resident fixture").document.layers[0].opacity, 1.0);
                let history = |engine: &crate::engine::Engine| {
                    let entries = |items: &[crate::engine::History]| {
                        items.iter().map(|h| {
                        serde_json::json!({"before":h.before,"after":h.after,"actor":h.actor,
                            "label":h.label,"task":h.task,"scopes":h.scopes,"gesture":h.gesture,
                            "reverted":h.reverted})
                    }).collect::<Vec<_>>()
                    };
                    serde_json::json!({"undo":entries(&engine.resident_state().expect("resident fixture").undo),"redo":entries(&engine.resident_state().expect("resident fixture").redo),
                        "saved_revision":engine.saved_revision,"leases":engine.leases})
                };
                let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
                let history_before = history(&engine);
                for layout in [Layout::Psd, Layout::Psb] {
                    let bytes = encode_layout(&engine.resident_state().expect("resident fixture").document, layout).unwrap();
                    let mut file = Cursor::new(&bytes);
                    assert_eq!(file.bytes(4).unwrap(), b"8BPS");
                    assert_eq!(file.u16().unwrap(), if layout.large() { 2 } else { 1 });
                    file.bytes(20).unwrap();
                    assert!(file.block().unwrap().is_empty());
                    let resources = file.block().unwrap();
                    assert_eq!(&resources[..20], b"8BIM\x0f\xa0\x0cPeerBrush.v1\0");
                    let payload_length =
                        u32::from_be_bytes(resources[20..24].try_into().unwrap()) as usize;
                    assert_eq!(&resources[24..28], b"PBR1");
                    let compressed = &resources[28..24 + payload_length];
                    let mut expected = b"8BIM\x0f\xa0\x0cPeerBrush.v1\0".to_vec();
                    let payload = [b"PBR1".as_slice(), compressed].concat();
                    expected.extend((payload.len() as u32).to_be_bytes());
                    expected.extend(&payload);
                    if payload.len() % 2 != 0 {
                        expected.push(0);
                    }
                    if tagged {
                        let profile = crate::color_profile::srgb_profile();
                        expected.extend(b"8BIM\x04\x0f\0\0");
                        expected.extend((profile.len() as u32).to_be_bytes());
                        expected.extend(profile);
                        if profile.len() % 2 != 0 {
                            expected.push(0);
                        }
                    }
                    assert_eq!(resources, expected);
                    let mut json = Vec::new();
                    ZlibDecoder::new(compressed).read_to_end(&mut json).unwrap();
                    let embedded: Embedded = serde_json::from_slice(&json).unwrap();
                    assert_eq!(embedded.format, 39);
                    assert_eq!(serde_json::to_value(&embedded.document).unwrap(), before);
                    // Independent previous Vec-compressor route, same source and
                    // settings: append/replay must preserve actual PBR1 bytes.
                    let original = Embedded {
                        format: embedded.format,
                        standard_hash: embedded.standard_hash,
                        document: engine.resident_state().expect("resident fixture").document.clone(),
                    };
                    assert_eq!(json, serde_json::to_vec(&original).unwrap());
                    let mut old = ZlibEncoder::new(Vec::new(), Compression::fast());
                    serde_json::to_writer(&mut old, &original).unwrap();
                    assert_eq!(compressed, old.finish().unwrap());
                    let section = layout.block(&mut file).unwrap();
                    let merged = &bytes[file.pos..];
                    let standard = expected_standard(&engine.resident_state().expect("resident fixture").document, layout.large());
                    assert_eq!(section, standard.0);
                    assert_eq!(merged, standard.1);
                    let hash = |data: &[u8]| {
                        data.iter().fold(0xcbf29ce484222325u64, |hash, byte| {
                            (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
                        })
                    };
                    assert_eq!(embedded.standard_hash, hash(section) ^ hash(merged));
                    let restored = decode(&bytes).unwrap();
                    assert!(!restored.read_only);
                    assert_eq!(restored.bit_depth, depth);
                    assert_eq!(serde_json::to_value(&restored).unwrap(), before);
                    assert!(restored.layers[0].pixels.retained.is_some());
                    assert!(restored.layers[0].mask.as_ref().unwrap().steps[0]
                        .pixels
                        .retained
                        .is_some());
                    let png = restored.export_png().unwrap();
                    let actual = image::load_from_memory(&png).unwrap();
                    assert_eq!(
                        actual.color(),
                        if depth == 16 {
                            image::ColorType::Rgba16
                        } else {
                            image::ColorType::Rgba8
                        }
                    );
                    assert_eq!(png, engine.resident_state().expect("resident fixture").document.export_png().unwrap());
                    assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
                    assert_eq!(history(&engine), history_before);
                }
            }
        }
    }

    #[test]
    fn compressed_source_transfer_is_chunked_and_keeps_short_write_error_and_cancel_guards() {
        struct Output<'a> {
            bytes: Vec<u8>,
            largest: usize,
            stop_at: Option<usize>,
            cancel: bool,
            control: &'a crate::saving::Control,
        }
        impl Write for Output<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.largest = self.largest.max(bytes.len());
                if self.stop_at.is_some_and(|limit| self.bytes.len() >= limit) {
                    return Err(std::io::Error::other(
                        "Injected compressed source write failure",
                    ));
                }
                let n = bytes.len().min(997);
                self.bytes.extend_from_slice(&bytes[..n]);
                if self.cancel && self.stop_at.is_some_and(|limit| self.bytes.len() >= limit) {
                    assert!(self.control.cancel());
                }
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        for depth in [8, 16] {
            let mut doc = Document::new_depth(257, 193, depth).unwrap();
            let mut random = 0xD31B49A7u32;
            for y in 0..193 {
                for x in 0..257 {
                    let mut words = [0u16; 4];
                    for word in &mut words {
                        random ^= random << 13;
                        random ^= random >> 17;
                        random ^= random << 5;
                        *word = random as u16;
                    }
                    doc.layers[0].pixels.set16(x, y, words);
                }
            }
            doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 0]);
            let before = serde_json::to_value(&doc).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let expected = encode_layout(&doc, layout).unwrap();
                let payload_length =
                    u32::from_be_bytes(expected[54..58].try_into().unwrap()) as usize;
                assert!(
                    payload_length > 2 * 64 * 1024,
                    "The fixture reaches multiple source chunks"
                );
                let control = crate::saving::Control::default();
                control.begin(doc.id.clone(), doc.revision).unwrap();
                let mut complete = Output {
                    bytes: vec![],
                    largest: 0,
                    stop_at: None,
                    cancel: false,
                    control: &control,
                };
                let mut spool = std::io::Cursor::new(Vec::new());
                encode_layout_to(&doc, layout, &mut complete, &mut spool, &|| control.check())
                    .unwrap();
                assert!(complete.largest <= 64 * 1024);
                assert_eq!(complete.bytes, expected);
                let standard = spool.into_inner();
                for cancel in [false, true] {
                    let control = crate::saving::Control::default();
                    control.begin(doc.id.clone(), doc.revision).unwrap();
                    let mut incomplete = Output {
                        bytes: vec![],
                        largest: 0,
                        stop_at: Some(62 + 64 * 1024),
                        cancel,
                        control: &control,
                    };
                    let mut spool = std::io::Cursor::new(Vec::new());
                    let error =
                        encode_layout_to(&doc, layout, &mut incomplete, &mut spool, &|| {
                            control.check()
                        })
                        .unwrap_err();
                    assert_eq!(
                        error,
                        if cancel {
                            "Save canceled; current work and destination preserved"
                        } else {
                            "Injected compressed source write failure"
                        }
                    );
                    assert_eq!(control.status().cancel_requested, cancel);
                    assert!(incomplete.largest <= 64 * 1024);
                    assert!(incomplete.bytes.len() >= 62 + 64 * 1024);
                    assert!(incomplete.bytes.len() < expected.len());
                    assert_eq!(incomplete.bytes, expected[..incomplete.bytes.len()]);
                    assert_eq!(spool.into_inner(), standard);
                    assert_eq!(serde_json::to_value(&doc).unwrap(), before);
                }
                assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            }
        }
    }

    #[test]
    fn bounded_source_and_final_writers_keep_short_write_limits_and_cancellation() {
        struct Short {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl Write for Short {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let n = bytes.len().min(2);
                self.bytes.extend_from_slice(&bytes[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }
        for error in [
            "Editable PSD source data exceeds its bounded serialization budget",
            "Photoshop output exceeds the initial 256 MiB file limit",
        ] {
            let cancelled = std::cell::Cell::new(false);
            let checks = std::cell::Cell::new(0);
            let check = || {
                checks.set(checks.get() + 1);
                if cancelled.get() {
                    Err("Project source changed while saving".into())
                } else {
                    Ok(())
                }
            };
            let mut writer = BoundedWriter {
                inner: Short {
                    bytes: vec![],
                    flushes: 0,
                },
                written: 0,
                limit: 3,
                check: &check,
                error,
            };
            writer.write_all(b"abc").unwrap();
            assert_eq!(writer.written, 3);
            assert_eq!(writer.inner.bytes, b"abc");
            assert_eq!(
                checks.get(),
                2,
                "Every partial write still checks the source"
            );
            assert_eq!(writer.write(b"d").unwrap_err().to_string(), error);
            assert_eq!(writer.written, 3);
            assert_eq!(writer.inner.bytes, b"abc");
            cancelled.set(true);
            for result in [writer.write(b"d").map(|_| ()), writer.flush()] {
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "Project source changed while saving"
                );
            }
            assert_eq!(
                writer.inner.flushes, 0,
                "Cancelled flush never reaches the file"
            );
            assert_eq!(writer.written, 3);
            assert_eq!(writer.inner.bytes, b"abc");
            cancelled.set(false);
            writer.flush().unwrap();
            assert_eq!(writer.inner.flushes, 1);
        }
    }
    #[test]
    fn native_channel_rows_match_scalar_samples_across_sparse_tiles_and_padding() {
        for depth in [8, 16] {
            let mut pixels = Raster::new_depth(513, 259, depth);
            for (x, y, words) in [
                (0, 0, [12347, 23459, 34571, 0]),
                (255, 255, [45679, 11113, 33337, 32769]),
                (256, 255, [60001, 20003, 40009, 1]),
                (511, 256, [137, 911, 31001, 65535]),
                (512, 258, [54321, 12345, 45671, 0]),
            ] {
                pixels.set16(x, y, words);
            }
            let before = pixels.clone();
            for y in [0, 1, 254, 255, 256, 258, 259] {
                for width in [1, 255, 256, 257, 513, 514] {
                    for channel in 0..4 {
                        let mut actual = vec![0xA5; width * usize::from(depth / 8)];
                        native_channel_row(&pixels, y, channel, &mut actual);
                        let expected = (0..width as i32)
                            .flat_map(|x| {
                                if depth == 16 {
                                    pixels.get16(x, y as i32)[channel].to_be_bytes().to_vec()
                                } else {
                                    vec![pixels.get(x, y as i32)[channel]]
                                }
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(
                            actual, expected,
                            "depth={depth}, y={y}, width={width}, channel={channel}"
                        );
                    }
                }
            }
            assert_eq!(pixels, before);
            assert_eq!(pixels.get16(0, 0)[3], 0);
            assert_ne!(pixels.get16(0, 0)[0], 0);
        }
    }

    #[test]
    fn sparse_native_channel_rows_keep_negative_layer_bounds_and_standard_plane_bytes() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(269, 3, depth).unwrap();
            let layer = &mut doc.layers[0];
            layer.x = -3;
            layer.y = -2;
            layer.pixels = Raster::new_depth(513, 259, depth);
            for (x, y, words) in [
                (0, 0, [12347, 23459, 34571, 0]),
                (255, 0, [45679, 11113, 33337, 32769]),
                (256, 1, [60001, 20003, 40009, 1]),
                (511, 256, [137, 911, 31001, 65535]),
                (512, 258, [54321, 12345, 45671, 0]),
            ] {
                layer.pixels.set16(x, y, words);
            }
            let before = serde_json::to_value(&doc).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let mut file = Cursor::new(&bytes);
                file.bytes(26).unwrap();
                file.block().unwrap();
                file.block().unwrap();
                let section = layout.block(&mut file).unwrap();
                let mut record = Cursor::new(layer_info(section, depth, layout).unwrap());
                assert_eq!(record.i16().unwrap(), -1);
                assert_eq!(
                    [
                        record.i32().unwrap(),
                        record.i32().unwrap(),
                        record.i32().unwrap(),
                        record.i32().unwrap()
                    ],
                    [-2, -3, 257, 510]
                );
                assert_eq!(record.u16().unwrap(), 4);
                let plane_length = 2 + 513 * 259 * usize::from(depth / 8);
                for channel in [-1, 0, 1, 2] {
                    assert_eq!(record.i16().unwrap(), channel);
                    assert_eq!(record.length(layout.large()).unwrap(), plane_length);
                }
                assert_eq!(record.bytes(8).unwrap(), b"8BIMnorm");
                record.bytes(4).unwrap();
                record.block().unwrap();
                for channel in [3usize, 0, 1, 2] {
                    assert_eq!(record.u16().unwrap(), 0);
                    let actual = record.bytes(plane_length - 2).unwrap();
                    let mut expected = Vec::with_capacity(plane_length - 2);
                    for y in 0..259 {
                        for x in 0..513 {
                            if depth == 16 {
                                expected.extend(
                                    doc.layers[0].pixels.get16(x, y)[channel].to_be_bytes(),
                                );
                            } else {
                                expected.push(doc.layers[0].pixels.get(x, y)[channel]);
                            }
                        }
                    }
                    assert_eq!(actual, expected);
                }
                let reopened = decode(&bytes).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.bit_depth, depth);
                assert_eq!((reopened.layers[0].x, reopened.layers[0].y), (-3, -2));
                assert_eq!(reopened.layers[0].pixels, doc.layers[0].pixels);
                assert_eq!(reopened.export_png().unwrap(), doc.export_png().unwrap());
                assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            }
        }
        // The empty 16-bit document's synthetic standard layer is still an
        // 8-bit empty Raster: it retains the existing scalar fallback.
        let mut empty = Document::new_depth(2, 1, 16).unwrap();
        empty.layers.clear();
        for bytes in [encode(&empty).unwrap(), encode_psb(&empty).unwrap()] {
            let reopened = decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, 16);
            assert!(reopened.layers.is_empty());
        }
    }

    fn streamed_fixture(depth: u16) -> Document {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        let layer = &mut doc.layers[0];
        layer.name = "X".into();
        if depth == 16 {
            layer.pixels.set16(0, 0, [12347, 23459, 34571, 0]);
            layer.pixels.set16(1, 0, [50001, 30003, 10001, 32769]);
        } else {
            layer.pixels.set(0, 0, [17, 99, 201, 0]);
            layer.pixels.set(1, 0, [222, 144, 33, 127]);
        }
        let mut mask = Raster::new_depth(2, 1, depth);
        mask.set16(0, 0, [0, 0, 0, 65535]);
        mask.set16(1, 0, [12345, 12345, 12345, 65535]);
        layer.mask = Some(Mask {
            linked: false,
            enabled: false,
            cache_key: id(),
            steps: vec![MaskStep {
                id: id(),
                kind: "paint".into(),
                enabled: true,
                weight: 1.,
                value: 0.,
                pixels: mask,
                settings: serde_json::Value::Null,
            }],
        });
        doc
    }
    // Independent literal record structure: one 1/2x1 paint record, disabled mask and name X.
    fn expected_standard(doc: &Document, large: bool) -> (Vec<u8>, Vec<u8>) {
        let mut record = vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        record.extend(doc.width.to_be_bytes());
        record.extend([0, 5]);
        let length = 2 + u64::from(doc.width) * u64::from(doc.bit_depth / 8);
        for channel in [-1i16, 0, 1, 2, -2] {
            record.extend(channel.to_be_bytes());
            if large {
                record.extend(length.to_be_bytes());
            } else {
                record.extend((length as u32).to_be_bytes());
            }
        }
        record.extend(b"8BIMnorm");
        record.extend([255, 0, 0, 0]);
        record.extend(66u32.to_be_bytes());
        record.extend([0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        record.extend(doc.width.to_be_bytes());
        record.extend([255, 3, 0, 0]);
        record.extend([0, 0, 0, 0, 1, b'X', 0, 0]);
        record.extend(b"8BIMluni");
        record.extend([0, 0, 0, 6, 0, 0, 0, 1, 0, b'X']);
        record.extend(b"8BIMlmgm");
        record.extend([0, 0, 0, 4, 0, 0, 0, 0]);
        let mut info = (-1i16).to_be_bytes().to_vec();
        info.extend(record);
        for channel in [3usize, 0, 1, 2, 4] {
            info.extend([0, 0]);
            for x in 0..doc.width as i32 {
                let value = if channel == 4 {
                    doc.layers[0].mask.as_ref().unwrap().steps[0]
                        .pixels
                        .get16(x, 0)[0]
                } else {
                    doc.layers[0].pixels.get16(x, 0)[channel]
                };
                if doc.bit_depth == 16 {
                    info.extend(value.to_be_bytes());
                } else {
                    info.push((value / 257) as u8);
                }
            }
        }
        if info.len() % 2 != 0 {
            info.push(0);
        }
        let length = |out: &mut Vec<u8>, n: usize| {
            if large {
                out.extend((n as u64).to_be_bytes());
            } else {
                out.extend((n as u32).to_be_bytes());
            }
        };
        let mut section = vec![];
        if doc.bit_depth == 16 {
            length(&mut section, 0);
            section.extend([0; 4]);
            section.extend(b"8BIMLr16");
        }
        length(&mut section, info.len());
        section.extend(&info);
        if doc.bit_depth == 16 {
            section.resize(section.len() + (4 - info.len() % 4) % 4, 0);
            section.extend(b"8BIMMt16");
            length(&mut section, 0);
        } else {
            section.extend([0; 4]);
        }
        let rendered = image::load_from_memory(&doc.export_png().unwrap())
            .unwrap()
            .into_rgba16()
            .into_raw();
        let mut merged = vec![0, 0];
        for c in 0..4 {
            for p in rendered.chunks_exact(4) {
                if doc.bit_depth == 16 {
                    let v = if c < 3 {
                        ((u64::from(p[c]) * u64::from(p[3])
                            + 65535 * (65535 - u64::from(p[3]))
                            + 32767)
                            / 65535) as u16
                    } else {
                        p[3]
                    };
                    merged.extend(v.to_be_bytes());
                } else {
                    let (v, a) = (u32::from(p[c] / 257), u32::from(p[3] / 257));
                    merged.push(if c < 3 {
                        ((v * a + 255 * (255 - a) + 127) / 255) as u8
                    } else {
                        a as u8
                    });
                }
            }
        }
        (section, merged)
    }
    #[test]
    fn save_progress_counts_real_rows_and_cancellation_keeps_exact_native_sources() {
        use std::cell::{Cell, RefCell};
        for depth in [8, 16] {
            let doc = streamed_fixture(depth);
            let before = serde_json::to_value(&doc).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let rows = RefCell::new(Vec::new());
                let mut output = Vec::new();
                let mut spool = std::io::Cursor::new(Vec::new());
                encode_layout_progress_to(
                    &doc,
                    layout,
                    &mut output,
                    &mut spool,
                    &|| Ok(()),
                    &|stage, completed, total| {
                        if total != 0 {
                            rows.borrow_mut()
                                .push((stage.to_string(), completed, total));
                        }
                        Ok(())
                    },
                )
                .unwrap();
                let rows = rows.into_inner();
                for (stage, total) in [
                    ("Writing standard layer channels", 5),
                    ("Writing current composite", 4),
                ] {
                    let actual: Vec<_> = rows
                        .iter()
                        .filter(|row| row.0 == stage)
                        .map(|row| (row.1, row.2))
                        .collect();
                    assert_eq!(
                        actual,
                        (0..=total).map(|done| (done, total)).collect::<Vec<_>>()
                    );
                }
                let mut cursor = Cursor::new(&output);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                cursor.block().unwrap();
                let section = layout.block(&mut cursor).unwrap();
                let expected = expected_standard(&doc, layout.large());
                assert_eq!(section, expected.0);
                assert_eq!(&output[cursor.pos..], expected.1);
                assert_eq!(
                    serde_json::to_value(decode(&output).unwrap()).unwrap(),
                    before
                );

                let control = crate::saving::Control::default();
                control.begin(doc.id.clone(), doc.revision).unwrap();
                let stopped = Cell::new(false);
                let mut incomplete = Vec::new();
                let mut partial_spool = std::io::Cursor::new(Vec::new());
                let error = encode_layout_progress_to(
                    &doc,
                    layout,
                    &mut incomplete,
                    &mut partial_spool,
                    &|| control.check(),
                    &|stage, completed, total| {
                        control.progress(stage, completed, total)?;
                        if stage == "Writing standard layer channels" && completed == 2 {
                            assert!(control.cancel());
                            stopped.set(true);
                        }
                        control.check()
                    },
                )
                .unwrap_err();
                assert!(error.contains("canceled"));
                assert!(stopped.get());
                assert!(incomplete.is_empty());
                assert!(partial_spool.get_ref().len() < spool.get_ref().len());
                assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            }
        }
    }

    #[test]
    fn streamed_psd_and_psb_match_independent_standard_bytes_and_native_private_sources() {
        for depth in [8, 16] {
            for width in [1, 2] {
                let mut doc = streamed_fixture(depth);
                if width == 1 {
                    doc.width = 1;
                    let color = doc.layers[0].pixels.get16(0, 0);
                    doc.layers[0].pixels = Raster::new_depth(1, 1, depth);
                    doc.layers[0].pixels.set16(0, 0, color);
                    let step = &mut doc.layers[0].mask.as_mut().unwrap().steps[0];
                    let mask = step.pixels.get16(0, 0);
                    step.pixels = Raster::new_depth(1, 1, depth);
                    step.pixels.set16(0, 0, mask);
                }
                let before = serde_json::to_value(&doc).unwrap();
                for layout in [Layout::Psd, Layout::Psb] {
                    let mut spool = std::io::Cursor::new(Vec::new());
                    let mut bytes = vec![];
                    encode_layout_to(&doc, layout, &mut bytes, &mut spool, &|| Ok(())).unwrap();
                    assert_eq!(bytes, encode_layout(&doc, layout).unwrap());
                    let mut cursor = Cursor::new(&bytes);
                    cursor.bytes(26).unwrap();
                    cursor.block().unwrap();
                    let resources = cursor.block().unwrap();
                    let payload_length =
                        u32::from_be_bytes(resources[20..24].try_into().unwrap()) as usize;
                    let compressed = &resources[28..24 + payload_length];
                    let section = layout.block(&mut cursor).unwrap();
                    let merged = &bytes[cursor.pos..];
                    let expected = expected_standard(&doc, layout.large());
                    assert_eq!(section, expected.0);
                    assert_eq!(merged, expected.1);
                    assert_eq!(spool.get_ref(), &[section, merged, compressed].concat());
                    let overhead = 34 + resources.len() - compressed.len()
                        + if layout.large() { 8 } else { 4 };
                    assert_eq!(bytes.len(), spool.get_ref().len() + overhead);
                    assert!(overhead >= if layout.large() { 70 } else { 66 });
                    let loaded = decode(&bytes).unwrap();
                    assert!(!loaded.read_only);
                    assert_eq!(serde_json::to_value(&loaded).unwrap(), before);
                    let mut standard = bytes[..30].to_vec();
                    standard.extend([0; 4]);
                    layout
                        .write_length(&mut standard, section.len() as u64)
                        .unwrap();
                    standard.extend(section);
                    standard.extend(merged);
                    let ordinary = decode(&standard).unwrap();
                    assert_eq!(ordinary.layers.len(), 1);
                    assert_eq!(
                        ordinary.layers[0].pixels.get16(0, 0),
                        doc.layers[0].pixels.get16(0, 0)
                    );
                    assert_eq!(
                        ordinary.layers[0].pixels.get16(1, 0),
                        doc.layers[0].pixels.get16(1, 0)
                    );
                    assert!(!ordinary.layers[0].mask.as_ref().unwrap().enabled);
                    assert_eq!(serde_json::to_value(&doc).unwrap(), before);
                }
            }
        }
    }
    #[test]
    fn compressed_source_spool_batches_actual_writes_without_changing_native_output() {
        use std::{cell::Cell, rc::Rc};
        struct PhysicalSpool {
            data: std::io::Cursor<Vec<u8>>,
            source: Rc<Cell<bool>>,
            writes: Vec<usize>,
        }
        impl Write for PhysicalSpool {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.source.get() {
                    self.writes.push(bytes.len());
                }
                self.data.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.data.flush()
            }
        }
        impl Read for PhysicalSpool {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.data.read(bytes)
            }
        }
        impl Seek for PhysicalSpool {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                self.data.seek(from)
            }
        }
        for depth in [8, 16] {
            let mut doc = streamed_fixture(depth);
            doc.width = 256;
            doc.height = 256;
            let pixels = &mut doc.layers[0].pixels;
            *pixels = Raster::new_depth(256, 256, depth);
            let mut seed = 0x5b37_19d1u32;
            for y in 0..256 {
                for x in 0..256 {
                    let pixel = std::array::from_fn(|_| {
                        seed ^= seed << 13;
                        seed ^= seed >> 17;
                        seed ^= seed << 5;
                        seed as u16
                    });
                    pixels.set16(x, y, pixel);
                }
            }
            pixels.set16(0, 0, [12347, 23459, 34571, 0]);
            let before = serde_json::to_value(&doc).unwrap();
            let png = doc.export_png().unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let source = Rc::new(Cell::new(false));
                let source_checks = Cell::new(0usize);
                let mut spool = PhysicalSpool {
                    data: std::io::Cursor::new(Vec::new()),
                    source: source.clone(),
                    writes: Vec::new(),
                };
                let mut output = Vec::new();
                encode_layout_progress_to(
                    &doc,
                    layout,
                    &mut output,
                    &mut spool,
                    &|| {
                        if source.get() {
                            source_checks.set(source_checks.get() + 1);
                        }
                        Ok(())
                    },
                    &|stage, _, _| {
                        source.set(stage == "Writing editable sources");
                        Ok(())
                    },
                )
                .unwrap();
                let mut file = Cursor::new(&output);
                file.bytes(26).unwrap();
                file.block().unwrap();
                let resources = file.block().unwrap();
                let length = u32::from_be_bytes(resources[20..24].try_into().unwrap()) as usize;
                let compressed = &resources[28..24 + length];
                let section = layout.block(&mut file).unwrap();
                let merged = &output[file.pos..];
                assert!(compressed.len() > 64 * 1024);
                assert!(spool.writes.len() >= 2);
                assert_eq!(spool.writes.iter().sum::<usize>(), compressed.len());
                assert!(spool.writes.iter().all(|n| *n > 0 && *n <= 64 * 1024));
                // The pinned compressor emits at most 32 KiB per write, including
                // a small zlib header. Physical writes coalesce those fragments.
                assert!(spool.writes[..spool.writes.len() - 1]
                    .iter()
                    .all(|n| *n >= 32 * 1024));
                assert_eq!(
                    spool.data.get_ref(),
                    &[section, merged, compressed].concat()
                );
                let original = Embedded {
                    format: 39,
                    standard_hash: hash(section) ^ hash(merged),
                    document: doc.clone(),
                };
                let input_checks = Cell::new(0usize);
                let count_input = || {
                    input_checks.set(input_checks.get() + 1);
                    Ok(())
                };
                let mut previous = BoundedWriter {
                    inner: ZlibEncoder::new(Vec::new(), Compression::fast()),
                    written: 0,
                    limit: if depth == 16 {
                        512 * 1024 * 1024
                    } else {
                        128 * 1024 * 1024
                    },
                    check: &count_input,
                    error: "Editable PSD source data exceeds its bounded serialization budget",
                };
                serde_json::to_writer(&mut previous, &original).unwrap();
                assert_eq!(compressed, previous.inner.finish().unwrap());
                let input_chunks = serde_json::to_vec(&original)
                    .unwrap()
                    .len()
                    .div_ceil(64 * 1024);
                assert!(source_checks.get() >= input_chunks);
                assert!(
                    source_checks.get() <= input_chunks + spool.writes.len() + 8,
                    "Source guards must cover bounded input and physical writes: {} source, {} chunks, {} physical writes",
                    source_checks.get(), input_chunks, spool.writes.len()
                );
                assert!(source_checks.get() < input_checks.get());
                let restored = decode(&output).unwrap();
                assert!(!restored.read_only);
                assert_eq!(serde_json::to_value(&restored).unwrap(), before);
                assert_eq!(restored.export_png().unwrap(), png);
                let mut standard_only = output[..30].to_vec();
                standard_only.extend([0; 4]);
                standard_only.extend(&output[34 + resources.len()..]);
                let standard = decode(&standard_only).unwrap();
                assert_eq!(standard.bit_depth, depth);
                assert_eq!(
                    standard.layers[0].pixels.rgba16(),
                    doc.layers[0].pixels.rgba16()
                );
                assert_eq!(standard.export_png().unwrap(), png);
                assert_eq!(serde_json::to_value(&doc).unwrap(), before);
            }
        }
    }

    #[test]
    fn appended_source_segment_is_bound_and_source_spool_failures_keep_native_input() {
        use std::cell::Cell;
        use std::rc::Rc;
        struct SourceSpool<'a> {
            data: std::io::Cursor<Vec<u8>>,
            source: Rc<Cell<bool>>,
            source_start: usize,
            first_source: bool,
            injected: bool,
            fault: &'static str,
            largest: usize,
            control: &'a crate::saving::Control,
        }
        impl Write for SourceSpool<'_> {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.largest = self.largest.max(bytes.len());
                if self.source.get() {
                    if !self.first_source {
                        assert_eq!(self.data.position(), self.source_start as u64);
                        self.first_source = true;
                    }
                    if self.fault == "write" {
                        self.injected = true;
                        return Err(std::io::Error::other("Injected source spool write failure"));
                    }
                    if self.fault == "cancel" && !self.injected {
                        self.injected = true;
                        assert!(self.control.cancel());
                    }
                }
                self.data.write(&bytes[..bytes.len().min(997)])
            }
            fn flush(&mut self) -> std::io::Result<()> {
                if self.source.get() && self.fault == "flush" {
                    self.injected = true;
                    return Err(std::io::Error::other("Injected source spool flush failure"));
                }
                Ok(())
            }
        }
        impl Read for SourceSpool<'_> {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let source = self.data.position() >= self.source_start as u64;
                let n = self.data.read(bytes)?;
                if source && self.fault == "corrupt" && n != 0 {
                    bytes[0] ^= 1;
                    self.injected = true;
                }
                Ok(n)
            }
        }
        impl Seek for SourceSpool<'_> {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                if self.source.get()
                    && self.fault == "truncate"
                    && matches!(from, SeekFrom::Start(at) if at == self.source_start as u64)
                {
                    self.data.get_mut().truncate(self.source_start);
                    self.injected = true;
                }
                self.data.seek(from)
            }
        }
        for depth in [8, 16] {
            let doc = streamed_fixture(depth);
            let before = serde_json::to_value(&doc).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let expected = encode_layout(&doc, layout).unwrap();
                let native = expected_standard(&doc, layout.large());
                let standard = [native.0.as_slice(), native.1.as_slice()].concat();
                let mut cursor = Cursor::new(&expected);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let resource = cursor.block().unwrap();
                let length = u32::from_be_bytes(resource[20..24].try_into().unwrap()) as usize;
                let compressed = &resource[28..24 + length];
                for fault in ["none", "write", "flush", "cancel", "corrupt", "truncate"] {
                    let control = crate::saving::Control::default();
                    control.begin(doc.id.clone(), doc.revision).unwrap();
                    let source = Rc::new(Cell::new(false));
                    let mut spool = SourceSpool {
                        data: std::io::Cursor::new(vec![]),
                        source: source.clone(),
                        source_start: standard.len(),
                        first_source: false,
                        injected: false,
                        fault,
                        largest: 0,
                        control: &control,
                    };
                    let mut output = Vec::new();
                    let result = encode_layout_progress_to(
                        &doc,
                        layout,
                        &mut output,
                        &mut spool,
                        &|| control.check(),
                        &|stage, _, _| {
                            if stage == "Writing editable sources" {
                                source.set(true);
                            }
                            Ok(())
                        },
                    );
                    assert!(source.get() && spool.first_source, "{fault}");
                    assert!(spool.largest <= 64 * 1024);
                    assert_eq!(&spool.data.get_ref()[..standard.len()], standard);
                    if fault == "none" {
                        result.unwrap();
                        assert_eq!(output, expected);
                        assert_eq!(&spool.data.get_ref()[standard.len()..], compressed);
                        assert_eq!(
                            serde_json::to_value(decode(&output).unwrap()).unwrap(),
                            before
                        );
                    } else {
                        let error = result.unwrap_err();
                        assert!(
                            spool.injected,
                            "The actual source segment must be reached: {fault}"
                        );
                        assert!(
                            error.contains(match fault {
                                "write" => "source spool write failure",
                                "flush" => "source spool flush failure",
                                "cancel" => "canceled",
                                "corrupt" => "spool changed",
                                "truncate" => "truncated",
                                _ => unreachable!(),
                            }),
                            "{fault}: {error}"
                        );
                        assert!(output.len() < expected.len());
                        if matches!(fault, "write" | "flush" | "cancel") {
                            assert!(output.is_empty());
                        }
                    }
                    assert_eq!(serde_json::to_value(&doc).unwrap(), before);
                }
            }
        }
        // Count preexisting standard bytes without allocating a near-limit file;
        // only the appended segment belongs to its independent hash.
        let mut bytes = Vec::new();
        let check = || Ok(());
        let mut source = StandardWriter::new(&mut bytes, &check);
        source.bytes = 256 * 1024 * 1024 - 3;
        source.write_all(&[17, 29, 43]).unwrap();
        assert_eq!(source.bytes, 256 * 1024 * 1024);
        let expected_hash = [17u8, 29, 43]
            .into_iter()
            .fold(0xcbf29ce484222325u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
            });
        assert_eq!(source.hash, expected_hash);
        assert!(source
            .write_all(&[59])
            .unwrap_err()
            .to_string()
            .contains("256 MiB"));
        drop(source);
        assert_eq!(bytes, [17, 29, 43]);
    }

    #[test]
    fn standard_transfer_is_bounded_and_spool_copy_rejects_corruption_and_interruption() {
        struct ShortSpool {
            data: std::io::Cursor<Vec<u8>>,
            changed: bool,
            largest: usize,
        }
        impl Write for ShortSpool {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.largest = self.largest.max(bytes.len());
                self.data.write(&bytes[..bytes.len().min(997)])
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Read for ShortSpool {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let n = self.data.read(bytes)?;
                if self.changed && n != 0 {
                    bytes[0] ^= 1;
                }
                Ok(n)
            }
        }
        impl Seek for ShortSpool {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                self.data.seek(from)
            }
        }
        for depth in [8, 16] {
            let mut doc = Document::new_depth(127, 257, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(5, 9, [12347, 23459, 34571, 45673]);
            for changed in [false, true] {
                let mut spool = ShortSpool {
                    data: std::io::Cursor::new(vec![]),
                    changed,
                    largest: 0,
                };
                let result =
                    encode_layout_to(&doc, Layout::Psb, &mut Vec::new(), &mut spool, &|| Ok(()));
                assert!(
                    spool.largest <= 64 * 1024,
                    "Only the bounded buffered transfer reaches the spool"
                );
                if changed {
                    assert!(result.unwrap_err().contains("spool changed"));
                } else {
                    result.unwrap();
                }
            }
            let count = std::cell::Cell::new(0);
            let mut spool = std::io::Cursor::new(vec![]);
            let error = encode_layout_to(&doc, Layout::Psd, &mut Vec::new(), &mut spool, &|| {
                count.set(count.get() + 1);
                if count.get() == 40 {
                    Err("Injected row cancellation".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert!(error.contains("Injected row cancellation"));
            assert_eq!(count.get(), 40);
            let mut occupied = std::io::Cursor::new(b"retain caller data".to_vec());
            assert!(encode_layout_to(
                &doc,
                Layout::Psd,
                &mut Vec::new(),
                &mut occupied,
                &|| Ok(())
            )
            .unwrap_err()
            .contains("must be empty"));
            assert_eq!(occupied.into_inner(), b"retain caller data");
        }
        let mut short = std::io::Cursor::new(vec![1, 2]);
        let error = copy_standard(
            &mut short,
            &mut Vec::new(),
            3,
            hash(&[1, 2, 3]),
            &mut [0; 2],
            &|| Ok(()),
        )
        .unwrap_err();
        assert!(error.contains("truncated"));
        let calls = std::cell::Cell::new(0);
        let error = copy_standard(
            &mut std::io::Cursor::new(vec![1, 2, 3, 4]),
            &mut Vec::new(),
            4,
            hash(&[1, 2, 3, 4]),
            &mut [0; 2],
            &|| {
                calls.set(calls.get() + 1);
                if calls.get() == 2 {
                    Err("Injected copy cancellation".into())
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error, "Injected copy cancellation");
    }
    #[test]
    fn streamed_channel_and_final_output_failures_never_return_a_complete_file() {
        struct Failing {
            data: std::io::Cursor<Vec<u8>>,
            left: usize,
        }
        impl Write for Failing {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.left == 0 {
                    return Err(std::io::Error::other("Injected disk write failure"));
                }
                let n = bytes.len().min(self.left);
                self.left -= n;
                self.data.write(&bytes[..n])
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Read for Failing {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                self.data.read(bytes)
            }
        }
        impl Seek for Failing {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                self.data.seek(from)
            }
        }
        for depth in [8, 16] {
            let doc = streamed_fixture(depth);
            let original = serde_json::to_value(&doc).unwrap();
            for layout in [Layout::Psd, Layout::Psb] {
                let mut failing = Failing {
                    data: std::io::Cursor::new(vec![]),
                    left: 7,
                };
                let error =
                    encode_layout_to(&doc, layout, &mut Vec::new(), &mut failing, &|| Ok(()))
                        .unwrap_err();
                assert!(error.contains("Injected disk write failure"));
                let mut failing = Failing {
                    data: std::io::Cursor::new(vec![]),
                    left: 37,
                };
                let error = encode_layout_to(
                    &doc,
                    layout,
                    &mut failing,
                    &mut std::io::Cursor::new(vec![]),
                    &|| Ok(()),
                )
                .unwrap_err();
                assert!(error.contains("Injected disk write failure"));
                assert_eq!(serde_json::to_value(&doc).unwrap(), original);
            }
        }
    }
    #[test]
    fn output_preflight_counts_cropped_sources_masks_and_the_actual_standard_plan() {
        let patterns = patterns::StandardPatterns::default();
        let mut doc = Document::new_depth(4, 4, 16).unwrap();
        doc.layers[0].pixels = Raster::new_depth(8192, 3277, 16);
        validate(&doc).unwrap();
        check_output_size(&doc, &standard_plan(&doc), &patterns).unwrap();
        doc.layers[0].mask = Some(Mask {
            enabled: true,
            linked: false,
            cache_key: id(),
            steps: vec![MaskStep {
                id: id(),
                kind: "paint".into(),
                enabled: true,
                weight: 1.,
                value: 255.,
                pixels: Raster::new_depth(8192, 3277, 16),
                settings: serde_json::json!({}),
            }],
        });
        validate(&doc).unwrap();
        assert!(check_output_size(&doc, &standard_plan(&doc), &patterns)
            .unwrap_err()
            .contains("Uncompressed Photoshop channels alone"));
        doc.layers[0].mask = None;
        doc.layers[0].pixels = Raster::new_depth(8192, 4096, 16);
        assert!(check_output_size(&doc, &standard_plan(&doc), &patterns).is_err());

        // An adjustment-only standard bake stores the tiny current canvas;
        // the large sparse editable sources remain in supplementary data.
        let mut adjustment = Layer::new("Adjustment", "adjustment", 4, 4);
        adjustment.pixels = Raster::new_depth(4, 4, 16);
        doc.layers.insert(0, adjustment);
        validate(&doc).unwrap();
        check_output_size(&doc, &standard_plan(&doc), &patterns).unwrap();
        let bytes = encode(&doc).unwrap();
        assert_eq!(decode(&bytes).unwrap().layers[1].pixels.width, 8192);
        doc.filters.push(crate::effects::Effect {
            id: id(),
            kind: "invert".into(),
            enabled: false,
            weight: 1.,
            settings: serde_json::json!({}).into(),
        });
        validate(&doc).unwrap();
        // Whole-document filters additionally retain standard source records.
        assert!(check_output_size(&doc, &standard_plan(&doc), &patterns).is_err());

        let mut empty = Document::new_depth(8192, 4096, 8).unwrap();
        empty.layers.clear();
        assert!(
            check_output_size(&empty, &standard_plan(&empty), &patterns).is_err(),
            "Empty documents still write a full Canvas raster and composite"
        );
    }
    #[test]
    fn output_record_preflight_counts_group_dividers_and_a_filtered_bake() {
        let mut doc = Document::new(4, 4).unwrap();
        doc.layers = (0..100)
            .map(|_| Layer::new("Folder", "group", 4, 4))
            .collect();
        validate(&doc).unwrap();
        check_output_size(&doc, &standard_plan(&doc), &patterns::StandardPatterns::default()).unwrap();
        doc.filters.push(crate::effects::Effect {
            id: id(),
            kind: "invert".into(),
            enabled: false,
            weight: 1.,
            settings: serde_json::json!({}).into(),
        });
        validate(&doc).unwrap();
        assert_eq!(encode(&doc).unwrap_err(), "Too many PSD layer records");
        assert_eq!(encode_psb(&doc).unwrap_err(), "Too many PSD layer records");
    }
    #[test]
    fn style_gate_compares_exact_standard_matte_and_alpha_without_straight_rgb_tolerance() {
        let mut saved = Raster::new_depth(1, 1, 8);
        for alpha in 0u32..=255 {
            for color in 0u32..=255 {
                let white = (color * alpha + 255 * (255 - alpha) + 127) / 255;
                let recovered = if alpha == 0 {
                    0
                } else {
                    ((white + alpha - 255) * 255 + alpha / 2) / alpha
                };
                saved.set16(
                    0,
                    0,
                    [
                        (recovered * 257) as u16,
                        (recovered * 257) as u16,
                        (recovered * 257) as u16,
                        (alpha * 257) as u16,
                    ],
                );
                assert!(same_standard_composite(
                    &[color as u8, color as u8, color as u8, alpha as u8],
                    &saved
                ));
                let wrong_alpha = if alpha == 255 { 254 } else { alpha + 1 };
                assert!(!same_standard_composite(
                    &[color as u8, color as u8, color as u8, wrong_alpha as u8],
                    &saved
                ));
            }
        }
        let mut saved = Raster::new_depth(1, 1, 16);
        saved.set16(0, 0, [12347, 23459, 34571, 65535]);
        assert!(same_standard_composite(
            &[12347u16, 23459, 34571, 65535],
            &saved
        ));
        assert!(!same_standard_composite(
            &[12348u16, 23459, 34571, 65535],
            &saved
        ));
        assert!(!same_standard_composite(
            &[12347u16, 23459, 34571, 65534],
            &saved
        ));
    }
    fn record() -> Record {
        Record {
            source: SourceLayerInfo {
                source_index: 0,
                parent_index: None,
                name: "Thin source".into(),
                kind: "paint".into(),
                bounds: [0, 0, 8192, 1],
                visible: true,
                has_mask: false,
                tags: vec![],
                warnings: vec![],
                details_truncated: false,
            },
            layer: Layer::new("Thin source", "paint", 8192, 1),
            channels: vec![(0, 2)],
            bounds: [0, 0, 1, 8192],
            mask_bounds: None,
            mask_pair: None,
            mask_default: 255,
            mask_density: None,
            mask_parameters: false,
            mask_enabled: true,
            mask_linked: true,
            retain_mask_original: false,
            section: 0,
            clipped: false,
        }
    }
    #[test]
    fn tile_budget_counts_padding_masks_and_skips_empty_records_before_decoding() {
        let mut records = (0..32).map(|_| record()).collect::<Vec<_>>();
        decoded_storage(&records, 8192, 1, 16).unwrap();
        for rec in &mut records {
            rec.bounds[2] = 256;
        }
        // A complete tile row costs exactly as much as a one-pixel-high row.
        decoded_storage(&records, 8192, 256, 16).unwrap();
        for rec in &mut records {
            rec.bounds[2] = 1;
        }
        records.push(record());
        assert!(decoded_storage(&records, 8192, 1, 16)
            .unwrap_err()
            .contains("budget"));
        records.pop();
        records[0].mask_bounds = Some([0, 0, 1, 1]);
        records[0].channels.push((-2, 2));
        assert!(decoded_storage(&records, 8192, 1, 16).is_err());
        for rec in &mut records {
            rec.channels.clear();
        }
        decoded_storage(&records, 8192, 1, 16).unwrap();
        assert!(records
            .iter()
            .all(|r| r.layer.pixels.tiles.is_empty() && r.layer.pixels.samples16.is_empty()));
    }
}

#[cfg(test)]
mod mask_rows_tests;
#[cfg(test)]
pub(crate) use mask_rows_tests::{native_gradient_grid_fixture, native_stroke_fixture};

#[cfg(test)]
mod source_stream_tests;

#[cfg(test)]
pub(crate) use source_stream_tests::observe_phase as observe_open_phase;
