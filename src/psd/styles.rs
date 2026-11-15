//! Bounded object-effects inspection and conservative overlay import.
//! Descriptor recognition alone never authorizes editing; the caller verifies the native composite.
//! Binary layout: https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/
use super::Cursor;
use serde_json::{json, Map, Value};
use std::io::{self, Write};

const MAX_DESCRIPTOR_BYTES: usize = 1024 * 1024;
const MAX_DESCRIPTOR_NODES: usize = 4096;
// A full 32-effect stack can pair sixteen maximal gradients with sixteen hard
// Inner Shadows and their contour descriptors.
const MAX_STYLE_DESCRIPTOR_NODES: usize = 4864;

pub(super) fn native_working_bytes(bytes: u64) -> Result<u64, String> {
    if bytes > MAX_DESCRIPTOR_BYTES as u64 {
        return Err("Photoshop layer style descriptor exceeds 1 MiB".into());
    }
    // A typed value takes at least five encoded bytes. Allow for the parsed
    // objects, their isolated working copy and one layer's normalized settings.
    let nodes = (bytes / 5 + 1).min(MAX_STYLE_DESCRIPTOR_NODES as u64);
    Ok(nodes * 8192 + bytes * 4 + 65536)
}

struct Reader<'a> {
    cursor: Cursor<'a>,
    nodes: usize,
    max_nodes: usize,
    blob_offsets: bool,
}
impl Reader<'_> {
    fn text(&mut self) -> Result<String, String> {
        let count = self.cursor.u32()? as usize;
        if count > 4096 {
            return Err("descriptor text exceeds 4096 code units".into());
        }
        let bytes = self.cursor.bytes(count * 2)?;
        let words = bytes
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        String::from_utf16(&words).map_err(|_| "invalid descriptor Unicode".into())
    }
    fn id(&mut self) -> Result<String, String> {
        self.id_with_form().map(|(name, _)| name)
    }
    fn id_with_form(&mut self) -> Result<(String, bool), String> {
        let count = self.cursor.u32()? as usize;
        let string_id = count != 0;
        let count = if count == 0 { 4 } else { count };
        if count > 128 {
            return Err("descriptor identifier exceeds 128 bytes".into());
        }
        let bytes = self.cursor.bytes(count)?;
        if !bytes.iter().all(|c| (32..=126).contains(c)) {
            return Err("invalid descriptor identifier".into());
        }
        Ok((std::str::from_utf8(bytes).unwrap().into(), string_id))
    }
    fn number(&mut self) -> Result<f64, String> {
        let n = f64::from_be_bytes(self.cursor.bytes(8)?.try_into().unwrap());
        if !n.is_finite() {
            return Err("non-finite descriptor number".into());
        }
        Ok(n)
    }
    fn node(&mut self, depth: usize) -> Result<(), String> {
        self.nodes += 1;
        if depth > 32 || self.nodes > self.max_nodes {
            return Err("descriptor exceeds its depth or item budget".into());
        }
        Ok(())
    }
    fn descriptor(&mut self, depth: usize) -> Result<Value, String> {
        self.node(depth)?;
        // Names and opaque data are validated but not exposed in diagnostics.
        self.text()?;
        let class = self.id()?;
        let count = self.cursor.u32()? as usize;
        if count > 256 {
            return Err("descriptor exceeds 256 properties".into());
        }
        let mut items = Map::new();
        for _ in 0..count {
            let key = self.id()?;
            if items.contains_key(&key) {
                return Err("duplicate descriptor property".into());
            }
            let value = self.value(depth + 1)?;
            items.insert(key, value);
        }
        Ok(json!({"class":class,"items":items}))
    }
    fn value(&mut self, depth: usize) -> Result<Value, String> {
        self.node(depth)?;
        let kind = self.cursor.bytes(4)?;
        let value = match kind {
            b"Objc" | b"GlbO" => self.descriptor(depth)?,
            b"VlLs" => {
                let count = self.cursor.u32()? as usize;
                if count > 256 {
                    return Err("descriptor list exceeds 256 items".into());
                }
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(self.value(depth + 1)?);
                }
                json!(values)
            }
            b"bool" => match self.cursor.u8()? {
                0 => json!(false),
                1 => json!(true),
                _ => return Err("invalid descriptor boolean".into()),
            },
            b"long" => json!(self.cursor.i32()?),
            b"comp" => json!(i64::from_be_bytes(
                self.cursor.bytes(8)?.try_into().unwrap()
            )),
            b"doub" => json!(self.number()?),
            b"UntF" => {
                let unit = self.cursor.bytes(4)?;
                if !unit.iter().all(|c| (32..=126).contains(c)) {
                    return Err("invalid descriptor unit".into());
                }
                json!([std::str::from_utf8(unit).unwrap(), self.number()?])
            }
            b"enum" => {
                let class = self.id()?;
                let (name, string_id) = self.id_with_form()?;
                // Four-character string IDs are not Photoshop char IDs. In
                // particular, a string "Mltp" must not become Multiply merely
                // because the bytes look like that legacy char ID.
                return Ok(json!({"type":"enum", "value":[class,name], "string_id":string_id}));
            }
            // Retained internally for bounded pattern-ID resolution; never exposed by inspect.
            b"TEXT" => json!(self.text()?),
            b"type" | b"GlbC" => {
                self.text()?;
                json!(self.id()?)
            }
            b"tdta" | b"alis" => {
                let count = self.cursor.u32()? as usize;
                let offset = self.cursor.pos;
                self.cursor.bytes(count)?;
                if self.blob_offsets && kind == b"tdta" {
                    json!({"bytes":count,"offset":offset})
                } else {
                    json!({"bytes":count})
                }
            }
            _ => return Err("unsupported descriptor value type".into()),
        };
        Ok(json!({"type":std::str::from_utf8(kind).unwrap(),"value":value}))
    }
}
fn boolean(value: &Value) -> Option<bool> {
    (value["type"] == "bool")
        .then(|| value["value"].as_bool())
        .flatten()
}
fn style_name(key: &str) -> Option<&'static str> {
    Some(match key {
        "DrSh" | "dropShadowMulti" => "Drop Shadow",
        "IrSh" | "innerShadowMulti" => "Inner Shadow",
        "OrGl" => "Outer Glow",
        "IrGl" => "Inner Glow",
        "SoFi" | "solidFillMulti" => "Color Overlay",
        "GrFl" | "gradientFillMulti" => "Gradient Overlay",
        "patternFill" => "Pattern Overlay",
        "FrFX" | "frameFXMulti" => "Stroke",
        "ebbl" => "Bevel & Emboss",
        "ChFX" => "Satin",
        _ => return None,
    })
}
fn root(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > MAX_DESCRIPTOR_BYTES {
        return Err("effects descriptor exceeds 1 MiB".into());
    }
    if bytes.len() < 4 || bytes[..4] != [0; 4] {
        return Err("unsupported effects descriptor version".into());
    }
    read_descriptor_block(&bytes[4..], false, MAX_DESCRIPTOR_BYTES, MAX_STYLE_DESCRIPTOR_NODES, None)
}
pub(super) fn descriptor_block(bytes: &[u8]) -> Result<Value, String> {
    read_descriptor_block(bytes, false, MAX_DESCRIPTOR_BYTES, MAX_DESCRIPTOR_NODES, None)
}

pub(super) fn descriptor_block_with_zero_trailer(bytes: &[u8], count: usize) -> Result<Value, String> {
    read_descriptor_block(bytes, false, MAX_DESCRIPTOR_BYTES, MAX_DESCRIPTOR_NODES, Some(count))
}

pub(super) fn descriptor_blob_offsets(bytes: &[u8], limit: usize) -> Result<Value, String> {
    read_descriptor_block(bytes, true, limit, MAX_DESCRIPTOR_NODES, None)
}

fn read_descriptor_block(
    bytes: &[u8], blob_offsets: bool, limit: usize, max_nodes: usize, zero_trailer: Option<usize>,
) -> Result<Value, String> {
    if bytes.len() > limit {
        return Err("Photoshop descriptor exceeds its size limit".into());
    }
    let mut reader = Reader {
        cursor: Cursor::new(bytes),
        nodes: 0,
        max_nodes,
        blob_offsets,
    };
    if reader.cursor.u32()? != 16 {
        return Err("unsupported effects descriptor version".into());
    }
    let root = reader.descriptor(0)?;
    let remaining = &bytes[reader.cursor.pos..];
    if (remaining.len() > 3 && zero_trailer != Some(remaining.len()))
        || remaining.iter().any(|v| *v != 0)
    {
        return Err("unrecognized effects descriptor trailer".into());
    }
    Ok(root)
}
/// Summary only: no native effect stack, editable IDs or decoded source pixels are created.
pub(super) fn inspect(bytes: &[u8]) -> Result<Vec<String>, String> {
    let root = root(bytes)?;
    let items = root["items"].as_object().unwrap();
    let mut summaries = vec![];
    let master = items.get("masterFXSwitch").and_then(boolean);
    if items.contains_key("masterFXSwitch") && master.is_none() {
        return Err("invalid style master switch".into());
    }
    summaries.push(format!(
        "Photoshop style master switch: {}",
        match master {
            Some(true) => "enabled",
            Some(false) => "disabled",
            None => "not specified",
        }
    ));
    for (key, value) in items {
        if matches!(key.as_str(), "Scl " | "masterFXSwitch") {
            continue;
        }
        if let Some(name) = style_name(key) {
            let entries = if value["type"] == "VlLs" {
                value["value"]
                    .as_array()
                    .ok_or("invalid effects list")?
                    .iter()
                    .collect::<Vec<_>>()
            } else {
                vec![value]
            };
            let mut enabled = 0;
            let mut disabled = 0;
            let mut unknown = 0;
            let mut present = 0;
            for entry in &entries {
                if !matches!(entry["type"].as_str(), Some("Objc" | "GlbO")) {
                    return Err("style entry must be a descriptor".into());
                }
                match boolean(&entry["value"]["items"]["enab"]) {
                    Some(true) => enabled += 1,
                    Some(false) => disabled += 1,
                    None => unknown += 1,
                }
                if boolean(&entry["value"]["items"]["present"]) == Some(true) {
                    present += 1;
                }
            }
            summaries.push(format!("Photoshop {name}: {} definitions; {enabled} enabled, {disabled} disabled, {unknown} unspecified; {present} marked present",entries.len()));
        } else {
            // Bounded printable keys only, never descriptor names, paths or blob contents.
            summaries.push(format!("Unrecognized Photoshop style property: {key}"));
        }
        if summaries.len() == 63 {
            summaries.push("Additional Photoshop style diagnostics omitted".into());
            break;
        }
    }
    Ok(summaries)
}

fn plain_number(value: &Value) -> Result<f64, String> {
    if !matches!(value["type"].as_str(), Some("doub" | "long")) {
        return Err("Photoshop style requires plain channel/coordinate numbers".into());
    }
    value["value"]
        .as_f64()
        .ok_or("Invalid Photoshop style number".into())
}
fn percent(value: &Value) -> Result<f64, String> {
    if value["type"] != "UntF" || value["value"][0] != "#Prc" {
        return Err("Photoshop style requires percentage units".into());
    }
    let n = value["value"][1]
        .as_f64()
        .ok_or("Invalid Photoshop style percentage")?;
    if !(0.0..=100.).contains(&n) {
        return Err("Photoshop style percentage is outside 0–100".into());
    }
    Ok(n / 100.)
}
fn bounded_percent(value: &Value, min: f64, max: f64) -> Result<f64, String> {
    if value["type"] != "UntF" || value["value"][0] != "#Prc" {
        return Err("Photoshop gradient placement requires percentage units".into());
    }
    value["value"][1]
        .as_f64()
        .filter(|n| n.is_finite() && (min..=max).contains(n))
        .map(|n| n / 100.)
        .ok_or_else(|| format!("Photoshop gradient percentage must be {min}–{max}"))
}
const MAX_COLOR_OVERLAYS: usize = 16;
const MAX_GRADIENT_OVERLAYS: usize = 16;
const MAX_INNER_SHADOWS: usize = 16;
const MAX_INSIDE_STROKES: usize = 16;
const MAX_LAYER_EFFECTS: usize = 32;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct StyleEffect {
    color: [f64; 3],
    opacity: f64,
    blend_id: &'static str,
    enabled: bool,
    shadow: Option<HardShadow>,
    stroke: Option<SolidStroke>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SolidStroke {
    position: [u8; 4],
    size: f64,
}

impl StyleEffect {
    fn key(self) -> [u8; 4] {
        if self.stroke.is_some() { *b"FrFX" }
        else { self.shadow.map_or(*b"SoFi", HardShadow::key) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct HardShadow {
    light_angle: f64,
    distance: f64,
    global: bool,
    knockout: Option<bool>,
}

impl HardShadow {
    fn key(self) -> [u8; 4] {
        if self.knockout.is_some() { *b"DrSh" } else { *b"IrSh" }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StandardGradient<'a> {
    settings: &'a Value,
    blend_id: &'static str,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct StandardPattern<'a> {
    png: &'a str,
    id: Option<super::patterns::PatternId>,
    placement: super::patterns::Placement,
    opacity: f64,
    blend_id: &'static str,
    enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StandardStyles<'a> {
    pattern: Option<StandardPattern<'a>>,
    gradients: [Option<StandardGradient<'a>>; MAX_GRADIENT_OVERLAYS],
    effects: [StyleEffect; MAX_LAYER_EFFECTS],
    count: usize,
    overlays: usize,
    inner_shadows: usize,
    strokes: usize,
    pub(super) mask_linked: Option<bool>,
    pub(super) layer_blend: [u8; 4],
    pub(super) layer_opacity: u8,
    interior_effects: Option<bool>,
    pub(super) fill: bool,
    pub(super) bounds: [i32; 4],
}

impl<'a> StandardStyles<'a> {
    pub(super) fn pattern_source(self) -> Option<&'a str> {
        self.pattern.map(|pattern| pattern.png)
    }

    pub(super) fn bind_patterns(mut self, patterns: &super::patterns::StandardPatterns) -> Option<Self> {
        if let Some(pattern) = &mut self.pattern {
            pattern.id = Some(patterns.id(pattern.png)?);
        }
        Some(self)
    }

    pub(super) fn tag(&self) -> [u8; 4] {
        if self.overlays > 1 || self.inner_shadows > 1 || self.strokes > 1 || self.gradients[1].is_some() { *b"lmfx" } else { *b"lfx2" }
    }

    pub(super) fn source_format(&self) -> u32 {
        let effect = self.effects[0];
        if self.strokes > 1 {
            super::MULTI_INSIDE_STROKE_FORMAT
        } else if self.inner_shadows > 1 {
            super::MULTI_INNER_SHADOW_FORMAT
        } else if self.effects[..self.count].iter().any(|effect| effect.shadow.is_some_and(|shadow| shadow.global)) {
            super::GLOBAL_LIGHT_STYLE_FORMAT
        } else if self.pattern.is_some_and(|pattern| pattern.placement != super::patterns::Placement::default()) {
            super::PLACED_PATTERN_OVERLAY_FORMAT
        } else if self.gradients[1].is_some() {
            super::MULTI_GRADIENT_OVERLAY_FORMAT
        } else if self.pattern.is_some() {
            super::PATTERN_OVERLAY_FORMAT
        } else if self.gradients[0].is_some() {
            super::GRADIENT_OVERLAY_FORMAT
        } else if self.effects[..self.count].iter().any(|effect| effect.stroke.is_some()) {
            super::STROKE_STYLE_FORMAT
        } else if self.count > self.overlays {
            if self.count == 1 { super::HARD_SHADOW_FORMAT } else { super::SHADOW_STACK_FORMAT }
        } else if self.count > 1 {
            super::MULTI_COLOR_OVERLAY_FORMAT
        } else if !self.fill {
            super::RASTER_COLOR_OVERLAY_FORMAT
        } else if self.layer_blend != *b"norm" || self.layer_opacity != 255 || self.interior_effects.is_some() {
            super::COLOR_OVERLAY_LAYER_FORMAT
        } else if !effect.enabled || self.mask_linked == Some(false) {
            super::COLOR_OVERLAY_STATE_FORMAT
        } else if self.mask_linked.is_some() {
            super::MASKED_COLOR_OVERLAY_FORMAT
        } else if effect.blend_id != "Nrml" {
            super::BLENDED_COLOR_OVERLAY_FORMAT
        } else if effect.opacity == 100. && effect.color.iter().all(|value| value.fract() == 0.) {
            super::COLOR_OVERLAY_FORMAT
        } else {
            super::PRECISE_COLOR_OVERLAY_FORMAT
        }
    }
}

pub(super) fn standard_styles(
    layer: crate::engine::view::LayerView<'_>, width: u32, height: u32,
    global_angle: Option<u32>,
) -> Option<StandardStyles<'_>> {
    use crate::engine::view::LayerView;
    let metadata = match layer {
        LayerView::Resident(layer) => &layer.psd_metadata[..],
        LayerView::Native(layer) => layer.psd_metadata(),
    };
    let interior_effects = match metadata.iter().find(|entry| entry.key == "infx") {
        Some(entry) => match entry.data.as_slice() {
            [flag @ (0 | 1), 0, 0, 0] => Some(*flag == 1),
            _ => return None,
        },
        None => None,
    };
    let layer_blend = super::scene_save36::blend(layer.blend()).ok()?;
    let layer_opacity = (layer.opacity() * 255.).round() as u8;
    let fill = layer.kind() == "fill";
    let pixels = layer.pixels();
    let [x, y] = layer.origin();
    let count = layer.effect_count();
    if !matches!(layer.kind(), "fill" | "paint") || layer.source().is_some()
        || fill && (layer.color()[3] != 255 || [x, y] != [0, 0] || pixels.width != width || pixels.height != height)
        || layer.clip_to().is_some() || layer_blend == *b"pass"
        || layer_blend != *b"norm" && interior_effects != Some(true)
        || f32::from(layer_opacity) / 255. != layer.opacity() || !(1..=MAX_LAYER_EFFECTS).contains(&count)
        || metadata.iter().any(|entry| !matches!(entry.key.as_str(), "lyid" | "lclr" | "infx"))
    { return None; }
    let first = layer.effect(0)?;
    let pattern = if first.kind() == "pattern_overlay" {
        Some(standard_pattern(first)?)
    } else {
        None
    };
    let mut offset = usize::from(pattern.is_some());
    let mut gradients = [None; MAX_GRADIENT_OVERLAYS];
    for entry in &mut gradients {
        let Some(effect) = layer.effect(offset).filter(|effect| effect.kind() == "gradient_overlay") else {
            break;
        };
        let settings = effect.settings();
        if effect.weight() != 1. || !super::gradient_fill::overlay_representable(settings) {
            return None;
        }
        *entry = Some(StandardGradient {
            settings,
            blend_id: standard_style_blend(settings, "normal")?,
            enabled: effect.enabled(),
        });
        offset += 1;
    }
    if fill && layer_blend != *b"norm" && (pattern.is_some() || gradients[0].is_some()) {
        return None;
    }
    let count = count - offset;
    let mut effects = [StyleEffect::default(); MAX_LAYER_EFFECTS];
    let mut overlays = 0;
    let mut inner_shadows = 0;
    let mut strokes = 0;
    let mut previous = 0;
    for (index, entry) in effects[..count].iter_mut().enumerate() {
        let effect = layer.effect(index + offset)?;
        let (order, default_blend) = match effect.kind() {
            "color_overlay" => (0, "normal"),
            "inner_shadow" => (1, "multiply"),
            "stroke" => (2, "normal"),
            "drop_shadow" => (3, "normal"),
            _ => return None,
        };
        if order < previous || order == 3 && order == previous { return None; }
        previous = order;
        *entry = standard_style_effect(effect, default_blend)?;
        if order == 0 {
            if overlays == MAX_COLOR_OVERLAYS || effect.settings().as_object()?.keys()
                .any(|key| !matches!(key.as_str(), "color" | "blend" | "opacity"))
            { return None; }
            overlays += 1;
        } else {
            if fill || layer.mask().is_some() || layer_blend != *b"norm" { return None; }
            if order == 2 {
                entry.stroke = Some(standard_solid_stroke(effect)?);
                strokes += 1;
                if strokes > MAX_INSIDE_STROKES { return None; }
            }
            else {
                entry.shadow = Some(standard_hard_shadow(effect, global_angle)?);
                if order == 1 {
                    inner_shadows += 1;
                    if inner_shadows > MAX_INNER_SHADOWS { return None; }
                }
            }
        }
    }
    if strokes > 1 && effects[..count].iter().any(|effect| {
        effect.stroke.is_some_and(|stroke| stroke.position != *b"InsF")
    }) {
        return None;
    }
    // Exterior strokes change the casting alpha; Photoshop shadows use original alpha.
    let active = |effect: &StyleEffect| effect.enabled && effect.opacity != 0.;
    if effects[..count].iter().any(|effect| active(effect)
        && effect.stroke.is_some_and(|stroke| stroke.position != *b"InsF" && stroke.size != 0.))
        && effects[..count].iter().any(|effect| active(effect)
            && effect.shadow.is_some_and(|shadow| shadow.knockout.is_some()))
    { return None; }
    Some(StandardStyles {
        pattern, gradients, effects, count, overlays, inner_shadows, strokes, mask_linked: layer.mask().map(|mask| mask.linked()),
        layer_blend, layer_opacity, interior_effects, fill,
        bounds: [x, y, x.checked_add(i32::try_from(pixels.width).ok()?)?, y.checked_add(i32::try_from(pixels.height).ok()?)?],
    })
}

fn standard_style_blend(settings: &Value, default_blend: &str) -> Option<&'static str> {
    let blend = match settings.get("blend") {
        Some(value) => value.as_str()?,
        None => default_blend,
    };
    let (_, blend_id, _) = OVERLAY_BLEND_IDS.iter().find(|(mode, _, _)| *mode == blend)?;
    Some(blend_id)
}

fn standard_style_effect(effect: crate::engine::view::EffectView<'_>, default_blend: &str) -> Option<StyleEffect> {
    let settings = effect.settings();
    if effect.weight() != 1. { return None; }
    let blend_id = standard_style_blend(settings, default_blend)?;
    let opacity = standard_style_unit(settings["opacity"].as_f64()?, 100.)?;
    let rgb = settings["color"].as_array()?;
    if rgb.len() != 3 { return None; }
    let mut color = [0.; 3];
    for (out, value) in color.iter_mut().zip(rgb) {
        *out = standard_style_unit(value.as_f64()?, 255.)?;
    }
    Some(StyleEffect { color, opacity, blend_id, enabled: effect.enabled(), shadow: None, stroke: None })
}

fn standard_style_unit(value: f64, scale: f64) -> Option<f64> {
    if !(0. ..=1.).contains(&value) { return None; }
    let scaled = value * scale;
    let rounded = scaled.round();
    if value == 0. { Some(0.) }
    else if rounded / scale == value { Some(rounded) }
    else { (scaled / scale == value).then_some(scaled) }
}

fn standard_pattern(effect: crate::engine::view::EffectView<'_>) -> Option<StandardPattern<'_>> {
    let settings = effect.settings();
    let number = |key, default| settings.get(key).map_or(Some(default), Value::as_f64);
    if effect.weight() != 1. || settings["pattern"] != "bitmap"
        || settings["sampling"] != "nearest" || settings["use_pattern_alpha"] == true
        || number("bitmap_scale", 1.) != Some(1.)
        || settings.as_object()?.keys().any(|key| !matches!(key.as_str(),
            "pattern" | "bitmap_png" | "bitmap_scale" | "bitmap_angle" | "phase_x" | "phase_y"
            | "sampling" | "opacity" | "blend" | "use_pattern_alpha" | "color" | "color2" | "scale" | "invert"))
    {
        return None;
    }
    Some(StandardPattern {
        png: settings["bitmap_png"].as_str()?,
        id: None,
        placement: super::patterns::Placement::from_settings(settings)?,
        opacity: standard_style_unit(number("opacity", 0.75)?, 100.)?,
        blend_id: standard_style_blend(settings, "normal")?,
        enabled: effect.enabled(),
    })
}

fn standard_solid_stroke(effect: crate::engine::view::EffectView<'_>) -> Option<SolidStroke> {
    let settings = effect.settings();
    if settings.as_object()?.keys().any(|key| !matches!(key.as_str(),
        "color" | "blend" | "opacity" | "position" | "size"))
        || settings.get("blend").is_some_and(|value| value != "normal")
    { return None; }
    let position = match settings["position"].as_str()? {
        "inside" => *b"InsF", "center" => *b"CtrF", "outside" => *b"OutF", _ => return None,
    };
    let size = settings["size"].as_f64()?;
    if !(0. ..=crate::effects::styles::MAX_STROKE_SIZE).contains(&size) || size.fract() != 0. { return None; }
    Some(SolidStroke { position, size })
}

fn standard_hard_shadow(effect: crate::engine::view::EffectView<'_>, global_angle: Option<u32>) -> Option<HardShadow> {
    let drop_shadow = effect.kind() == "drop_shadow";
    let settings = effect.settings();
    if !matches!(effect.kind(), "inner_shadow" | "drop_shadow")
        || settings.as_object()?.keys().any(|key| !matches!(key.as_str(),
            "color" | "blend" | "opacity" | "angle" | "distance" | "size" | "spread" | "contour" | "pixel_snap" | "global_light")
            && !(drop_shadow && key == "source_knockout"))
        || settings["size"] != 0. || settings.get("spread").is_some_and(|value| value != 0.)
        || settings.get("contour").is_some_and(|value| value != "linear")
        || drop_shadow && settings.get("blend").is_some_and(|value| value != "normal")
    { return None; }
    let angle = settings["angle"].as_f64()?;
    let distance = settings["distance"].as_f64()?;
    if !(-180. ..180.).contains(&angle) || angle.fract() != 0.
        || !(0. ..=128.).contains(&distance) || distance.fract() != 0.
        || ![-180., -90., 0., 90.].contains(&angle) && settings["pixel_snap"] != true
    { return None; }
    let knockout = if drop_shadow {
        Some(match settings.get("source_knockout") {
            Some(value) => value.as_bool()?,
            None => false,
        })
    } else { None };
    let global = settings.get("global_light").map(|value| {
        crate::effects::styles::GlobalLight::from_value(value)
            .filter(|light| global_angle == Some(light.angle) && light.is_linked(settings))
            .ok_or(())
    }).transpose().ok()?;
    Some(HardShadow {
        light_angle: global.map_or(if angle < 0. { angle + 180. } else { angle - 180. }, |light| light.local_angle),
        distance,
        knockout,
        global: global.is_some(),
    })
}

const COLOR_OVERLAY_ROOT: &[u8] = b"\0\0\0\0\0\0\0\x10\0\0\0\0\0\0\0\0null";
const COLOR_OVERLAY_MASTER: &[u8] = b"\0\0\0\x0emasterFXSwitchbool\x01";
const COLOR_OVERLAY_COUNT: &[u8] = b"\0\0\0\x0enumModifyingFXlong";
const COLOR_OVERLAY_LIST: &[u8] = b"\0\0\0\x0esolidFillMultiVlLs";
const GRADIENT_OVERLAY_LIST: &[u8] = b"\0\0\0\x11gradientFillMultiVlLs";
const INNER_SHADOW_LIST: &[u8] = b"\0\0\0\x10innerShadowMultiVlLs";
const STROKE_LIST: &[u8] = b"\0\0\0\x0cframeFXMultiVlLs";
const STYLE_EFFECT_ENABLED: &[u8] = b"\0\0\0\0enabbool";
const COLOR_OVERLAY_BLEND: &[u8] = b"\0\0\0\x07presentbool\x01\0\0\0\0Md  enum\0\0\0\0BlnM";
const COLOR_OVERLAY_OPACITY: &[u8] = b"\0\0\0\0OpctUntF#Prc";
const COLOR_OVERLAY_COLOR: &[u8] = b"\0\0\0\0Clr Objc";
fn write_descriptor(output: &mut impl Write, styles: &StandardStyles<'_>) -> io::Result<()> {
    let (overlays, remaining) = styles.effects[..styles.count].split_at(styles.overlays);
    let (inner_shadows, remaining) = remaining.split_at(styles.inner_shadows);
    let (strokes, remaining) = remaining.split_at(styles.strokes);
    let gradients = styles.gradients.iter().flatten();
    let gradient_count = gradients.clone().count();
    let multi = styles.tag() == *b"lmfx";
    let fields = 1 + u32::from(!overlays.is_empty()) + remaining.len() as u32
        + u32::from(!inner_shadows.is_empty()) + u32::from(!strokes.is_empty()) + u32::from(multi)
        + u32::from(gradient_count > 0) + u32::from(styles.pattern.is_some());
    output.write_all(COLOR_OVERLAY_ROOT)?;
    output.write_all(&fields.to_be_bytes())?;
    output.write_all(COLOR_OVERLAY_MASTER)?;
    if multi {
        output.write_all(COLOR_OVERLAY_COUNT)?;
        let enabled = styles.effects[..styles.count].iter().filter(|effect| effect.enabled).count() as u32
            + gradients.clone().filter(|gradient| gradient.enabled).count() as u32
            + u32::from(styles.pattern.is_some_and(|pattern| pattern.enabled));
        output.write_all(&enabled.to_be_bytes())?;
    }
    if overlays.len() > 1 {
        output.write_all(COLOR_OVERLAY_LIST)?;
        output.write_all(&(overlays.len() as u32).to_be_bytes())?;
    } else if !overlays.is_empty() {
        output.write_all(b"\0\0\0\0SoFi")?;
    }
    for effect in overlays {
        write_style_object(output, *b"SoFi", effect)?;
    }
    if inner_shadows.len() > 1 {
        output.write_all(INNER_SHADOW_LIST)?;
        output.write_all(&(inner_shadows.len() as u32).to_be_bytes())?;
    } else if !inner_shadows.is_empty() {
        output.write_all(b"\0\0\0\0IrSh")?;
    }
    for effect in inner_shadows {
        write_style_object(output, *b"IrSh", effect)?;
    }
    if strokes.len() > 1 {
        output.write_all(STROKE_LIST)?;
        output.write_all(&(strokes.len() as u32).to_be_bytes())?;
    } else if !strokes.is_empty() {
        output.write_all(b"\0\0\0\0FrFX")?;
    }
    for effect in strokes {
        write_style_object(output, *b"FrFX", effect)?;
    }
    for effect in remaining {
        let key = effect.key();
        output.write_all(&[0; 4])?;
        output.write_all(&key)?;
        write_style_object(output, key, effect)?;
    }
    if gradient_count > 1 {
        output.write_all(GRADIENT_OVERLAY_LIST)?;
        output.write_all(&(gradient_count as u32).to_be_bytes())?;
    } else if gradient_count == 1 {
        output.write_all(b"\0\0\0\0GrFl")?;
    }
    for gradient in gradients {
        // Batch each object into one write to the guarded standard file sink.
        let mut object = io::Cursor::new([0; super::gradient_fill::MAX_DESCRIPTOR_BYTES]);
        write_style_header(&mut object, *b"GrFl", 12)?;
        let opacity = gradient.settings["opacity"].as_f64().unwrap_or(0.75) * 100.;
        write_style_properties(&mut object, gradient.enabled, gradient.blend_id, opacity)?;
        super::gradient_fill::write_fields(&mut object, gradient.settings).map_err(io::Error::other)?;
        output.write_all(&object.get_ref()[..object.position() as usize])?;
    }
    if let Some(pattern) = styles.pattern {
        let class = b"patternFill";
        let id_len = (class.len() as u32).to_be_bytes();
        output.write_all(&id_len)?;
        output.write_all(class)?;
        output.write_all(b"Objc\0\0\0\0")?;
        output.write_all(&id_len)?;
        output.write_all(class)?;
        let angle = pattern.placement.angle;
        output.write_all(&(9 + u32::from(angle != 0)).to_be_bytes())?;
        write_style_properties(output, pattern.enabled, pattern.blend_id, pattern.opacity)?;
        let id = pattern.id.ok_or_else(|| io::Error::other("Missing admitted Photoshop pattern asset"))?;
        super::patterns::write_identity(output, id)?;
        output.write_all(b"\0\0\0\0Algnbool\0")?;
        output.write_all(b"\0\0\0\0Lnkdbool\0")?;
        output.write_all(b"\0\0\0\0Scl UntF#Prc")?;
        output.write_all(&100f64.to_be_bytes())?;
        super::patterns::write_phase(output, pattern.placement.phase)?;
        if angle != 0 {
            super::patterns::write_angle(output, angle)?;
        }
    }
    Ok(())
}

pub(super) fn write_standard_style_tag(output: &mut impl Write, styles: &StandardStyles<'_>) -> io::Result<()> {
    struct Size(usize);
    impl Write for Size {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.checked_add(bytes.len()).filter(|len| *len <= MAX_DESCRIPTOR_BYTES)
                .ok_or_else(|| io::Error::other("Photoshop style descriptor exceeds 1 MiB"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut size = Size(0);
    write_descriptor(&mut size, styles)?;
    output.write_all(b"8BIM")?;
    output.write_all(&styles.tag())?;
    output.write_all(&(size.0 as u32).to_be_bytes())?;
    write_descriptor(output, styles)?;
    if size.0 % 2 != 0 {
        output.write_all(&[0])?;
    }
    Ok(())
}

pub(super) fn matches_descriptor(styles: &StandardStyles<'_>, payload: &[u8]) -> bool {
    struct Match<'a>(&'a [u8]);
    impl Write for Match<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0 = self.0.strip_prefix(bytes)
                .ok_or_else(|| io::Error::other("Photoshop style descriptor differs"))?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> { Ok(()) }
    }
    let mut comparison = Match(payload);
    write_descriptor(&mut comparison, styles).is_ok() && comparison.0.is_empty()
}

fn write_style_object(output: &mut impl std::io::Write, key: [u8; 4], effect: &StyleEffect) -> std::io::Result<()> {
    let fields: u32 = if effect.stroke.is_some() { 8 }
        else { effect.shadow.map_or(5, |shadow| 13 + u32::from(shadow.knockout.is_some())) };
    write_style_header(output, key, fields)?;
    write_style_properties(output, effect.enabled, effect.blend_id, effect.opacity)?;
    output.write_all(COLOR_OVERLAY_COLOR)?;
    output.write_all(&super::adjustments::rgb_descriptor(effect.color))?;
    if let Some(shadow) = effect.shadow { write_shadow_fields(output, shadow)?; }
    if let Some(stroke) = effect.stroke {
        output.write_all(b"\0\0\0\0Stylenum\0\0\0\0FStl\0\0\0\0")?;
        output.write_all(&stroke.position)?;
        output.write_all(b"\0\0\0\0PntTenum\0\0\0\0FrFl\0\0\0\0SClr")?;
        output.write_all(b"\0\0\0\0Sz  UntF#Pxl")?;
        output.write_all(&stroke.size.to_be_bytes())?;
    }
    Ok(())
}

fn write_style_header(output: &mut impl std::io::Write, key: [u8; 4], fields: u32) -> std::io::Result<()> {
    output.write_all(b"Objc")?;
    output.write_all(&[0; 8])?;
    output.write_all(&key)?;
    output.write_all(&fields.to_be_bytes())
}

fn write_style_properties(output: &mut impl std::io::Write, enabled: bool, blend: &str, opacity: f64) -> std::io::Result<()> {
    output.write_all(STYLE_EFFECT_ENABLED)?;
    output.write_all(&[u8::from(enabled)])?;
    output.write_all(COLOR_OVERLAY_BLEND)?;
    let id = blend.as_bytes();
    let length = if id.len() == 4 { 0 } else { id.len() as u32 };
    output.write_all(&length.to_be_bytes())?;
    output.write_all(id)?;
    output.write_all(COLOR_OVERLAY_OPACITY)?;
    output.write_all(&opacity.to_be_bytes())
}

fn write_shadow_fields(output: &mut impl std::io::Write, shadow: HardShadow) -> std::io::Result<()> {
    output.write_all(b"\0\0\0\0uglgbool")?;
    output.write_all(&[u8::from(shadow.global)])?;
    output.write_all(b"\0\0\0\0AntAbool\0")?;
    for (key, unit, value) in [
        (b"lagl", b"#Ang", shadow.light_angle),
        (b"Dstn", b"#Pxl", shadow.distance),
        (b"Ckmt", b"#Pxl", 0.), (b"blur", b"#Pxl", 0.), (b"Nose", b"#Prc", 0.),
    ] {
        output.write_all(&[0; 4])?;
        output.write_all(key)?;
        output.write_all(b"UntF")?;
        output.write_all(unit)?;
        output.write_all(&value.to_be_bytes())?;
    }
    output.write_all(b"\0\0\0\0TrnSObjc\0\0\0\0\0\0\0\0ShpC\0\0\0\x01\0\0\0\0Crv VlLs\0\0\0\x02")?;
    for value in [0u32, 255] {
        output.write_all(b"Objc\0\0\0\0\0\0\0\0CrPt\0\0\0\x02\0\0\0\0Hrznlong")?;
        output.write_all(&value.to_be_bytes())?;
        output.write_all(b"\0\0\0\0Vrtclong")?;
        output.write_all(&value.to_be_bytes())?;
    }
    if let Some(knockout) = shadow.knockout {
        output.write_all(b"\0\0\0\x0dlayerConcealsbool")?;
        output.write_all(&[u8::from(knockout)])?;
    }
    Ok(())
}

// Descriptor IDs differ from the layer record blend keys (ag-psd BlnM).
const OVERLAY_BLEND_IDS: &[(&str, &str, &[&str])] = &[
    ("normal", "Nrml", &["normal"]),
    ("multiply", "Mltp", &["multiply"]),
    ("screen", "Scrn", &["screen"]),
    ("overlay", "Ovrl", &["overlay"]),
    ("darken", "Drkn", &["darken"]),
    ("lighten", "Lghn", &["lighten"]),
    ("difference", "Dfrn", &["difference"]),
    ("exclusion", "Xclu", &["exclusion"]),
    ("color_burn", "CBrn", &["colorBurn"]),
    ("linear_burn", "linearBurn", &[]),
    ("darker_color", "darkerColor", &[]),
    ("color_dodge", "CDdg", &["colorDodge"]),
    ("linear_dodge", "linearDodge", &[]),
    ("lighter_color", "lighterColor", &[]),
    ("soft_light", "SftL", &["softLight"]),
    ("hard_light", "HrdL", &["hardLight"]),
    ("vivid_light", "vividLight", &[]),
    ("linear_light", "linearLight", &[]),
    ("pin_light", "pinLight", &[]),
    ("hard_mix", "hardMix", &[]),
    ("subtract", "blendSubtraction", &["Sbtr", "subtract"]),
    ("divide", "blendDivide", &["divide"]),
    ("hue", "H   ", &[]),
    ("saturation", "Strt", &[]),
    ("color", "Clr ", &[]),
    ("luminosity", "Lmns", &[]),
];

fn overlay_blend(value: &Value) -> Result<&'static str, String> {
    if value["type"] != "enum" || value["value"][0] != "BlnM" {
        return Err("Invalid Photoshop overlay blend enumeration".into());
    }
    let name = value["value"][1].as_str();
    let string_id = value["string_id"].as_bool();
    OVERLAY_BLEND_IDS.iter().find_map(|&(mode, canonical, aliases)| {
        std::iter::once(canonical).chain(aliases.iter().copied())
            .any(|id| Some(id) == name && Some(id.len() != 4) == string_id)
            .then_some(mode)
    }).ok_or_else(|| "Unsupported Photoshop overlay blend remains protected".into())
}
// RGBC has either byte-scaled channel fields or normalized float fields.
// Accept one complete spelling only; never clamp HDR values or mix scales.
// Reference: psd-tools/composite/paint.py::_get_color and terminology.py.
fn rgb_color(value: &Value) -> Result<Vec<f64>, String> {
    if value["value"]["class"] == "HSBC" {
        let channels = descriptor(value, "HSBC")?;
        if channels.len() != 3
            || ["H   ", "Strt", "Brgh"]
                .iter()
                .any(|key| !channels.contains_key(*key))
        {
            return Err(
                "Photoshop HSB colors require exactly hue, saturation and brightness".into(),
            );
        }
        let component = |key: &str, unit: &str, max: f64| -> Result<f64, String> {
            let value = field(channels, key);
            let number = if value["type"] == "UntF" {
                if value["value"][0] != unit {
                    return Err("Invalid Photoshop HSB component unit".into());
                }
                value["value"][1]
                    .as_f64()
                    .ok_or("Invalid Photoshop HSB component")?
            } else {
                plain_number(value)?
            };
            if !number.is_finite() || !(0.0..=max).contains(&number) {
                return Err("Photoshop HSB component exceeds its supported range".into());
            }
            Ok(number)
        };
        let hue = component("H   ", "#Ang", 360.)?.rem_euclid(360.) / 60.;
        let saturation = component("Strt", "#Prc", 100.)? / 100.;
        let brightness = component("Brgh", "#Prc", 100.)? / 100.;
        let fraction = hue - hue.floor();
        let p = brightness * (1. - saturation);
        let q = brightness * (1. - saturation * fraction);
        let t = brightness * (1. - saturation * (1. - fraction));
        return Ok(match hue as u8 {
            0 => vec![brightness, t, p],
            1 => vec![q, brightness, p],
            2 => vec![p, brightness, t],
            3 => vec![p, q, brightness],
            4 => vec![t, p, brightness],
            _ => vec![brightness, p, q],
        });
    }
    if value["value"]["class"] == "Grsc" {
        let channels = descriptor(value, "Grsc")?;
        if channels.len() != 1 || !channels.contains_key("Gry ") {
            return Err("Photoshop grayscale colors require one plain gray channel".into());
        }
        let black = plain_number(field(channels, "Gry "))?;
        if !(0.0..=100.).contains(&black) {
            return Err("Photoshop grayscale black percentage must be within 0–100".into());
        }
        // Grsc stores percent black: zero is white and 100 is black.
        // Preserve fractional source values in normalized RGB, without bytes.
        return Ok(vec![(100. - black) / 100.; 3]);
    }
    let channels = descriptor(value, "RGBC")?;
    let byte_keys = ["Rd  ", "Grn ", "Bl  "];
    let float_keys = ["redFloat", "greenFloat", "blueFloat"];
    let (keys, scale) =
        if channels.len() == 3 && byte_keys.iter().all(|key| channels.contains_key(*key)) {
            (byte_keys, 255.)
        } else if channels.len() == 3 && float_keys.iter().all(|key| channels.contains_key(*key)) {
            (float_keys, 1.)
        } else {
            return Err("Photoshop RGB colors require one complete plain channel spelling".into());
        };
    keys.into_iter()
        .map(|key| {
            let channel = plain_number(field(channels, key))?;
            if !(0.0..=scale).contains(&channel) {
                return Err(format!("Photoshop RGB channels must be within 0–{scale}"));
            }
            Ok(channel / scale)
        })
        .collect()
}

pub(super) fn solid_fill(bytes: &[u8]) -> Result<crate::raster::Pixel, String> {
    if bytes.len() > 512 {
        return Err("Photoshop Solid Color descriptor exceeds its supported size".into());
    }
    let root = descriptor_block(bytes)?;
    let items = root["items"].as_object().ok_or("Invalid Solid Color properties")?;
    if root["class"] != "null" || items.len() != 1 {
        return Err("Unsupported Photoshop Solid Color descriptor".into());
    }
    let channels = descriptor(field(items, "Clr "), "RGBC")?;
    if channels.len() != 3 {
        return Err("Photoshop Solid Color requires three plain RGB channels".into());
    }
    let mut color = [255; 4];
    for (out, key) in color.iter_mut().zip(["Rd  ", "Grn ", "Bl  "]) {
        let value = plain_number(field(channels, key))?;
        if !(0.0..=255.0).contains(&value) || value.fract() != 0.0 {
            return Err("Photoshop Solid Color exceeds editable fill precision".into());
        }
        *out = value as u8;
    }
    Ok(color)
}

/// A legacy stack supplies editable effects or confirms the corresponding
/// projections of an object-based stack.
pub(super) fn legacy_style(
    bytes: &[u8],
    modern: Option<&[crate::effects::Effect]>,
    global_angle: Option<u32>,
) -> Result<Vec<crate::effects::Effect>, String> {
    let mut reader = Cursor::new(bytes);
    if reader.u16()? != 0 {
        return Err("Unsupported legacy layer effects version".into());
    }
    let count = reader.u16()?;
    if !(1..=4).contains(&count) {
        return Err("Legacy effects require common state and at most three supported effects".into());
    }
    let mut common = false;
    let mut definitions = [None, None, None];
    for _ in 0..count {
        if reader.bytes(4)? != b"8BIM" {
            return Err("Invalid legacy effect signature".into());
        }
        let key = reader.bytes(4)?;
        let payload = reader.block()?;
        match key {
            b"cmnS" if !common && payload == [0, 0, 0, 0, 1, 0, 0] => common = true,
            b"sofi" if definitions[0].is_none() => definitions[0] = Some(legacy_color_overlay(payload)?),
            b"isdw" if definitions[1].is_none() => definitions[1] = Some(legacy_hard_shadow(payload, "inner_shadow", global_angle)?),
            b"dsdw" if definitions[2].is_none() => definitions[2] = Some(legacy_hard_shadow(payload, "drop_shadow", global_angle)?),
            _ => return Err("Unsupported, duplicate or invalid legacy effect record".into()),
        }
    }
    let trailer = &bytes[reader.pos..];
    if !common || trailer.len() > 3 || trailer.iter().any(|&byte| byte != 0)
        || !trailer.is_empty() && bytes.len() % 2 != 0
    {
        return Err("Legacy effects have invalid common state or padding".into());
    }
    let mut effects = Vec::new();
    for definition in definitions.into_iter().flatten() {
        if let Some(modern) = modern {
            let matches = modern.iter().find(|effect| effect.kind == definition.kind)
                .is_some_and(|effect| definition.matches_projection(effect));
            if !matches {
                return Err(format!("Legacy {} disagrees with the object-based style", definition.kind));
            }
        } else {
            effects.push(definition.into_effect()?);
        }
    }
    Ok(effects)
}

struct LegacyEffect {
    kind: &'static str,
    color: [u16; 3],
    blend: &'static str,
    opacity: u8,
    enabled: bool,
    shadow: Option<ShadowGeometry>,
}

impl LegacyEffect {
    fn into_effect(self) -> Result<crate::effects::Effect, String> {
        if self.kind == "drop_shadow" {
            return Err("Legacy Drop Shadow requires an object-based style with explicit knockout".into());
        }
        // Normalize through the object descriptor's units so the same legacy
        // words can be written back without drifting on repeated saves.
        let mut settings = json!({
            "opacity": (f64::from(self.opacity) * 100. / 255.) / 100.,
            "blend": self.blend,
            "color": self.color.map(|value| (f64::from(value) / 257.) / 255.),
        });
        if let Some(shadow) = self.shadow {
            shadow.apply(&mut settings);
        }
        crate::effects::validate(self.kind, &settings)?;
        Ok(crate::effects::Effect {
            id: crate::engine::id(),
            kind: self.kind.into(),
            enabled: self.enabled,
            weight: 1.,
            settings: settings.into(),
        })
    }

    fn matches_projection(&self, effect: &crate::effects::Effect) -> bool {
        let settings = &effect.settings;
        effect.enabled == self.enabled && effect.weight == 1.
            && settings["blend"].as_str().unwrap_or("normal") == self.blend
            && settings["opacity"].as_f64().is_some_and(|value| (value * 255.).round() == f64::from(self.opacity))
            && self.shadow.is_none_or(|shadow| shadow.matches(settings))
            && (self.kind != "drop_shadow" || settings["source_knockout"].is_boolean())
            && settings["color"].as_array().is_some_and(|color| {
                color.len() == 3 && color.iter().zip(self.color).all(|(value, word)| {
                    value.as_f64().is_some_and(|value| (value * 65535.).round() == f64::from(word))
                })
            })
    }
}

fn legacy_color_overlay(payload: &[u8]) -> Result<LegacyEffect, String> {
    let mut reader = Cursor::new(payload);
    if payload.len() != 34 || reader.u32()? != 2 {
        return Err("Invalid legacy Color Overlay version or length".into());
    }
    let blend = legacy_blend(&mut reader)?;
    let color = legacy_rgb(&mut reader)?;
    let opacity = reader.u8()?;
    let enabled = legacy_enabled(reader.u8()?)?;
    if legacy_rgb(&mut reader)? != color {
        return Err("Legacy Color Overlay requires matching RGB and native colors".into());
    }
    Ok(LegacyEffect { kind: "color_overlay", color, blend, opacity, enabled, shadow: None })
}

fn legacy_hard_shadow(payload: &[u8], kind: &'static str, global_angle: Option<u32>) -> Result<LegacyEffect, String> {
    let mut reader = Cursor::new(payload);
    let version = reader.u32()?;
    if !matches!((version, payload.len()), (0, 41) | (2, 51)) {
        return Err("Invalid legacy shadow version or length".into());
    }
    if reader.i32()? != 0 || reader.i32()? != 0 {
        return Err("Legacy hard shadows require zero blur and intensity".into());
    }
    let light = f64::from(reader.i32()?) / 65536.;
    let distance = f64::from(reader.i32()?) / 65536.;
    let color = legacy_rgb(&mut reader)?;
    let blend = legacy_blend(&mut reader)?;
    if kind == "drop_shadow" && blend != "normal" {
        return Err("Independent legacy Drop Shadow backdrop blends remain protected".into());
    }
    let enabled = legacy_enabled(reader.u8()?)?;
    let use_global = legacy_enabled(reader.u8()?)?;
    let shadow = ShadowGeometry::from_light(light, distance, use_global, global_angle)?;
    let opacity = reader.u8()?;
    if version == 2 && legacy_rgb(&mut reader)? != color {
        return Err("Legacy shadows require matching RGB and native colors".into());
    }
    Ok(LegacyEffect { kind, color, blend, opacity, enabled, shadow: Some(shadow) })
}

fn legacy_blend(reader: &mut Cursor<'_>) -> Result<&'static str, String> {
    if reader.bytes(4)? != b"8BIM" {
        return Err("Invalid legacy effect blend signature".into());
    }
    super::blend_name(reader.bytes(4)?)
        .filter(|blend| OVERLAY_BLEND_IDS.iter().any(|(supported, _, _)| blend == supported))
        .ok_or_else(|| "Unsupported legacy effect blend".into())
}

fn legacy_rgb(reader: &mut Cursor<'_>) -> Result<[u16; 3], String> {
    let space = reader.u16()?;
    let color = [reader.u16()?, reader.u16()?, reader.u16()?];
    if space != 0 || reader.u16()? != 0 {
        return Err("Legacy effect requires plain RGB color words".into());
    }
    Ok(color)
}

fn legacy_enabled(value: u8) -> Result<bool, String> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err("Invalid legacy effect enabled state".into()),
    }
}

pub(super) fn native_styles(
    bytes: &[u8],
    patterns: &[super::patterns::Source],
    depth: u16,
    global_angle: Option<u32>,
    admit_bitmap: &mut dyn FnMut(usize) -> Result<(), String>,
) -> Result<(Vec<crate::effects::Effect>, Option<usize>), String> {
    let parsed = root(bytes)?;
    if !matches!(parsed["class"].as_str(), Some("null" | "Lefx")) {
        return Err("Unsupported Photoshop effects class".into());
    }
    let items = parsed["items"]
        .as_object()
        .ok_or("Invalid effects object")?;
    if items.keys().any(|key| {
        !matches!(
            key.as_str(),
            "masterFXSwitch"
                | "numModifyingFX"
                | "Scl "
                | "SoFi"
                | "solidFillMulti"
                | "patternFill"
                | "GrFl"
                | "gradientFillMulti"
                | "FrFX"
                | "frameFXMulti"
                | "IrSh"
                | "innerShadowMulti"
                | "DrSh"
                | "dropShadowMulti"
        )
    }) {
        return Err("Unsupported Photoshop style in overlay stack".into());
    }
    let master = boolean(field(items, "masterFXSwitch")).ok_or("Missing style master switch")?;
    if items.get("Scl ").is_some_and(|value| percent(value) != Ok(1.)) {
        return Err("Scaled Photoshop styles remain protected".into());
    }
    let isolated = |key: &str, entry: &Value| {
        let mut fields = Map::new();
        for name in ["masterFXSwitch", "Scl "] {
            if let Some(value) = items.get(name) {
                fields.insert(name.into(), value.clone());
            }
        }
        fields.insert(key.into(), entry.clone());
        json!({"class": parsed["class"], "items": fields})
    };
    let stroke_entries = match (items.get("FrFX"), items.get("frameFXMulti")) {
        (Some(_), Some(_)) => return Err("Ambiguous Photoshop Stroke aliases".into()),
        (Some(stroke), None) => std::slice::from_ref(stroke),
        (None, Some(list)) if list["type"] == "VlLs" => list["value"]
            .as_array()
            .filter(|entries| (1..=MAX_INSIDE_STROKES).contains(&entries.len()))
            .ok_or("Unsupported Photoshop Stroke list length")?
            .as_slice(),
        (None, Some(_)) => return Err("Photoshop Stroke multi-effect key requires an explicit list".into()),
        (None, None) => &[],
    };
    let mut effects = vec![];
    let mut pattern_index = None;
    // Modern Photoshop stores a redundant count of enabled effects.
    // Validate it independently of the master switch, including hidden stacks.
    let modifying = items
        .get("numModifyingFX")
        .map(|value| {
            if value["type"] != "long" {
                return Err("Photoshop modifying-effect count requires an integer".to_string());
            }
            value["value"].as_u64().filter(|n| *n <= MAX_LAYER_EFFECTS as u64).ok_or_else(|| {
                "Photoshop modifying-effect count exceeds the shared limit".to_string()
            })
        })
        .transpose()?;
    let mut active_sources = 0u64;
    let source_active = |entry: &Value| entry["value"]["items"]["enab"]["value"] == true;
    // Photoshop overlays run Pattern, Gradient, Color (Color is uppermost).
    // Keep that execution order; native stacks saved previously keep theirs.
    // Reference: Patchy Photoshop-produced overlay-zorder fixture, commit
    // bb6b9426080842af6f4f65acb122516e07328323; see docs/layer-styles.md.
    // Preserve multi-overlay entries in file order, including disabled entries.
    // Never try alternative orders merely to match the saved appearance.
    for keys in [
        &["patternFill"][..],
        &["GrFl", "gradientFillMulti"][..],
        &["SoFi", "solidFillMulti"][..],
    ] {
        let present = keys
            .iter()
            .filter(|key| items.contains_key(**key))
            .collect::<Vec<_>>();
        if present.is_empty() {
            continue;
        }
        if present.len() != 1 {
            return Err("Ambiguous Photoshop single/multiple overlay aliases".into());
        }
        let key = *present[0];
        let source = field(items, key);
        let entries = if source["type"] == "VlLs" {
            let entries = source["value"]
                .as_array()
                .filter(|v| (1..=16).contains(&v.len()))
                .ok_or("Photoshop overlay list requires 1–16 definitions")?;
            if key == "patternFill" || entries.len() > 1 && !key.ends_with("Multi") {
                return Err(
                    "Multiple overlays require an explicit Photoshop multi-effect key".into(),
                );
            }
            entries.iter().collect::<Vec<_>>()
        } else {
            vec![source]
        };
        if effects.len() + entries.len() > MAX_LAYER_EFFECTS {
            return Err("Photoshop overlay stack exceeds the shared 32-effect limit".into());
        }
        for entry in entries {
            match keys[0] {
                "SoFi" => effects.extend(color_overlay_root(&isolated("SoFi", entry))?),
                "patternFill" => {
                    let (effect, index) = pattern_overlay(&isolated("patternFill", entry), patterns, depth, admit_bitmap)?;
                    effects.push(effect);
                    pattern_index = Some(index);
                }
                _ => effects.push(gradient_effect(descriptor(entry, "GrFl")?, master)?),
            }
            active_sources += u64::from(source_active(entry));
        }
    }
    // Overlays and Inner Shadows preserve alpha. Ordered Inner Shadows cast
    // from that same original alpha after the preceding colors have been applied.
    // Apply the single Normal Drop Shadow last: its behind compositing keeps
    // it beneath the styled image, and its grown alpha cannot feed an inner
    // shadow or become overlay coverage. The caller still requires unchanged
    // casting alpha, explicit Drop Shadow knockout, Normal layer blend, no masks/clipping
    // and an exact composite.
    // Reference: Krita kis_layer_style_projection_plane.cpp (overlay inner
    // shadow and stylesBefore drop shadow). Multiple Drop Shadows require a shared backdrop.
    for keys in [["IrSh", "innerShadowMulti"], ["DrSh", "dropShadowMulti"]] {
        let present = keys
            .iter()
            .filter(|key| items.contains_key(**key))
            .collect::<Vec<_>>();
        if present.is_empty() {
            continue;
        }
        if present.len() != 1 {
            return Err("Ambiguous Photoshop single/multiple shadow aliases".into());
        }
        let key = present[0];
        let source = field(items, key);
        let entries = if key.ends_with("Multi") {
            if source["type"] != "VlLs" {
                return Err("Photoshop shadow multi-effect keys require an explicit list".into());
            }
            let limit = if keys[0] == "IrSh" { MAX_INNER_SHADOWS } else { 1 };
            source["value"].as_array()
                .filter(|entries| (1..=limit).contains(&entries.len()))
                .ok_or("Unsupported Photoshop shadow list length")?
                .as_slice()
        } else {
            std::slice::from_ref(source)
        };
        if effects.len() + entries.len() > MAX_LAYER_EFFECTS {
            return Err("Photoshop style stack exceeds the shared 32-effect limit".into());
        }
        for entry in entries {
            effects.push(hard_shadow(entry, keys[0], master, global_angle)?);
            active_sources += u64::from(source_active(entry));
        }
    }
    if !stroke_entries.is_empty() {
        if effects.len() + stroke_entries.len() > MAX_LAYER_EFFECTS {
            return Err("Photoshop style stack exceeds the shared 32-effect limit".into());
        }
        let drop_shadow = effects.last().filter(|effect| effect.kind == "drop_shadow");
        let casts = |effect: &crate::effects::Effect| {
            effect.enabled && effect.settings["opacity"].as_f64() != Some(0.)
        };
        let casts_shadow = drop_shadow.is_some_and(casts);
        let index = effects.len() - usize::from(drop_shadow.is_some());
        // Inside strokes keep the original alpha for later strokes and the final Drop Shadow.
        for (offset, source) in stroke_entries.iter().enumerate() {
            let (stroke, enabled) = solid_stroke(source, master)?;
            if stroke.settings["position"] != "inside" {
                if stroke_entries.len() > 1 {
                    return Err("Multiple Photoshop Strokes require inside placement".into());
                }
                if stroke.settings["size"].as_f64() != Some(0.) && casts(&stroke) && casts_shadow {
                    return Err("Photoshop outside or center Stroke combined with Drop Shadow remains protected".into());
                }
            }
            effects.insert(index + offset, stroke);
            active_sources += u64::from(enabled);
        }
    }
    if effects.is_empty() {
        return Err("No supported Photoshop overlay definitions".into());
    }
    if modifying.is_some_and(|count| count != active_sources) {
        return Err("Photoshop modifying-effect count disagrees with its definitions".into());
    }
    Ok((effects, pattern_index))
}
// Solid strokes follow ag-psd's EffectDescriptor, FStl and FrFl.
fn solid_stroke(source: &Value, master: bool) -> Result<(crate::effects::Effect, bool), String> {
    let fields = descriptor(source, "FrFX")?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "enab"
                | "present"
                | "showInDialog"
                | "Styl"
                | "PntT"
                | "Md  "
                | "Opct"
                | "Sz  "
                | "Clr "
                | "overprint"
        )
    }) {
        return Err("Unsupported Photoshop solid Stroke property".into());
    }
    let enabled = boolean(field(fields, "enab")).ok_or("Invalid stroke enabled flag")?;
    let present = boolean(field(fields, "present")).ok_or("Invalid stroke present flag")?;
    if fields
        .get("showInDialog")
        .is_some_and(|v| boolean(v).is_none())
        || fields
            .get("overprint")
            .is_some_and(|v| boolean(v) != Some(false))
    {
        return Err("Invalid stroke dialog flag or unsupported overprint".into());
    }
    if field(fields, "PntT")["type"] != "enum"
        || field(fields, "PntT")["value"] != json!(["FrFl", "SClr"])
    {
        return Err("Only plain solid-color Photoshop strokes can be imported".into());
    }
    let position = field(fields, "Styl");
    if position["type"] != "enum" || position["value"][0] != "FStl" {
        return Err("Invalid Photoshop Stroke position enum".into());
    }
    let position = match position["value"][1].as_str() {
        Some("OutF") => "outside",
        Some("InsF") => "inside",
        Some("CtrF") => "center",
        _ => return Err("Unsupported Photoshop Stroke position".into()),
    };
    let size = field(fields, "Sz  ");
    let pixels = size["value"][1].as_f64().ok_or("Missing stroke size")?;
    if size["type"] != "UntF"
        || size["value"][0] != "#Pxl"
        || !(0. ..=crate::effects::styles::MAX_STROKE_SIZE).contains(&pixels)
        || pixels.fract() != 0.
    {
        return Err("Photoshop Stroke requires a bounded whole-pixel size".into());
    }
    let blend = overlay_blend(field(fields, "Md  "))?;
    if blend != "normal" {
        return Err("Independent Photoshop Stroke backdrop blends remain protected".into());
    }
    let settings = json!({"position":position,"size":pixels,"color":rgb_color(field(fields,"Clr "))?,"opacity":percent(field(fields,"Opct"))?,"blend":blend});
    crate::effects::validate("stroke", &settings)?;
    let effect = crate::effects::Effect {
        id: crate::engine::id(),
        kind: "stroke".into(),
        enabled: master && enabled && present,
        weight: 1.,
        settings: settings.into(),
    };
    Ok((effect, enabled))
}
#[derive(Clone, Copy)]
struct ShadowGeometry {
    angle: f64,
    distance: f64,
    pixel_snap: bool,
    global_light: Option<crate::effects::styles::GlobalLight>,
}

impl ShadowGeometry {
    fn from_light(local_angle: f64, distance: f64, use_global: bool, global_angle: Option<u32>) -> Result<Self, String> {
        use crate::effects::styles::GlobalLight;
        let global_light = if use_global {
            let angle = global_angle.filter(|&angle| angle < 360)
                .ok_or("Global-light shadows require an explicit valid Photoshop Global Angle")?;
            Some(GlobalLight { angle, local_angle })
        } else {
            None
        };
        let light = global_light.map_or(local_angle, |global| f64::from(global.angle));
        if !(-180. ..=180.).contains(&local_angle) || light.fract() != 0.
            || !(0. ..=128.).contains(&distance) || distance.fract() != 0.
        {
            return Err("Hard shadows require a whole-degree cast and bounded whole-pixel distance".into());
        }
        // Photoshop locates the light, while the renderer locates the cast.
        Ok(Self {
            angle: GlobalLight::cast_angle(light),
            distance,
            pixel_snap: light % 90. != 0.,
            global_light,
        })
    }

    fn apply(self, settings: &mut Value) {
        settings["angle"] = json!(self.angle);
        settings["distance"] = json!(self.distance);
        settings["size"] = json!(0.);
        settings["spread"] = json!(0.);
        settings["contour"] = json!("linear");
        if self.pixel_snap {
            settings["pixel_snap"] = json!(true);
        }
        if let Some(global) = self.global_light {
            settings["global_light"] = json!([global.angle, global.local_angle]);
        }
    }

    fn matches(self, settings: &Value) -> bool {
        let same_light = match (self.global_light, crate::effects::styles::GlobalLight::from_value(&settings["global_light"])) {
            (None, None) => true,
            (Some(legacy), Some(modern)) => legacy.angle == modern.angle
                && (legacy.local_angle * 65536.).round() == (modern.local_angle * 65536.).round(),
            _ => false,
        };
        same_light && settings["angle"].as_f64() == Some(self.angle)
            && settings["distance"].as_f64() == Some(self.distance)
            && settings["size"] == 0.
            && settings["spread"] == 0.
            && settings["contour"] == "linear"
            && (settings["pixel_snap"] == true) == self.pixel_snap
    }
}

// Shadow fields follow psd-tools/api/effects.py and Krita's ASL serializer.
// Validate each isolated shadow definition. The native stack builder keeps
// casts bound to original alpha. Independent exterior backdrop blends and
// multiple Drop Shadows still require separate source/backdrop handling.
fn hard_shadow(source: &Value, key: &str, master: bool, global_angle: Option<u32>) -> Result<crate::effects::Effect, String> {
    let kind = if key == "DrSh" { "drop_shadow" } else { "inner_shadow" };
    let fields = descriptor(source, key)?;
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "enab"
                | "present"
                | "showInDialog"
                | "Md  "
                | "Opct"
                | "Clr "
                | "uglg"
                | "lagl"
                | "Dstn"
                | "Ckmt"
                | "blur"
                | "Nose"
                | "AntA"
                | "TrnS"
        ) && !(kind == "drop_shadow" && key == "layerConceals")
    }) {
        return Err("Unsupported Photoshop hard shadow property".into());
    }
    let required_bool =
        |key: &str| boolean(field(fields, key)).ok_or("Missing or invalid shadow boolean");
    let enabled = required_bool("enab")?;
    let present = required_bool("present")?;
    if fields
        .get("showInDialog")
        .is_some_and(|v| boolean(v).is_none())
        || required_bool("AntA")?
    {
        return Err("Antialiased or invalid Photoshop hard shadows remain protected".into());
    }
    let unit = |key: &str, unit: &str| -> Result<f64, String> {
        let value = field(fields, key);
        if value["type"] != "UntF" || value["value"][0] != unit {
            return Err("Invalid Photoshop shadow unit".into());
        }
        value["value"][1]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or("Invalid Photoshop shadow number".into())
    };
    let hard_only = "Only integer-angle, integer-distance shadows with zero size, choke and noise can be imported";
    let geometry = ShadowGeometry::from_light(unit("lagl", "#Ang")?, unit("Dstn", "#Pxl")?, required_bool("uglg")?, global_angle)?;
    if unit("blur", "#Pxl")? != 0.
        || unit("Ckmt", "#Pxl")? != 0.
        || percent(field(fields, "Nose"))? != 0.
    {
        return Err(hard_only.into());
    }
    let contour = descriptor(field(fields, "TrnS"), "ShpC")?;
    if contour
        .keys()
        .any(|key| !matches!(key.as_str(), "Nm  " | "Crv "))
        || contour.get("Nm  ").is_some_and(|v| v["type"] != "TEXT")
        || field(contour, "Crv ")["type"] != "VlLs"
    {
        return Err("Hard shadow requires an explicit linear contour".into());
    }
    let points = field(contour, "Crv ")["value"]
        .as_array()
        .filter(|v| v.len() == 2)
        .ok_or("Hard shadow requires two contour endpoints")?;
    for (point, expected) in points.iter().zip([0., 255.]) {
        let point = descriptor(point, "CrPt")?;
        if point.len() != 2
            || plain_number(field(point, "Hrzn"))? != expected
            || plain_number(field(point, "Vrtc"))? != expected
        {
            return Err("Nonlinear or malformed Photoshop shadow contour remains protected".into());
        }
    }
    let color = rgb_color(field(fields, "Clr "))?;
    let blend = overlay_blend(field(fields, "Md  "))?;
    if kind == "drop_shadow" && blend != "normal" {
        return Err("Independent Photoshop Drop Shadow backdrop blends remain protected".into());
    }
    let mut settings = json!({"opacity": percent(field(fields, "Opct"))?, "color": color});
    geometry.apply(&mut settings);
    if kind == "inner_shadow" {
        settings["blend"] = json!(blend);
    } else {
        // Retain pre-composite knockout even on currently binary alpha:
        // later source edits must not lose the Photoshop definition.
        // Reference: Krita ASL layerConceals and applyDropShadow knockout.
        settings["source_knockout"] = json!(required_bool("layerConceals")?);
    }
    crate::effects::validate(kind, &settings)?;
    Ok(crate::effects::Effect {
        id: crate::engine::id(),
        kind: kind.into(),
        enabled: master && enabled && present,
        weight: 1.,
        settings: settings.into(),
    })
}
fn pattern_overlay(
    root: &Value,
    patterns: &[super::patterns::Source],
    depth: u16,
    admit_bitmap: &mut dyn FnMut(usize) -> Result<(), String>,
) -> Result<(crate::effects::Effect, usize), String> {
    if !matches!(root["class"].as_str(), Some("null" | "Lefx")) {
        return Err("Unsupported Photoshop effects class".into());
    }
    let items = root["items"].as_object().ok_or("Invalid effects object")?;
    if items
        .keys()
        .any(|k| !matches!(k.as_str(), "masterFXSwitch" | "Scl " | "patternFill"))
        || items.get("Scl ").is_some_and(|v| percent(v) != Ok(1.))
    {
        return Err("Only a single unscaled plain Pattern Overlay can be imported".into());
    }
    let master = boolean(field(items, "masterFXSwitch")).ok_or("Missing style master switch")?;
    let fields = descriptor(field(items, "patternFill"), "patternFill")?;
    if fields.keys().any(|k| {
        !matches!(
            k.as_str(),
            "enab"
                | "present"
                | "showInDialog"
                | "Md  "
                | "Opct"
                | "Ptrn"
                | "Angl"
                | "Scl "
                | "Algn"
                | "Lnkd"
                | "phase"
        )
    }) {
        return Err("Unsupported Pattern Overlay property".into());
    }
    let enabled = boolean(field(fields, "enab")).ok_or("Missing pattern enabled state")?;
    let present = boolean(field(fields, "present")).ok_or("Missing pattern present state")?;
    let blend = overlay_blend(field(fields, "Md  "))?;
    if fields
        .get("showInDialog")
        .is_some_and(|v| boolean(v).is_none())
        || percent(field(fields, "Scl "))? != 1.
        || boolean(field(fields, "Algn")) != Some(false)
        || fields
            .get("Lnkd")
            .is_some_and(|v| boolean(v) != Some(false))
    {
        return Err("Pattern Overlay requires unlinked placement at 100% scale".into());
    }
    let placement = pattern_placement(fields)?;
    let (index, source) = pattern_asset(field(fields, "Ptrn"), patterns, depth)?;
    let effect = bitmap_pattern(source, placement, percent(field(fields, "Opct"))?, blend, master && enabled && present, admit_bitmap)?;
    Ok((effect, index))
}

pub(super) fn pattern_fill(
    bytes: &[u8],
    patterns: &[super::patterns::Source],
    depth: u16,
    admit_bitmap: &mut dyn FnMut(usize) -> Result<(), String>,
) -> Result<(crate::effects::Effect, usize), String> {
    let root = descriptor_block(bytes)?;
    let fields = root["items"].as_object().ok_or("Invalid Pattern Fill properties")?;
    if root["class"] != "null" || fields.keys().any(|key|
        !matches!(key.as_str(), "Ptrn" | "Lnkd" | "Algn" | "phase" | "Scl " | "Angl"))
        || boolean(field(fields, "Lnkd")) != Some(false)
        || fields.get("Algn").is_some_and(|v| boolean(v) != Some(false))
        || fields.get("Scl ").is_some_and(|v| percent(v) != Ok(1.))
    {
        return Err("Only unlinked Pattern Fill at 100% scale is supported".into());
    }
    let placement = pattern_placement(fields)?;
    let (index, source) = pattern_asset(field(fields, "Ptrn"), patterns, depth)?;
    let mut effect = bitmap_pattern(source, placement, 1., "normal", true, admit_bitmap)?;
    std::sync::Arc::make_mut(&mut effect.settings)["use_pattern_alpha"] = json!(true);
    crate::effects::validate(&effect.kind, &effect.settings)?;
    Ok((effect, index))
}

fn pattern_placement(fields: &Map<String, Value>) -> Result<super::patterns::Placement, String> {
    let angle = fields.get("Angl").map_or(Some(0), |value| {
        if value["type"] != "UntF" || value["value"][0] != "#Ang" { return None; }
        super::patterns::right_angle(value["value"][1].as_f64()?)
    }).ok_or("Photoshop pattern requires right-angle rotation")?;
    let phase = if let Some(value) = fields.get("phase") {
        let point = descriptor(value, "Pnt ")?;
        let error = "Photoshop pattern requires bounded whole-pixel offsets";
        if point.len() != 2 { return Err(error.into()); }
        let coordinate = |key| {
            let value = field(point, key);
            let number = if value["type"] == "UntF" && value["value"][0] == "#Pxl" {
                value["value"][1].as_f64()
            } else {
                plain_number(value).ok()
            };
            number.and_then(super::patterns::integral_phase).ok_or(error)
        };
        [coordinate("Hrzn")?, coordinate("Vrtc")?]
    } else {
        [0, 0]
    };
    Ok(super::patterns::Placement { phase, angle })
}

fn pattern_asset<'a>(
    value: &Value,
    patterns: &'a [super::patterns::Source],
    depth: u16,
) -> Result<(usize, &'a super::patterns::Source), String> {
    let pattern = descriptor(value, "Ptrn")?;
    if pattern
        .keys()
        .any(|k| !matches!(k.as_str(), "Nm  " | "Idnt"))
        || pattern.get("Nm  ").is_some_and(|v| v["type"] != "TEXT")
        || field(pattern, "Idnt")["type"] != "TEXT"
    {
        return Err("Unsupported pattern identity fields".into());
    }
    let identity = field(pattern, "Idnt")["value"]
        .as_str()
        .ok_or("Invalid pattern identity")?;
    let identity = identity.strip_suffix('\0').unwrap_or(identity);
    let mut matches = patterns
        .iter()
        .enumerate()
        .filter(|(_, p)| p.pattern_id == identity);
    let (index, source) = matches.next().ok_or("Missing embedded pattern identity")?;
    if matches.next().is_some() {
        return Err("Missing or ambiguous embedded pattern identity".into());
    }
    // Pattern channels carry their own precision. An 8-bit tile is exactly
    // representable in a 16-bit document; retain its PNG8 source rather than
    // projecting the document or pretending the tile contains native low bits.
    if !(source.bit_depth == depth || source.bit_depth == 8 && depth == 16) {
        return Err("Pattern precision requires protected interpretation".into());
    }
    Ok((index, source))
}

fn bitmap_pattern(
    source: &super::patterns::Source,
    placement: super::patterns::Placement,
    opacity: f64,
    blend: &str,
    enabled: bool,
    admit: &mut dyn FnMut(usize) -> Result<(), String>,
) -> Result<crate::effects::Effect, String> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    // The asset rectangle is its storage frame, not document placement. The
    // decoder checks matching full channel rectangles and extracts local rows.
    // Retain those tile samples without applying the storage origin as phase.
    let png = if source.bit_depth == 16 {
        crate::raster::png16(source.width, source.height, &source.pixels.rgba16())?
    } else {
        crate::raster::png(source.width, source.height, &source.pixels.rgba())?
    };
    if png.len() > crate::effects::patterns::ENCODED_LIMIT {
        return Err("Bitmap PNG exceeds 4 MiB".into());
    }
    admit(png.len().div_ceil(3) * 4 + source.width as usize * source.height as usize * 8)?;
    let settings = json!({"opacity":opacity,"blend":blend,"pattern":"bitmap","bitmap_png":STANDARD.encode(png),
        "bitmap_scale":1.,"bitmap_angle":f64::from(placement.angle),
        "phase_x":f64::from(placement.phase[0]),"phase_y":f64::from(placement.phase[1]),"sampling":"nearest"});
    crate::effects::validate("pattern_overlay", &settings)?;
    Ok(crate::effects::Effect {
        id: crate::engine::id(), kind: "pattern_overlay".into(), enabled,
        weight: 1., settings: settings.into(),
    })
}
fn descriptor<'a>(value: &'a Value, class: &str) -> Result<&'a Map<String, Value>, String> {
    if !matches!(value["type"].as_str(), Some("Objc" | "GlbO")) || value["value"]["class"] != class
    {
        return Err("Unsupported Photoshop style descriptor class".into());
    }
    value["value"]["items"]
        .as_object()
        .ok_or("Invalid Photoshop style properties".into())
}
fn field<'a>(fields: &'a Map<String, Value>, key: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    fields.get(key).unwrap_or(&NULL)
}
/// GdFl uses the same gradient definition as GrFl, without overlay blending.
/// The native coverage mode applies gradient opacity to the fill's alpha.
pub(super) fn gradient_fill(bytes: &[u8]) -> Result<crate::effects::Effect, String> {
    let root = descriptor_block(bytes)?;
    let mut fields = root["items"].as_object().ok_or("Invalid Gradient Fill properties")?.clone();
    if root["class"] != "null" || fields.keys().any(|key| !matches!(key.as_str(),
        "Grad" | "Angl" | "Type" | "Rvrs" | "Dthr" | "Algn" | "Scl " | "Ofst"
    )) {
        return Err("Unsupported Photoshop Gradient Fill descriptor".into());
    }
    for key in ["enab", "present"] {
        fields.insert(key.into(), json!({"type":"bool", "value":true}));
    }
    fields.insert("Md  ".into(), json!({"type":"enum", "value":["BlnM", "Nrml"]}));
    fields.insert("Opct".into(), json!({"type":"UntF", "value":["#Prc", 100.]}));
    let mut effect = gradient_effect(&fields, true)?;
    std::sync::Arc::make_mut(&mut effect.settings)["use_gradient_alpha"] = json!(true);
    crate::effects::validate(&effect.kind, &effect.settings)?;
    Ok(effect)
}

fn gradient_effect(fields: &Map<String, Value>, master: bool) -> Result<crate::effects::Effect, String> {
    if fields.keys().any(|k| {
        !matches!(
            k.as_str(),
            "enab"
                | "present"
                | "showInDialog"
                | "Md  "
                | "Opct"
                | "Grad"
                | "Angl"
                | "Type"
                | "Rvrs"
                | "Dthr"
                | "Algn"
                | "Scl "
                | "Ofst"
        )
    }) {
        return Err("Unsupported Photoshop gradient property/interpolation method".into());
    }
    let required_bool = |key: &str| {
        fields
            .get(key)
            .and_then(boolean)
            .ok_or_else(|| format!("Invalid Gradient Overlay {key}"))
    };
    let enabled = required_bool("enab")?;
    let present = required_bool("present")?;
    if fields
        .get("showInDialog")
        .is_some_and(|v| boolean(v).is_none())
    {
        return Err("Invalid Gradient Overlay dialog flag".into());
    }
    if required_bool("Dthr")? {
        return Err("Dithered Photoshop gradients remain protected".into());
    }
    let align_layer = required_bool("Algn")?;
    let blend = overlay_blend(field(fields, "Md  "))?;
    if field(fields, "Type")["type"] != "enum" || field(fields, "Type")["value"][0] != "GrdT" {
        return Err("Invalid Photoshop Gradient Overlay shape enumeration".into());
    }
    let style = match field(fields, "Type")["value"][1].as_str() {
        Some("Lnr ") => "linear",
        Some("Rflc") => "reflected",
        Some("Rdl ") => "radial",
        Some("Angl") => "angle",
        Some("Dmnd") => "diamond",
        _ => return Err("Unsupported Photoshop Gradient Overlay shape".into()),
    };
    let angle = field(fields, "Angl");
    let degrees = angle["value"][1]
        .as_f64()
        .ok_or("Missing Photoshop gradient angle")?;
    if angle["type"] != "UntF"
        || angle["value"][0] != "#Ang"
        || ![-180., -90., 0., 90., 180.].contains(&degrees)
    {
        return Err("Only cardinal-axis Photoshop gradients can be imported".into());
    }
    let scale = bounded_percent(field(fields, "Scl "), 10., 1000.)?;
    let offset = if let Some(offset) = fields.get("Ofst") {
        let offset = descriptor(offset, "Pnt ")?;
        if offset.len() != 2 {
            return Err("Gradient offset requires exactly two percentage coordinates".into());
        }
        Some((
            bounded_percent(field(offset, "Hrzn"), -100., 100.)?,
            bounded_percent(field(offset, "Vrtc"), -100., 100.)?,
        ))
    } else {
        None
    };
    let gradient = descriptor(field(fields, "Grad"), "Grdn")?;
    if gradient
        .keys()
        .any(|k| !matches!(k.as_str(), "Nm  " | "GrdF" | "Intr" | "Clrs" | "Trns"))
        || gradient.get("Nm  ").is_some_and(|v| v["type"] != "TEXT")
        || field(gradient, "GrdF")["type"] != "enum"
        || field(gradient, "GrdF")["value"] != json!(["GrdF", "CstS"])
        || plain_number(field(gradient, "Intr"))? != 0.
    {
        return Err(
            "Noise, smooth or unsupported Photoshop gradient definitions remain protected".into(),
        );
    }
    let mut settings = json!({"opacity":percent(field(fields, "Opct"))? ,"blend":blend,"angle":degrees,"scale":scale,"style":style,"reverse":required_bool("Rvrs")?});
    if align_layer || matches!(style, "radial" | "angle" | "diamond") {
        // Explicit alignment and the newly supported shapes use the existing
        // native integer grid. Recognition never bypasses the caller's exact
        // complete-composite gate. Shape IDs: ag-psd src/descriptor.ts, GrdT.
        settings["pixel_grid"] = json!(true);
    }
    if align_layer {
        settings["align_layer"] = json!(true);
    }
    if let Some((x, y)) = offset {
        settings["offset_x"] = json!(x);
        settings["offset_y"] = json!(y);
    }
    for (input, output, class, alpha) in [
        ("Clrs", "color_stops", "Clrt", false),
        ("Trns", "alpha_stops", "TrnS", true),
    ] {
        // An omitted transparency list means opaque coverage. Explicit empty
        // or malformed lists remain invalid, rather than silently defaulting.
        if alpha && !gradient.contains_key(input) {
            settings[output] = json!([]);
            continue;
        }
        let list = field(gradient, input);
        if list["type"] != "VlLs" {
            return Err("Gradient stops require descriptor lists".into());
        }
        let list = list["value"]
            .as_array()
            .filter(|s| (1..=16).contains(&s.len()))
            .ok_or("Gradient requires 1–16 source stops")?;
        let mut stops = vec![];
        let mut previous = -1.;
        for entry in list {
            let stop = descriptor(entry, class)?;
            if stop.keys().any(|k| {
                !matches!(k.as_str(), "Lctn" | "Mdpn")
                    && if alpha {
                        k != "Opct"
                    } else {
                        k != "Clr " && k != "Type"
                    }
            }) || field(stop, "Lctn")["type"] != "long"
                || field(stop, "Mdpn")["type"] != "long"
                || !(1. ..=99.).contains(&plain_number(field(stop, "Mdpn"))?)
            {
                return Err("Unsupported Photoshop gradient stop fields/midpoint".into());
            }
            // Photoshop stores each midpoint on the destination stop. Read
            // and validate all source fields before translating to the native
            // outgoing-segment representation below.
            let position = plain_number(field(stop, "Lctn"))? / 4096.;
            if !(0.0..=1.).contains(&position) || position <= previous {
                return Err("Photoshop gradient stops must strictly increase within 0–4096".into());
            }
            previous = position;
            let mut result = json!({
                "position":position,
                "midpoint":plain_number(field(stop, "Mdpn"))?/100.
            });
            if alpha {
                result["opacity"] = json!(percent(field(stop, "Opct"))?);
            } else {
                if field(stop, "Type")["type"] != "enum"
                    || field(stop, "Type")["value"] != json!(["Clry", "UsrS"])
                {
                    return Err(
                        "Foreground/background Photoshop gradient colors remain protected".into(),
                    );
                }
                result["color"] = json!(rgb_color(field(stop, "Clr "))?);
            }
            stops.push(result);
        }
        // The first Photoshop midpoint is unused. Native sources place the
        // ratio on the preceding stop; never reinterpret already-saved native
        // definitions. Reference: Patchy docs/ps-compat.md (PS calibration).
        for index in 0..stops.len() - 1 {
            stops[index]["midpoint"] = stops[index + 1]["midpoint"].clone();
        }
        stops.last_mut().unwrap()["midpoint"] = json!(0.5);
        // Photoshop extends the nearest stop outside its range, including a
        // one-stop constant ramp. Native controls require explicit endpoints;
        // retain every original stop and add only equivalent constant edges.
        if stops[0]["position"] != 0. {
            let mut edge = stops[0].clone();
            edge["position"] = json!(0.);
            edge["midpoint"] = json!(0.5);
            stops.insert(0, edge);
        }
        if stops.last().unwrap()["position"] != 1. {
            let mut edge = stops.last().unwrap().clone();
            edge["position"] = json!(1.);
            edge["midpoint"] = json!(0.5);
            stops.push(edge);
        }
        if stops.len() > 16 {
            return Err("Photoshop gradient endpoints exceed the shared 16-stop limit".into());
        }
        settings[output] = json!(stops);
    }
    crate::effects::validate("gradient_overlay", &settings)?;
    Ok(crate::effects::Effect {
        id: crate::engine::id(),
        kind: "gradient_overlay".into(),
        enabled: master && enabled && present,
        weight: 1.,
        settings: settings.into(),
    })
}
/// Small explicit import subset. The caller must verify the entire saved composite.
fn color_overlay_root(root: &Value) -> Result<Vec<crate::effects::Effect>, String> {
    if !matches!(root["class"].as_str(), Some("null" | "Lefx")) {
        return Err("Unsupported Photoshop effects class".into());
    }
    let items = root["items"].as_object().ok_or("Invalid effects object")?;
    let master = items
        .get("masterFXSwitch")
        .and_then(boolean)
        .ok_or("Missing style master switch")?;
    if let Some(scale) = items.get("Scl ") {
        if percent(scale)? != 1. {
            return Err("Scaled Photoshop styles remain protected".into());
        }
    }
    if items.keys().any(|k| {
        !matches!(
            k.as_str(),
            "masterFXSwitch" | "Scl " | "SoFi" | "solidFillMulti"
        )
    }) || items.contains_key("SoFi") && items.contains_key("solidFillMulti")
    {
        return Err("Only unambiguous plain Color Overlay definitions can be imported".into());
    }
    let source = items
        .get("SoFi")
        .or_else(|| items.get("solidFillMulti"))
        .ok_or("No Color Overlay definitions")?;
    let entries = if source["type"] == "VlLs" {
        source["value"]
            .as_array()
            .ok_or("Invalid Color Overlay list")?
            .iter()
            .collect::<Vec<_>>()
    } else {
        vec![source]
    };
    if entries.is_empty() || entries.len() > 32 {
        return Err("Color Overlay requires 1–32 definitions".into());
    }
    // Photoshop multi-effect ordering is not guessed. Preserve a single definition only.
    if entries.len() != 1 {
        return Err("Multiple Photoshop Color Overlay ordering remains protected".into());
    }
    let mut effects = vec![];
    for entry in entries {
        if !matches!(entry["type"].as_str(), Some("Objc" | "GlbO"))
            || entry["value"]["class"] != "SoFi"
        {
            return Err("Invalid Color Overlay descriptor class".into());
        }
        let fields = entry["value"]["items"]
            .as_object()
            .ok_or("Invalid Color Overlay properties")?;
        if fields.keys().any(|k| {
            !matches!(
                k.as_str(),
                "enab" | "present" | "showInDialog" | "Md  " | "Opct" | "Clr "
            )
        }) {
            return Err("Unsupported Color Overlay property".into());
        }
        let enabled = fields
            .get("enab")
            .and_then(boolean)
            .ok_or("Missing Color Overlay enabled state")?;
        let present = fields
            .get("present")
            .and_then(boolean)
            .ok_or("Missing Color Overlay present state")?;
        if fields
            .get("showInDialog")
            .is_some_and(|v| boolean(v).is_none())
        {
            return Err("Invalid Color Overlay dialog state".into());
        }
        let blend = overlay_blend(field(fields, "Md  "))?;
        let opacity = percent(&entry["value"]["items"]["Opct"])?;
        let rgb = rgb_color(field(fields, "Clr "))?;
        let settings = json!({"opacity":opacity,"blend":blend,"color":rgb});
        crate::effects::validate("color_overlay", &settings)?;
        effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "color_overlay".into(),
            enabled: master && enabled && present,
            weight: 1.,
            settings: settings.into(),
        });
    }
    Ok(effects)
}

#[cfg(test)]
pub(in crate::psd) mod tests {
    use super::*;

    pub(in crate::psd) fn color_overlay_mask(depth: u16) -> crate::engine::Mask {
        let mut pixels = crate::raster::Raster::new_depth(2, 1, depth);
        pixels.set16(0, 0, [22119, 22119, 22119, 65535]);
        pixels.set16(1, 0, [51013, 51013, 51013, 65535]);
        crate::engine::Mask {
            enabled: true,
            linked: true,
            cache_key: crate::engine::id(),
            steps: vec![crate::engine::MaskStep {
                id: crate::engine::id(), kind: "paint".into(), enabled: true,
                weight: 1., value: 0., pixels, settings: Value::Null,
            }],
        }
    }

    pub(in crate::psd) fn color_overlay_raster(depth: u16) -> crate::raster::Raster {
        let mut pixels = crate::raster::Raster::new_depth(3, 1, depth);
        pixels.set16(0, 0, [51013, 43177, 23459, 65535]);
        pixels.set16(1, 0, [12347, 34571, 56789, 44913]);
        pixels.set16(2, 0, [34571, 45673, 56789, 0]);
        pixels
    }

    pub(in crate::psd) fn extend_color_overlay_stack(layer: &mut crate::engine::Layer) {
        for (enabled, blend, color) in [
            (false, "normal", [1., 0., 0.]),
            (true, "screen", [0.125, 0.75, 0.25]),
        ] {
            layer.effects.push(crate::effects::Effect {
                id: crate::engine::id(), kind: "color_overlay".into(), enabled, weight: 1.,
                settings: json!({"opacity":0.5, "blend":blend, "color":color}).into(),
            });
        }
    }

    pub(in crate::psd) fn standard_shadow_fixture(depth: u16) -> crate::effects::Effect {
        let mut settings = json!({"opacity":0.625, "color":[0.1875, 0.625, 0.90625],
            "distance":1., "size":0., "spread":0., "contour":"linear"});
        let kind = if depth == 8 {
            settings["angle"] = json!(-135.);
            settings["pixel_snap"] = json!(true);
            settings["blend"] = json!("multiply");
            "inner_shadow"
        } else {
            settings["angle"] = json!(0.);
            settings["source_knockout"] = json!(true);
            "drop_shadow"
        };
        crate::effects::Effect {
            id: crate::engine::id(), kind: kind.into(), enabled: true, weight: 1., settings: settings.into(),
        }
    }

    pub(in crate::psd) fn standard_inner_shadow_stack() -> [crate::effects::Effect; 3] {
        std::array::from_fn(|index| {
            let mut effect = standard_shadow_fixture(8);
            effect.enabled = index != 1;
            let settings = std::sync::Arc::make_mut(&mut effect.settings);
            settings["opacity"] = json!([0.625, 0.25, 0.5][index]);
            effect
        })
    }

    pub(in crate::psd) fn standard_gradient_overlay_fixture(depth: u16) -> crate::effects::Effect {
        let mut settings = super::super::gradient_fill::tests::settings();
        settings.as_object_mut().unwrap().remove("use_gradient_alpha");
        settings["opacity"] = json!(0.625);
        settings["blend"] = json!("multiply");
        settings["offset_x"] = json!(0.125);
        settings["offset_y"] = json!(-0.0625);
        settings.as_object_mut().unwrap().remove("align_layer");
        if depth == 8 {
            settings.as_object_mut().unwrap().remove("pixel_grid");
        } else {
            settings["style"] = json!("radial");
        }
        crate::effects::Effect {
            id: crate::engine::id(),
            kind: "gradient_overlay".into(),
            enabled: true,
            weight: 1.,
            settings: settings.into(),
        }
    }

    pub(in crate::psd) fn standard_gradient_stack_fixture(depth: u16) -> [crate::effects::Effect; 3] {
        std::array::from_fn(|index| {
            let mut effect = standard_gradient_overlay_fixture(depth);
            effect.enabled = index != 1;
            let settings = std::sync::Arc::make_mut(&mut effect.settings);
            let (opacity, angle, blend) = [(0.625, 0., "multiply"), (0.25, 90., "screen"), (0.5, -90., "overlay")][index];
            settings["opacity"] = json!(opacity);
            settings["angle"] = json!(angle);
            settings["blend"] = json!(blend);
            effect
        })
    }

    pub(in crate::psd) fn standard_pattern_overlay_fixture(depth: u16) -> crate::effects::Effect {
        use base64::{engine::general_purpose::STANDARD, Engine as _};
        let png = if depth == 16 {
            crate::raster::png16(2, 1, &[12347, 23459, 34571, 40001, 19277, 30671, 40991, 0])
        } else {
            crate::raster::png(2, 1, &[47, 103, 211, 121, 199, 81, 35, 0])
        }.unwrap();
        crate::effects::Effect {
            id: crate::engine::id(),
            kind: "pattern_overlay".into(),
            enabled: true,
            weight: 1.,
            settings: json!({"opacity":0.625,"blend":"multiply","pattern":"bitmap",
                "bitmap_png":STANDARD.encode(png),"bitmap_scale":1.,"bitmap_angle":0.,
                "phase_x":0.,"phase_y":0.,"sampling":"nearest"}).into(),
        }
    }

    pub(in crate::psd) fn standard_stroke_fixture(position: &str) -> crate::effects::Effect {
        crate::effects::Effect {
            id: crate::engine::id(), kind: "stroke".into(), enabled: true, weight: 1.,
            settings: json!({"opacity":0.625, "color":[0.1875, 0.625, 0.90625],
                "blend":"normal", "size":1., "position":position}).into(),
        }
    }

    pub(in crate::psd) fn standard_inside_stroke_stack() -> [crate::effects::Effect; 3] {
        std::array::from_fn(|index| {
            let mut effect = standard_stroke_fixture("inside");
            effect.enabled = index != 1;
            let settings = std::sync::Arc::make_mut(&mut effect.settings);
            settings["size"] = json!((index + 1) as f64);
            settings["color"] = json!([[0.125, 0.25, 0.5], [0.5, 0.125, 0.25], [0.25, 0.5, 0.125]][index]);
            effect
        })
    }

    #[test]
    fn maximum_style_lists_fit_the_reader() {
        let mut layer = crate::engine::Layer::new("Gradient stack", "paint", 2, 1);
        for index in 0..MAX_GRADIENT_OVERLAYS {
            let mut effect = standard_gradient_overlay_fixture(8);
            effect.enabled = index % 3 != 1;
            let settings = std::sync::Arc::make_mut(&mut effect.settings);
            settings["offset_x"] = json!(index as f64 / 16.);
            for key in ["color_stops", "alpha_stops"] {
                let endpoints = settings[key].as_array().unwrap();
                let stops = (0..16).map(|stop| {
                    let last = stop == 15;
                    let mut value = endpoints[usize::from(last)].clone();
                    value["position"] = json!(if last { 1. } else { stop as f64 / 16. });
                    value
                }).collect::<Vec<_>>();
                settings[key] = json!(stops);
            }
            layer.effects.push(effect);
        }
        for format in [
            super::super::MULTI_GRADIENT_OVERLAY_FORMAT,
            super::super::MULTI_INNER_SHADOW_FORMAT,
            super::super::MULTI_INSIDE_STROKE_FORMAT,
        ] {
            layer.effects.truncate(MAX_GRADIENT_OVERLAYS);
            if format == super::super::MULTI_GRADIENT_OVERLAY_FORMAT {
                for _ in 0..MAX_COLOR_OVERLAYS / 2 {
                    extend_color_overlay_stack(&mut layer);
                }
            } else if format == super::super::MULTI_INNER_SHADOW_FORMAT {
                layer.effects.extend((0..MAX_INNER_SHADOWS).map(|_| standard_shadow_fixture(8)));
            } else {
                layer.effects.extend((0..MAX_INSIDE_STROKES).map(|_| standard_stroke_fixture("inside")));
            }
            let recipe = standard_styles((&layer).into(), 2, 1, None).unwrap();
            assert_eq!(recipe.tag(), *b"lmfx");
            assert_eq!(recipe.source_format(), format);
            let mut tag = Vec::new();
            write_standard_style_tag(&mut tag, &recipe).unwrap();
            let len = u32::from_be_bytes(tag[8..12].try_into().unwrap()) as usize;
            let payload = &tag[12..12 + len];
            assert_eq!(tag.len(), 12 + len.next_multiple_of(2));
            assert!(matches_descriptor(&recipe, payload));
            let (opened, pattern) = native_styles(payload, &[], 8, None, &mut |_| Ok(())).unwrap();
            assert!(pattern.is_none());
            assert_eq!(opened.len(), MAX_LAYER_EFFECTS);
            for (actual, expected) in opened.iter().zip(&layer.effects) {
                assert_eq!(actual.kind, expected.kind);
                assert_eq!(actual.settings, expected.settings);
                assert_eq!(actual.enabled, expected.enabled);
            }
            layer.effects.swap(0, 1);
            let reordered = standard_styles((&layer).into(), 2, 1, None).unwrap();
            assert!(!matches_descriptor(&reordered, payload));
            layer.effects.swap(0, 1);
        }
    }

    #[test]
    fn stroke_lists_refuse_exterior_positions_and_excess_definitions() {
        let mut layer = crate::engine::Layer::new("Strokes", "paint", 2, 1);
        layer.effects = standard_inside_stroke_stack().into();
        let recipe = standard_styles((&layer).into(), 2, 1, None).unwrap();
        let mut payload = Vec::new();
        write_descriptor(&mut payload, &recipe).unwrap();
        let position = payload.windows(4).position(|bytes| bytes == b"InsF").unwrap();
        payload[position..position + 4].copy_from_slice(b"OutF");
        assert!(native_styles(&payload, &[], 8, None, &mut |_| Ok(())).is_err());

        layer.effects[0] = standard_stroke_fixture("outside");
        assert!(standard_styles((&layer).into(), 2, 1, None).is_none());
        layer.effects = (0..=MAX_INSIDE_STROKES).map(|_| standard_stroke_fixture("inside")).collect();
        assert!(standard_styles((&layer).into(), 2, 1, None).is_none());
    }

    #[test]
    fn standard_solid_stroke_preserves_positions_and_refuses_incompatible_casts() {
        use crate::engine::Layer;
        use std::sync::Arc;

        for depth in [8, 16] {
            let mut layer = Layer::new("Stroke", "paint", 3, 1);
            layer.pixels = color_overlay_raster(depth);
            for position in ["inside", "center", "outside"] {
                for size in [0., 1., crate::effects::styles::MAX_STROKE_SIZE] {
                    for enabled in [false, true] {
                        let mut effect = standard_stroke_fixture(position);
                        effect.enabled = enabled;
                        Arc::make_mut(&mut effect.settings)["size"] = json!(size);
                        layer.effects = vec![effect];
                        let recipe = standard_styles((&layer).into(), 2, 1, None).unwrap();
                        assert_eq!(recipe.source_format(), super::super::STROKE_STYLE_FORMAT);
                        assert_eq!(recipe.tag(), *b"lfx2");
                        let mut descriptor = Vec::new();
                        write_descriptor(&mut descriptor, &recipe).unwrap();
                        let (opened, pattern) = native_styles(&descriptor, &[], depth, None, &mut |_| Ok(())).unwrap();
                        assert_eq!(pattern, None);
                        assert_eq!(opened.len(), 1);
                        assert_eq!(opened[0].kind, layer.effects[0].kind);
                        assert_eq!(opened[0].settings, layer.effects[0].settings);
                        assert_eq!(opened[0].enabled, enabled);
                    }
                }
            }
            layer.effects = vec![standard_stroke_fixture("outside"), standard_shadow_fixture(16)];
            assert!(standard_styles((&layer).into(), 2, 1, None).is_none());
            for disabled in [0, 1] {
                layer.effects[disabled].enabled = false;
                assert!(standard_styles((&layer).into(), 2, 1, None).is_some());
                layer.effects[disabled].enabled = true;
            }
            layer.effects.truncate(1);
            for size in [0.5, crate::effects::styles::MAX_STROKE_SIZE + 1.] {
                Arc::make_mut(&mut layer.effects[0].settings)["size"] = json!(size);
                assert!(standard_styles((&layer).into(), 2, 1, None).is_none());
            }
            layer.effects[0] = standard_stroke_fixture("inside");
            Arc::make_mut(&mut layer.effects[0].settings)["blend"] = json!("multiply");
            assert!(standard_styles((&layer).into(), 2, 1, None).is_none());

            // Exercise the fixed descriptor capacity and enabled count at its full admitted stack.
            let overlay = crate::effects::Effect {
                id: crate::engine::id(), kind: "color_overlay".into(), enabled: true, weight: 1.,
                settings: json!({"opacity":0.5, "blend":"normal", "color":[1., 0., 0.]}).into(),
            };
            layer.effects = (0..MAX_COLOR_OVERLAYS).map(|_| {
                crate::effects::Effect { id: crate::engine::id(), ..overlay.clone() }
            }).collect();
            layer.effects.extend([standard_shadow_fixture(8), standard_stroke_fixture("inside"), standard_shadow_fixture(16)]);
            let recipe = standard_styles((&layer).into(), 2, 1, None).unwrap();
            assert_eq!(recipe.tag(), *b"lmfx");
            let mut descriptor = Vec::new();
            write_descriptor(&mut descriptor, &recipe).unwrap();
            let (opened, _) = native_styles(&descriptor, &[], depth, None, &mut |_| Ok(())).unwrap();
            assert_eq!(opened.len(), layer.effects.len());
            assert!(opened.iter().zip(&layer.effects).all(|(a, b)| a.kind == b.kind && a.settings == b.settings && a.enabled == b.enabled));
            layer.effects.swap(MAX_COLOR_OVERLAYS, MAX_COLOR_OVERLAYS + 1);
            assert!(standard_styles((&layer).into(), 2, 1, None).is_none());
        }
    }

    #[test]
    fn color_overlay_descriptors_round_trip_canonical_blend_ids_and_keep_read_aliases() {
        for &(mode, canonical, aliases) in OVERLAY_BLEND_IDS {
            let mut effects = [StyleEffect::default(); MAX_LAYER_EFFECTS];
            effects[0] = StyleEffect {
                color: [47., 103.5, 211.], opacity: 62.5, blend_id: canonical, enabled: true, shadow: None, stroke: None,
            };
            let overlay = StandardStyles {
                pattern: None, gradients: [None; MAX_GRADIENT_OVERLAYS], effects, count: 1, overlays: 1, inner_shadows: 0, strokes: 0, mask_linked: None,
                layer_blend: *b"norm", layer_opacity: 255, interior_effects: None,
                fill: true, bounds: [0, 0, 2, 1],
            };
            let mut bytes = Vec::new();
            write_descriptor(&mut bytes, &overlay).unwrap();
            let decoded = root(&bytes).unwrap();
            let effects = color_overlay_root(&decoded).unwrap();
            assert_eq!(effects[0].settings["blend"], mode);
            assert_eq!(effects[0].settings["opacity"], 0.625);
            assert_eq!(effects[0].settings["color"][1], 103.5 / 255.);
            for id in std::iter::once(canonical).chain(aliases.iter().copied()) {
                let value = |string_id| json!({"type":"enum", "value":["BlnM", id], "string_id":string_id});
                assert_eq!(overlay_blend(&value(id.len() != 4)).unwrap(), mode);
                assert!(overlay_blend(&value(id.len() == 4)).is_err());
            }
        }
    }
    fn id(out: &mut Vec<u8>, name: &str) {
        out.extend(
            (if name.len() == 4 {
                0
            } else {
                name.len() as u32
            })
            .to_be_bytes(),
        );
        out.extend(name.as_bytes());
    }
    pub(in crate::psd) fn descriptor(class: &str, items: Vec<(&str, &[u8; 4], Vec<u8>)>) -> Vec<u8> {
        let mut out = 1u32.to_be_bytes().to_vec();
        out.extend([0, 0]);
        id(&mut out, class);
        out.extend((items.len() as u32).to_be_bytes());
        for (key, kind, data) in items {
            id(&mut out, key);
            out.extend(kind);
            out.extend(data);
        }
        out
    }
    fn effect(enabled: bool) -> Vec<u8> {
        descriptor(
            "DrSh",
            vec![
                ("enab", b"bool", vec![u8::from(enabled)]),
                ("present", b"bool", vec![1]),
                ("showInDialog", b"bool", vec![1]),
            ],
        )
    }
    fn payload(master: bool) -> Vec<u8> {
        let mut list = 2u32.to_be_bytes().to_vec();
        for enabled in [true, false] {
            list.extend(b"Objc");
            list.extend(effect(enabled));
        }
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor(
            "null",
            vec![
                ("masterFXSwitch", b"bool", vec![u8::from(master)]),
                ("dropShadowMulti", b"VlLs", list),
                (
                    "SoFi",
                    b"Objc",
                    descriptor("SoFi", vec![("enab", b"bool", vec![0])]),
                ),
            ],
        ));
        out
    }
    #[test]
    fn descriptors_report_multiple_and_disabled_definitions_without_creating_effects() {
        for master in [true, false] {
            let bytes = payload(master);
            let report = inspect(&bytes).unwrap();
            assert!(report.iter().any(|v|v=="Photoshop Drop Shadow: 2 definitions; 1 enabled, 1 disabled, 0 unspecified; 2 marked present"));
            assert!(report
                .iter()
                .any(|v| v.contains("Color Overlay: 1 definitions; 0 enabled, 1 disabled")));
            assert_eq!(
                report[0],
                format!(
                    "Photoshop style master switch: {}",
                    if master { "enabled" } else { "disabled" }
                )
            );
            for padding in 0..=3 {
                let mut padded = bytes.clone();
                padded.extend(vec![0; padding]);
                assert_eq!(inspect(&padded).unwrap(), report);
            }
        }
    }
    #[test]
    fn truncated_or_malformed_descriptors_are_bounded_errors_not_panics() {
        let original = payload(true);
        for end in 0..original.len() {
            assert!(inspect(&original[..end]).is_err(), "{end}");
        }
        let mut invalid = original.clone();
        invalid[4..8].copy_from_slice(&17u32.to_be_bytes());
        assert!(inspect(&invalid).is_err());
        let mut invalid = original.clone();
        invalid.push(1);
        assert!(inspect(&invalid).is_err());
        let mut invalid = original.clone();
        invalid.extend([0; 4]);
        assert!(inspect(&invalid).is_err());
        let mut invalid = original.clone();
        invalid[12..14].copy_from_slice(&0xd800u16.to_be_bytes());
        assert!(inspect(&invalid).unwrap_err().contains("Unicode"));
        for value in [2, 255] {
            let mut invalid = original.clone();
            let at = invalid.windows(4).position(|v| v == b"bool").unwrap() + 4;
            invalid[at] = value;
            assert!(inspect(&invalid).unwrap_err().contains("boolean"));
        }
        let mut invalid = original.clone();
        let at = invalid.windows(4).position(|v| v == b"bool").unwrap();
        invalid[at..at + 4].copy_from_slice(b"obj ");
        assert!(inspect(&invalid).unwrap_err().contains("value type"));
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor(
            "null",
            vec![("same", b"bool", vec![0]), ("same", b"bool", vec![1])],
        ));
        assert!(inspect(&out).unwrap_err().contains("duplicate"));
    }
    #[test]
    fn unknown_properties_and_opaque_content_stay_protected_and_do_not_expose_values() {
        let mut raw = 7u32.to_be_bytes().to_vec();
        raw.extend(b"private");
        let mut units = b"#Prc".to_vec();
        units.extend(80f64.to_be_bytes());
        let mut text = 7u32.to_be_bytes().to_vec();
        for c in "private".encode_utf16() {
            text.extend(c.to_be_bytes());
        }
        let mut enum_value = vec![];
        id(&mut enum_value, "BlnM");
        id(&mut enum_value, "Nrml");
        let values = descriptor(
            "FutureStyle",
            vec![
                ("data", b"tdta", raw.clone()),
                ("alias", b"alis", raw),
                ("TEXT", b"TEXT", text),
                ("large", b"comp", (-7i64).to_be_bytes().to_vec()),
                ("unit", b"UntF", units),
                ("enum", b"enum", enum_value),
                ("number", b"doub", 2.5f64.to_be_bytes().to_vec()),
                ("integer", b"long", (-3i32).to_be_bytes().to_vec()),
                ("global", b"GlbO", effect(true)),
            ],
        );
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor("null", vec![("FutureStyle", b"Objc", values)]));
        let report = inspect(&out).unwrap().join("\n");
        assert!(report.contains("Unrecognized Photoshop style property: FutureStyle"));
        assert!(!report.contains("private"));
        let at = out.windows(4).position(|v| v == b"doub").unwrap() + 4;
        out[at..at + 8].copy_from_slice(&f64::NAN.to_be_bytes());
        assert!(inspect(&out).unwrap_err().contains("non-finite"));
    }
    #[test]
    fn descriptor_byte_depth_item_and_identifier_budgets_reject_before_unbounded_allocation() {
        assert!(inspect(&vec![0; 1024 * 1024 + 1])
            .unwrap_err()
            .contains("1 MiB"));
        let mut nested = descriptor("null", vec![]);
        for _ in 0..34 {
            nested = descriptor("null", vec![("nest", b"Objc", nested)]);
        }
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(nested);
        assert!(inspect(&out).unwrap_err().contains("budget"));
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(4097u32.to_be_bytes());
        assert!(inspect(&out).unwrap_err().contains("code units"));
        let mut list = 257u32.to_be_bytes().to_vec();
        list.extend([0; 4]);
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor("null", vec![("list", b"VlLs", list)]));
        assert!(inspect(&out).unwrap_err().contains("256 items"));
        let mut invalid = payload(true);
        let at = invalid
            .windows(14)
            .position(|v| v == b"masterFXSwitch")
            .unwrap();
        invalid[at..at + 1].copy_from_slice(&[0]);
        assert!(inspect(&invalid).unwrap_err().contains("identifier"));
        let names = (0..17).map(|i| format!("list-{i}")).collect::<Vec<_>>();
        let mut list = 256u32.to_be_bytes().to_vec();
        for _ in 0..256 {
            list.extend(b"bool");
            list.push(1);
        }
        let items = names
            .iter()
            .map(|n| (n.as_str(), b"VlLs", list.clone()))
            .collect();
        let mut out = vec![0; 4];
        out.extend(16u32.to_be_bytes());
        out.extend(descriptor("null", items));
        assert!(inspect(&out).unwrap_err().contains("item budget"));
    }

    fn replace_style(
        bytes: &[u8],
        depth: u16,
        layout: super::super::Layout,
        payload: &[u8],
    ) -> Vec<u8> {
        use super::super::{tag, Layout};
        let mut r = Cursor::new(bytes);
        r.bytes(26).unwrap();
        r.block().unwrap();
        r.block().unwrap();
        let section_length = r.pos;
        let section = layout.block(&mut r).unwrap();
        let section_start = r.pos - section.len();
        let mut info = Cursor::new(section);
        let (length_at, start) = if depth == 8 {
            let data = layout.block(&mut info).unwrap();
            (section_start, section_start + info.pos - data.len())
        } else {
            layout.block(&mut info).unwrap();
            info.block().unwrap();
            assert_eq!(info.bytes(8).unwrap(), b"8BIMMt16");
            assert!(layout.tag_block(&mut info, b"Mt16").unwrap().is_empty());
            assert_eq!(info.bytes(4).unwrap(), b"8BIM");
            assert_eq!(info.bytes(4).unwrap(), b"Lr16");
            let length = section_start + info.pos;
            let data = layout.tag_block(&mut info, b"Lr16").unwrap();
            (length, section_start + info.pos - data.len())
        };
        let mut layer = Cursor::new(&bytes[start..]);
        assert_eq!(layer.i16().unwrap(), -1);
        layer.bytes(16).unwrap();
        let channels = layer.u16().unwrap();
        for _ in 0..channels {
            layer.i16().unwrap();
            layer.length(layout.large()).unwrap();
        }
        layer.bytes(12).unwrap();
        let extra_length = start + layer.pos;
        let extra = layer.block().unwrap();
        let extra_start = start + layer.pos - extra.len();
        let at = extra.windows(8).position(|v| v == b"8BIMfxrp").unwrap() + extra_start;
        let old_len = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        let mut replacement = vec![];
        tag(&mut replacement, b"lmfx", payload);
        let change = replacement.len() as i64 - (12 + old_len + (old_len % 2)) as i64;
        let mut result = bytes.to_vec();
        result.splice(at..at + 12 + old_len + (old_len % 2), replacement);
        let mut update = |at: usize, wide: bool| {
            let size = if wide { 8 } else { 4 };
            let original = u64::from_be_bytes({
                let mut v = [0; 8];
                v[8 - size..].copy_from_slice(&bytes[at..at + size]);
                v
            });
            let value = (original as i64 + change) as u64;
            result[at..at + size].copy_from_slice(&value.to_be_bytes()[8 - size..]);
        };
        update(extra_length, false);
        update(length_at, layout == Layout::Psb);
        update(section_length, layout == Layout::Psb);
        result
    }
    #[test]
    fn modern_styles_keep_psd_psb_native_composites_and_source_reports_protected() {
        use crate::{
            engine::{Document, Engine},
            psd, server,
        };
        for depth in [8, 16] {
            for layout in [super::super::Layout::Psd, super::super::Layout::Psb] {
                let mut doc = Document::new_depth(3, 2, depth).unwrap();
                doc.layers[0]
                    .pixels
                    .set16(1, 1, [12347, 23459, 34571, 65535]);
                doc.layers[0].psd_metadata = vec![psd::LayerMetadata {
                    key: "fxrp".into(),
                    data: vec![0; 16],
                }];
                let baseline = doc.export_png().unwrap();
                let encoded = if layout == super::super::Layout::Psd {
                    psd::encode(&doc)
                } else {
                    psd::encode_psb(&doc)
                }
                .unwrap();
                for malformed in [false, true] {
                    let mut descriptor = payload(true);
                    descriptor.resize((descriptor.len() + 3) & !3, 0);
                    if malformed {
                        descriptor[4..8].copy_from_slice(&99u32.to_be_bytes());
                    }
                    let original = replace_style(&encoded, depth, layout, &descriptor);
                    let protected = psd::decode(&original).unwrap();
                    assert!(protected.read_only);
                    assert_eq!(protected.bit_depth, depth);
                    assert!(protected.layers[0].effects.is_empty());
                    assert_eq!(protected.export_png().unwrap(), baseline);
                    assert!(protected.source_structure[0].tags.contains(&"lmfx".into()));
                    assert!(
                        protected.source_structure[0]
                            .warnings
                            .iter()
                            .any(|v| v.contains(if malformed {
                                "Cannot inspect"
                            } else {
                                "Drop Shadow: 2 definitions"
                            })),
                        "{:?}",
                        protected.warnings
                    );
                    assert!(psd::encode(&protected).is_err());
                    let mut engine = Engine::new();
                    engine.resident_state_mut().expect("resident fixture").document = protected;
                    let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                    assert!(engine
                        .edit(
                            "human",
                            &[json!({"op":"layer.delete","layer":layer})],
                            None,
                            None,
                            "Refuse protected edits"
                        )
                        .is_err());
                    let shared = std::sync::Arc::new(std::sync::Mutex::new(engine));
                    let id = shared.lock().unwrap().resident_state().expect("resident fixture").document.id.clone();
                    let report = server::dispatch(
                        &shared,
                        "document",
                        &json!({"action":"save_report","document_id":id,"expected_revision":0}),
                    )
                    .unwrap();
                    assert_eq!(report["save_allowed"], false);
                    assert_eq!(shared.lock().unwrap().resident_state().expect("resident fixture").document.export_png().unwrap(), baseline);
                    assert!(shared.lock().unwrap().resident_state().expect("resident fixture").undo.is_empty());
                    assert_eq!(
                        psd::decode(&original).unwrap().export_png().unwrap(),
                        baseline
                    );
                }
            }
        }
    }
}
