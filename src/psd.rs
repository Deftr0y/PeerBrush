//! A bounded PSD v1 codec. Unsupported Photoshop features are never silently rewritten.
use crate::{
    engine::{id, Document, Layer, Mask, MaskStep},
    raster::{check_size, Raster},
};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::{borrow::Cow, io::Read};

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
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ *b as u64).wrapping_mul(0x100000001b3)
    })
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

// The shared compositor implements Photoshop's default grouped raster clipping.
// More complicated units keep their standard baked appearance until supported.
fn standard_clipping(doc: &Document) -> bool {
    doc.layers.iter().all(|layer| {
        layer.clip_to.as_ref().is_none_or(|base| {
            ["paint", "fill"].contains(&layer.kind.as_str())
                && doc
                    .layers
                    .iter()
                    .any(|l| l.id == *base && ["paint", "fill"].contains(&l.kind.as_str()))
        })
    })
}

fn default_blending_ranges(bytes: &[u8]) -> bool {
    bytes.is_empty()
        || (bytes.len() <= 32
            && bytes.len() % 8 == 0
            && bytes.chunks_exact(4).all(|range| range == [0, 0, 255, 255]))
}

fn standard_flag(key: &[u8], bytes: &[u8]) -> Result<(), &'static str> {
    let boolean = bytes.len() == 4 && bytes[0] <= 1 && bytes[1..] == [0; 3];
    match key {
        b"clbl" if boolean && bytes[0] == 1 => Ok(()),
        b"clbl" => Err("Blend Clipped Layers As Group disabled or invalid (clbl)"),
        b"knko" if bytes == [0; 4] => Ok(()),
        b"knko" => Err("Photoshop knockout blending (knko)"),
        b"iOpa"
            if (bytes.len() == 1 || bytes.len() == 4)
                && bytes[0] == 255
                && bytes[1..].iter().all(|v| *v == 0) =>
        {
            Ok(())
        }
        b"iOpa" => Err("Photoshop fill opacity (iOpa)"),
        b"lspf" if bytes == [0; 4] => Ok(()),
        b"lspf" => Err("Photoshop layer protection flags (lspf)"),
        // Interior effects and vector sources are separately protected. These
        // validated flags do not alter ordinary integer-positioned raster layers.
        b"infx" if boolean => Ok(()),
        b"sn2P" if bytes.len() == 4 && u32::from_be_bytes(bytes.try_into().unwrap()) <= 1 => Ok(()),
        b"lyid" if bytes.len() == 4 => Ok(()),
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
    match entry.key.as_str() {
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
        "lclr" if entry.data.len() == 8 => {
            if u16::from_be_bytes(entry.data[..2].try_into().unwrap()) <= 8
                && entry.data[2..].iter().all(|v| *v == 0)
            {
                Ok(())
            } else {
                Err(invalid())
            }
        }
        "lnsr" if entry.data.len() == 4 => Ok(()),
        "shmd" => static_timestamp_metadata(&entry.data),
        _ => Err(invalid()),
    }
}

pub fn validate_metadata(entries: &[LayerMetadata]) -> Result<(), String> {
    if entries.len() > 4 {
        return Err("Photoshop layer metadata exceeds its bounded tag count".into());
    }
    let mut keys = std::collections::HashSet::new();
    for entry in entries {
        if !keys.insert(entry.key.as_str()) {
            return Err("Duplicate Photoshop layer metadata tag".into());
        }
        validate_metadata_entry(entry)?;
    }
    Ok(())
}

/// Respect Photoshop's per-item copy-on-duplication flag; never clone unique IDs.
pub fn metadata_for_duplicate(entries: &[LayerMetadata]) -> Vec<LayerMetadata> {
    entries
        .iter()
        .filter(|entry| entry.key != "shmd" || entry.data.get(12) == Some(&1))
        .cloned()
        .collect()
}

#[derive(Serialize, Deserialize)]
struct Embedded {
    format: u32,
    standard_hash: u64,
    document: Document,
}
struct BoundedWriter<W> {
    inner: W,
    written: usize,
    limit: usize,
}
impl<W: std::io::Write> std::io::Write for BoundedWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.written.saturating_add(bytes.len()) > self.limit {
            return Err(std::io::Error::other(
                "Editable PSD source data exceeds its bounded serialization budget",
            ));
        }
        let n = self.inner.write(bytes)?;
        self.written += n;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

pub fn encode(doc: &Document) -> Result<Vec<u8>, String> {
    if doc.read_only {
        return Err(
            "Cannot save an unsupported PSD without explicitly creating a compatible copy".into(),
        );
    }
    validate(doc)?;
    let high = doc.bit_depth == 16;
    let masks = if high {
        vec![None; doc.layers.len()]
    } else {
        doc.layers
            .iter()
            .map(|l| {
                l.mask
                    .as_ref()
                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
            })
            .collect::<Vec<_>>()
    };
    let colors = if high {
        vec![None; doc.layers.len()]
    } else {
        crate::effects::prepare(doc, &masks)?
    };
    let colors16 = if high {
        Some(crate::depth16::prepare(doc)?)
    } else {
        None
    };
    let masks16 = if high {
        Some(crate::depth16::prepare_masks(doc)?)
    } else {
        None
    };
    let composite16 = if high {
        Some(crate::depth16::render(doc)?)
    } else {
        None
    };
    // Ordinary raster clipping remains standard, editable PSD layer data.
    // Adjustments and unsupported clipping units still need a standard bake.
    let baked = if !doc.filters.is_empty()
        || doc.layers.iter().any(|l| l.kind == "adjustment")
        || !standard_clipping(doc)
    {
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
    let standard_sources = if !doc.filters.is_empty() {
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
    fn order<'a>(doc: &'a Document, parent: Option<&str>, out: &mut Vec<(&'a Layer, u32)>) {
        for l in doc.layers.iter().filter(|l| l.parent.as_deref() == parent) {
            if l.kind == "group" && crate::effects::active(l) {
                // A group effect is baked as one standard raster layer; embedded sources retain its editable children.
                out.push((l, 0));
            } else if l.kind == "group" {
                out.push((l, 1));
                order(doc, Some(&l.id), out);
                out.push((l, 3));
            } else {
                out.push((l, 0));
            }
        }
    }
    if let Some(layer) = &baked {
        ordered.push((layer, 0));
        if let Some(source) = &standard_sources {
            order(source, None, &mut ordered);
        }
    } else {
        order(doc, None, &mut ordered);
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
    let mut channels = vec![];
    for (l, section) in &ordered {
        let baked_group = *section == 0 && l.kind == "group" && crate::effects::active(l);
        let (w, h) = if *section == 0 {
            if baked_group {
                (doc.width, doc.height)
            } else {
                (l.pixels.width, l.pixels.height)
            }
        } else {
            (0, 0)
        };
        let (x, y) = if *section == 0 && !baked_group {
            (l.x, l.y)
        } else {
            (0, 0)
        };
        i32b(&mut records, y);
        i32b(&mut records, x);
        i32b(&mut records, y + h as i32);
        i32b(&mut records, x + w as i32);
        let mask = l.mask.as_ref();
        // Folder masks cover the standard canvas; private sources retain off-canvas frames.
        let (mask_w, mask_h, mask_x, mask_y) = if l.kind == "group" {
            (doc.width, doc.height, 0, 0)
        } else {
            (l.pixels.width, l.pixels.height, l.x, l.y)
        };
        let has_mask = mask.is_some() && *section != 3;
        let prepared = if has_mask {
            mask.and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
        } else {
            None
        };
        let index = doc.layers.iter().position(|n| n.id == l.id);
        let derived = index.and_then(|i| colors[i].as_ref());
        let derived16 = index.and_then(|i| colors16.as_ref().and_then(|c| c[i].as_ref()));
        let mask16 = index.and_then(|i| masks16.as_ref().and_then(|m| m[i].as_ref()));
        u16b(&mut records, if has_mask { 5 } else { 4 });
        for ch in [-1i16, 0, 1, 2]
            .into_iter()
            .chain(if has_mask { Some(-2) } else { None })
        {
            let mut data = vec![];
            u16b(&mut data, 0);
            let (cw, ch_height) = if ch == -2 { (mask_w, mask_h) } else { (w, h) };
            for yy in 0..ch_height {
                for xx in 0..cw {
                    if high {
                        let value = if ch == -2 {
                            (crate::depth16::mask_value_prepared(
                                l,
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
                        u16b(&mut data, value);
                    } else {
                        data.push(if ch == -2 {
                            (l.mask_value_prepared(
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
                        });
                    }
                }
            }
            u16b(&mut records, ch as u16);
            u32b(&mut records, data.len() as u32);
            channels.extend(data);
        }
        records.extend_from_slice(b"8BIM");
        records.extend_from_slice(match l.blend.as_str() {
            "pass_through" if *section != 3 => b"pass",
            "multiply" => b"mul ",
            "screen" => b"scrn",
            "overlay" => b"over",
            "darken" => b"dark",
            "lighten" => b"lite",
            "linear_dodge" => b"lddg",
            "linear_burn" => b"lbrn",
            "color_dodge" => b"div ",
            "color_burn" => b"idiv",
            "hard_light" => b"hLit",
            "soft_light" => b"sLit",
            "difference" => b"diff",
            "exclusion" => b"smud",
            "subtract" => b"fsub",
            "divide" => b"fdiv",
            _ => b"norm",
        });
        records.push((l.opacity * 255.0).round() as u8);
        records.push(u8::from(l.clip_to.is_some()));
        records.push(if l.visible { 0 } else { 2 });
        records.push(0);
        let mut extra = vec![];
        if has_mask {
            let mut m = vec![];
            i32b(&mut m, mask_y);
            i32b(&mut m, mask_x);
            i32b(&mut m, mask_y + mask_h as i32);
            i32b(&mut m, mask_x + mask_w as i32);
            m.extend_from_slice(&[
                255,
                if mask.is_some_and(|m| !m.enabled) {
                    2
                } else {
                    0
                },
                0,
                0,
            ]);
            block(&mut extra, &m);
        } else {
            u32b(&mut extra, 0);
        }
        u32b(&mut extra, 0);
        let name = if *section == 3 {
            "</Layer group>"
        } else {
            &l.name
        };
        let n = name.as_bytes().len().min(255);
        extra.push(n as u8);
        extra.extend_from_slice(&name.as_bytes()[..n]);
        while extra.len() % 4 != 0 {
            extra.push(0);
        }
        let mut unicode = vec![];
        let utf16: Vec<u16> = name.encode_utf16().collect();
        u32b(&mut unicode, utf16.len() as u32);
        for c in utf16 {
            u16b(&mut unicode, c);
        }
        tag(&mut extra, b"luni", &unicode);
        if *section != 3
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
        if *section != 3 {
            for metadata in &l.psd_metadata {
                let key: &[u8; 4] = metadata
                    .key
                    .as_bytes()
                    .try_into()
                    .map_err(|_| "Invalid Photoshop metadata tag")?;
                tag(&mut extra, key, &metadata.data);
            }
        }
        block(&mut records, &extra);
    }
    let mut info = vec![];
    // A negative count identifies the merged fourth channel as transparency.
    u16b(&mut info, (-(ordered.len() as i16)) as u16);
    info.extend(records);
    info.extend(channels);
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut layer_section = vec![];
    if high {
        u32b(&mut layer_section, 0); // Primary8-bit layer information is empty.
        u32b(&mut layer_section, 0); // Global layer mask.
        layer_section.extend(b"8BIMLr16");
        block(&mut layer_section, &info);
        layer_section.resize(layer_section.len() + (4 - info.len() % 4) % 4, 0);
        layer_section.extend(b"8BIMMt16");
        u32b(&mut layer_section, 0);
    } else {
        block(&mut layer_section, &info);
        u32b(&mut layer_section, 0);
    }
    let (w, h) = (doc.width, doc.height);
    let mut composite = vec![];
    u16b(&mut composite, 0);
    if let Some(image) = &composite16 {
        for c in 0..4 {
            for p in image.words.chunks_exact(4) {
                let value = if c < 3 {
                    ((p[c] as u64 * p[3] as u64 + 65535 * (65535 - p[3] as u64) + 32767) / 65535)
                        as u16
                } else {
                    p[3]
                };
                u16b(&mut composite, value);
            }
        }
    } else {
        let (_, _, rgba, _) = doc.preview(None, w.max(h), None, false)?;
        for c in 0..4 {
            for p in rgba.chunks_exact(4) {
                composite.push(if c < 3 {
                    ((p[c] as u32 * p[3] as u32 + 255 * (255 - p[3] as u32) + 127) / 255) as u8
                } else {
                    p[3]
                });
            }
        }
    }
    let embedded = Embedded {
        // Older readers cannot interpret soft selections or smooth curve sources.
        format: if !doc.filters.is_empty() {
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
        standard_hash: hash(&layer_section) ^ hash(&composite),
        document: doc.clone(),
    };
    let mut z = BoundedWriter {
        inner: ZlibEncoder::new(Vec::new(), Compression::fast()),
        written: 0,
        limit: if high {
            512 * 1024 * 1024
        } else {
            128 * 1024 * 1024
        },
    };
    serde_json::to_writer(&mut z, &embedded).map_err(|e| e.to_string())?;
    let compressed = z.inner.finish().map_err(|e| e.to_string())?;
    let mut resources = vec![];
    resources.extend_from_slice(b"8BIM");
    u16b(&mut resources, 4000);
    resources.push(12);
    resources.extend_from_slice(b"PeerBrush.v1");
    resources.push(0);
    let mut payload = b"PBR1".to_vec();
    payload.extend(compressed);
    block(&mut resources, &payload);
    if payload.len() % 2 != 0 {
        resources.push(0);
    }
    let mut out = b"8BPS".to_vec();
    u16b(&mut out, 1);
    out.extend_from_slice(&[0; 6]);
    u16b(&mut out, 4);
    u32b(&mut out, h);
    u32b(&mut out, w);
    u16b(&mut out, doc.bit_depth);
    u16b(&mut out, 3);
    u32b(&mut out, 0);
    if doc.srgb_tagged {
        let profile = crate::color_profile::srgb_profile();
        resources.extend_from_slice(b"8BIM");
        u16b(&mut resources, 1039);
        resources.extend([0; 2]);
        block(&mut resources, profile);
        if profile.len() % 2 != 0 {
            resources.push(0);
        }
    }
    block(&mut out, &resources);
    block(&mut out, &layer_section);
    out.extend(composite);
    if out.len() > 256 * 1024 * 1024 {
        return Err("PSD output exceeds the initial 256 MiB file limit".into());
    }
    Ok(out)
}

fn decode_channel(data: &[u8], w: u32, h: u32) -> Result<Vec<u8>, String> {
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
            let mut lengths = vec![];
            for _ in 0..h {
                lengths.push(r.u16()? as usize);
            }
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
struct Record {
    layer: Layer,
    channels: Vec<(i16, usize)>,
    bounds: [i32; 4],
    mask_bounds: Option<[i32; 4]>,
    mask_default: u8,
    mask_enabled: bool,
    section: u32,
    clipped: bool,
}
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
                let (pw, ph) = if rec.layer.kind == "group" {
                    (w as i64, h as i64)
                } else {
                    (lw, lh)
                };
                let left = (b[1] as i64 - rec.bounds[1] as i64).clamp(0, pw);
                let right = (b[3] as i64 - rec.bounds[1] as i64).clamp(0, pw);
                let top = (b[0] as i64 - rec.bounds[0] as i64).clamp(0, ph);
                let bottom = (b[2] as i64 - rec.bounds[0] as i64).clamp(0, ph);
                if right > left && bottom > top {
                    bytes += (((right + tile - 1) / tile - left / tile)
                        * ((bottom + tile - 1) / tile - top / tile))
                        as u64
                        * unit;
                }
            }
        }
        if bytes > 512 * 1024 * 1024 {
            return Err("PSD decoded tiles exceed the 512 MiB raster-source budget".into());
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
    control: &crate::loading::Control,
) -> Result<Raster, String> {
    let mut kind = [0; 2];
    input.read_exact(&mut kind).map_err(|e| e.to_string())?;
    let kind = u16::from_be_bytes(kind);
    if kind > 3 {
        return Err("Unsupported PSD composite compression".into());
    }
    let lengths = if kind == 1 {
        let mut lengths = Vec::with_capacity(h as usize * count);
        for _ in 0..h as usize * count {
            let mut n = [0; 2];
            input.read_exact(&mut n).map_err(|e| e.to_string())?;
            lengths.push(u16::from_be_bytes(n) as usize);
        }
        lengths
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
    control.progress("Resolving saved composite transparency", 0, 1)?;
    // PSD compatibility colors are composited over white. Restore straight RGB in tiles.
    let max = if depth == 16 { 65535i64 } else { 255 };
    let restore = |pixel: &mut [u16]| {
        let a = pixel[3] as i64;
        for v in &mut pixel[..3] {
            *v = if a == 0 {
                0
            } else {
                (((*v as i64 + a - max) * max + a / 2) / a).clamp(0, max) as u16
            };
        }
    };
    for tile in raster.samples16.values_mut() {
        control.check()?;
        for pixel in std::sync::Arc::make_mut(tile).chunks_exact_mut(4) {
            restore(pixel);
        }
    }
    for tile in raster.tiles.values_mut() {
        control.check()?;
        for pixel in std::sync::Arc::make_mut(tile).chunks_exact_mut(4) {
            let mut p = pixel.try_into().map(|p: [u8; 4]| p.map(u16::from)).unwrap();
            restore(&mut p);
            pixel.copy_from_slice(&p.map(|v| v as u8));
        }
    }
    Ok(raster)
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
    decode_reader(
        &mut std::io::Cursor::new(data),
        &crate::loading::Control::default(),
    )
}
fn stream_layer_channel(
    bytes: &[u8],
    raster: &mut Raster,
    channel: usize,
    initialize: bool,
    control: &crate::loading::Control,
) -> Result<(), String> {
    if raster.width == 0 || raster.height == 0 {
        return Ok(());
    }
    let mut cursor = std::io::Cursor::new(bytes);
    let mut kind = [0; 2];
    cursor.read_exact(&mut kind).map_err(|e| e.to_string())?;
    let kind = u16::from_be_bytes(kind);
    if kind > 3 {
        return Err("Unsupported PSD channel compression".into());
    }
    let mut lengths = vec![];
    if kind == 1 {
        for _ in 0..raster.height {
            let mut n = [0; 2];
            cursor.read_exact(&mut n).map_err(|e| e.to_string())?;
            lengths.push(u16::from_be_bytes(n) as usize);
        }
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
    }
    Ok(())
}
/// Stream the composite into native tiles; never buffer the complete encoded file.
pub fn decode_reader<R: Read + std::io::Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
) -> Result<Document, String> {
    use std::io::SeekFrom;
    let length = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if length > 256 * 1024 * 1024 {
        return Err("Initial PSD file limit is 256 MiB".into());
    }
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    control.progress("Reading PSD header", 0, 1)?;
    let mut header = [0; 26];
    reader.read_exact(&mut header).map_err(|e| e.to_string())?;
    let mut r = Cursor::new(&header);
    if r.bytes(4)? != b"8BPS" || r.u16()? != 1 {
        return Err("Only PSD v1 is supported; PSB is not supported yet".into());
    }
    r.bytes(6)?;
    let channel_count = r.u16()? as usize;
    let h = r.u32()?;
    let w = r.u32()?;
    check_size(w, h)?;
    let depth = r.u16()?;
    let mode = r.u16()?;
    if ![8, 16].contains(&depth) || mode != 3 || !(3..=4).contains(&channel_count) {
        return Err("PeerBrush opens 8-bit and 16-bit RGB PSDs (3 or 4 channels); unsupported Photoshop source features remain protected".into());
    }
    read_section(reader, length, control, "Reading color data")?;
    let resources = read_section(reader, length, control, "Reading PSD resources")?;
    let layer_section = read_section(reader, length, control, "Reading layer sources")?;
    let transparency = merged_transparency(&layer_section, depth)?;
    let mut input = HashedReader {
        reader,
        hash: 0xcbf29ce484222325,
    };
    let merged = stream_composite(
        &mut input,
        w,
        h,
        depth,
        channel_count,
        transparency,
        control,
    )?;
    let mut remaining = [0; 65536];
    loop {
        control.check()?;
        if input.read(&mut remaining).map_err(|e| e.to_string())? == 0 {
            break;
        }
    }
    decode_parts(
        &resources,
        &layer_section,
        w,
        h,
        depth,
        merged,
        input.hash,
        control,
    )
}
fn read_section<R: Read + std::io::Seek>(
    reader: &mut R,
    length: u64,
    control: &crate::loading::Control,
    stage: &str,
) -> Result<Vec<u8>, String> {
    let mut n = [0; 4];
    reader.read_exact(&mut n).map_err(|e| e.to_string())?;
    let n = u32::from_be_bytes(n) as usize;
    let position = reader.stream_position().map_err(|e| e.to_string())?;
    if n as u64 > length.saturating_sub(position) {
        return Err("Truncated PSD section".into());
    }
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
fn decode_parts(
    resources: &[u8],
    layer_section: &[u8],
    w: u32,
    h: u32,
    depth: u16,
    merged: Raster,
    composite_hash: u64,
    control: &crate::loading::Control,
) -> Result<Document, String> {
    let mut rr = Cursor::new(resources);
    let mut embedded = None;
    let mut warnings = vec![];
    let mut has_merged_composite = true;
    let mut icc_profile = None;
    let mut seen_profile = false;
    let mut srgb_tagged = false;
    while rr.pos < resources.len() {
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
        if key == 4000 && payload.starts_with(b"PBR1") {
            embedded = Some(&payload[4..]);
        }
        // VersionInfo marks a missing compatibility composite explicitly.
        if key == 1057 && payload.len() >= 5 && payload[..4] == [0, 0, 0, 1] {
            has_merged_composite = payload[4] != 0;
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
            } else {
                match crate::color_profile::supported(payload) {
                    Ok(()) => warnings.push("Embedded RGB color profiles use an sRGB preview. Convert to an sRGB copy to edit; original samples and profile remain protected".into()),
                    Err(reason) => warnings.push(format!("Embedded color profiles are protected; this preview is unmanaged: {reason}")),
                }
                icc_profile = Some(std::sync::Arc::new(payload.to_vec()));
            }
        }
        if payload.len() % 2 != 0 {
            rr.bytes(1)?;
        }
    }
    control.preview(&merged, icc_profile.as_deref().map(Vec::as_slice))?;
    let high = depth == 16;
    if high && !has_merged_composite {
        return Err("This 16-bit PSD has no saved compatibility image. Re-save a copy in Photoshop with Maximize PSD Compatibility enabled to preview it in PeerBrush".into());
    }
    // Supplementary sources cannot override an unsupported standard resource
    // (for example a profile added without changing layer/composite hashes).
    if let Some(bytes) = embedded.filter(|_| warnings.is_empty()) {
        control.progress("Restoring editable sources", 0, 1)?;
        let mut json = vec![];
        let limit = if high {
            512 * 1024 * 1024
        } else {
            128 * 1024 * 1024
        };
        let mut zip = ZlibDecoder::new(&bytes[..]).take(limit + 1);
        let mut chunk = vec![0; 1024 * 1024];
        let result = (|| -> Result<(), String> {
            loop {
                control.check()?;
                let n = zip.read(&mut chunk).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                json.extend_from_slice(&chunk[..n]);
            }
            Ok(())
        })();
        control.check()?;
        if result.is_ok()
            && json.len()
                <= if high {
                    512 * 1024 * 1024
                } else {
                    128 * 1024 * 1024
                }
        {
            if let Ok(mut e) = serde_json::from_slice::<Embedded>(&json) {
                if (1..=15).contains(&e.format)
                    && e.document.bit_depth == depth
                    && (!high || e.format >= 5)
                    && e.standard_hash == hash(layer_section) ^ composite_hash
                    && validate(&e.document).is_ok()
                {
                    control.progress("Ready", 1, 1)?;
                    e.document.srgb_tagged |= srgb_tagged;
                    return Ok(e.document);
                }
            }
        }
        warnings.push(
            "PeerBrush definitions were changed or removed externally; using saved PSD pixels"
                .into(),
        );
    }
    let mut records = vec![];
    if !layer_section.is_empty() {
        let info = layer_info(layer_section, depth)?;
        if info.len() >= 2 {
            let mut lr = Cursor::new(info);
            let signed_count = lr.i16()?;
            let count = signed_count.unsigned_abs() as usize;
            if count > 200 {
                return Err("Too many PSD layers for this version".into());
            }
            for record_index in 0..count {
                control.progress("Reading layer definitions", record_index, count)?;
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
                    || lw > 8192
                    || lh > 8192
                    || lw as i64 * lh as i64 > 32 * 1024 * 1024
                {
                    return Err("PSD layer bounds exceed limits".into());
                }
                let nc = lr.u16()?;
                if nc > 8 {
                    return Err("Unsupported PSD layer channels".into());
                }
                let mut channels = vec![];
                for _ in 0..nc {
                    let channel = lr.i16()?;
                    channels.push((channel, lr.u32()? as usize));
                    if !(-2..=2).contains(&channel) {
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
                if flags & 1 != 0 || flags & !0x1f != 0 {
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
                if !maskdata.is_empty() && (maskdata.len() != 20 || maskdata[18..] != [0; 2]) {
                    warnings
                        .push("Advanced or invalid Photoshop mask data is not supported".into());
                }
                if maskdata.len() >= 18 {
                    let mut m = Cursor::new(maskdata);
                    mask_bounds = Some([m.i32()?, m.i32()?, m.i32()?, m.i32()?]);
                    if mask_bounds
                        .unwrap()
                        .iter()
                        .any(|v| v.unsigned_abs() > 100000)
                    {
                        return Err("PSD mask coordinates exceed initial limits".into());
                    }
                    mask_default = m.u8()?;
                    let mask_flags = m.u8()?;
                    mask_enabled = mask_flags & 2 == 0;
                    if mask_flags & !2 != 0 {
                        warnings.push("Advanced PSD mask flags are not supported yet".into());
                    }
                }
                if !default_blending_ranges(ex.block()?) {
                    warnings.push(
                        "Photoshop Blend If ranges are not supported; using the saved composite"
                            .into(),
                    );
                }
                let np = ex.u8()? as usize;
                let mut name = String::from_utf8_lossy(ex.bytes(np)?).to_string();
                let padded = (np + 1 + 3) & !3;
                ex.bytes(padded - np - 1)?;
                let mut section = 0;
                let mut psd_metadata = vec![];
                let mut seen_tags = std::collections::HashSet::new();
                while ex.pos + 12 <= extra.len() {
                    let signature = ex.bytes(4)?;
                    if signature != b"8BIM" && signature != b"8B64" {
                        warnings.push("Unrecognized Photoshop layer tag signature".into());
                        break;
                    }
                    let key = ex.bytes(4)?;
                    let payload = ex.block()?;
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
                        b"luni" => {
                            let mut n = Cursor::new(payload);
                            let count = n.u32()? as usize;
                            if count > 10000 {
                                return Err("PSD name is too long".into());
                            }
                            let mut chars = vec![];
                            for _ in 0..count {
                                chars.push(n.u16()?);
                            }
                            name = String::from_utf16_lossy(&chars);
                        }
                        b"lsct" | b"lsdk" => {
                            if seen_tags.contains(b"lsct".as_slice())
                                && seen_tags.contains(b"lsdk".as_slice())
                            {
                                warnings.push("Conflicting Photoshop section divider tags".into());
                            }
                            let mut divider = Cursor::new(payload);
                            section = divider.u32()?;
                            if ![4, 12, 16].contains(&payload.len()) || section > 3 {
                                warnings.push(
                                    "Unsupported Photoshop section divider (lsct/lsdk)".into(),
                                );
                            }
                            if payload.len() >= 12 {
                                if divider.bytes(4)? != b"8BIM" {
                                    warnings.push("Invalid Photoshop group blend signature".into());
                                }
                                blend = read_blend(divider.bytes(4)?, &mut warnings);
                            }
                            if payload.len() >= 16 && divider.u32()? != 0 {
                                warnings.push(
                                    "Photoshop animation scene groups are not supported".into(),
                                );
                            }
                        }
                        b"fxrp" | b"lclr" | b"lnsr" | b"shmd" => {
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
                        b"lyid" | b"lspf" | b"clbl" | b"infx" | b"knko" | b"iOpa" | b"sn2P" => {
                            if signature != b"8BIM" {
                                warnings.push("Unsupported Photoshop layer flag signature".into());
                            } else if let Err(reason) = standard_flag(key, payload) {
                                warnings.push(format!(
                                    "Unsupported Photoshop setting {}: {reason}; using the saved composite",
                                    String::from_utf8_lossy(key)
                                ));
                            }
                        }
                        _ => warnings.push(format!(
                            "Unsupported Photoshop feature: {}",
                            String::from_utf8_lossy(key)
                        )),
                    }
                }
                if ex.pos != extra.len() && extra[ex.pos..].iter().any(|v| *v != 0) {
                    warnings.push(
                        "Unrecognized Photoshop layer data; using the saved composite".into(),
                    );
                }
                if section == 0 && flags & 0x18 == 0x18 {
                    warnings.push("Photoshop layer pixels do not represent its appearance".into());
                }
                if section != 0 && clipping != 0 {
                    warnings.push("Photoshop clipping involving a folder is not supported".into());
                }
                let mut layer = Layer::new(
                    &name,
                    if section == 1 || section == 2 {
                        "group"
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
                layer.opacity = opacity;
                layer.blend = blend.into();
                layer.psd_metadata = psd_metadata;
                records.push(Record {
                    layer,
                    channels,
                    bounds,
                    mask_bounds,
                    mask_default,
                    mask_enabled,
                    section,
                    clipped: clipping == 1,
                });
            }
            if warnings.is_empty() {
                decoded_storage(&records, w, h, depth)?;
                let record_count = records.len();
                for (record_index, rec) in records.iter_mut().enumerate() {
                    control.progress("Decoding native layer pixels", record_index, record_count)?;
                    let lw = rec.layer.pixels.width;
                    let lh = rec.layer.pixels.height;
                    let mut pixels = Raster::new_depth(lw, lh, depth);
                    let mut initialized = false;
                    for &(ch, size) in &rec.channels {
                        control.check()?;
                        let bytes = lr.bytes(size)?;
                        if ch == -2 {
                            if let Some(b) = rec.mask_bounds {
                                let mw = b[3].checked_sub(b[1]).ok_or("Invalid mask")?;
                                let mh = b[2].checked_sub(b[0]).ok_or("Invalid mask")?;
                                if mw < 0 || mh < 0 {
                                    return Err("Invalid mask bounds".into());
                                }
                                let values = if high {
                                    decode_channel_16(bytes, mw as u32, mh as u32)?
                                } else {
                                    decode_channel(bytes, mw as u32, mh as u32)?
                                        .into_iter()
                                        .map(|v| v as u16 * 257)
                                        .collect()
                                };
                                let (pw, ph) = if rec.layer.kind == "group" {
                                    (w, h)
                                } else {
                                    (lw, lh)
                                };
                                let mut pixels = Raster::new_depth(pw, ph, depth);
                                for yy in 0..mh {
                                    if yy % 16 == 0 {
                                        control.check()?;
                                    }
                                    for xx in 0..mw {
                                        let v = values[(yy * mw + xx) as usize];
                                        pixels.set16(
                                            xx + b[1] - rec.bounds[1],
                                            yy + b[0] - rec.bounds[0],
                                            [v, v, v, 65535],
                                        );
                                    }
                                }
                                rec.layer.mask = Some(Mask {
                                    cache_key: crate::engine::id(),
                                    enabled: rec.mask_enabled,
                                    steps: vec![
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
                                    ],
                                });
                            }
                        } else if (-1..=2).contains(&ch) {
                            let c = if ch == -1 { 3 } else { ch as usize };
                            stream_layer_channel(bytes, &mut pixels, c, !initialized, control)?;
                            initialized = true;
                        } else {
                            warnings.push(format!("Unsupported Photoshop layer channel: {ch}"));
                        }
                    }
                    if lw > 0 && lh > 0 {
                        rec.layer.pixels = pixels;
                    } else if rec.layer.kind == "group" {
                        rec.layer.pixels = Raster::new_depth(w, h, depth);
                    }
                }
            }
        }
    }
    let mut doc = Document::new(w, h)?;
    doc.bit_depth = depth;
    doc.icc_profile = icc_profile;
    doc.srgb_tagged = srgb_tagged;
    doc.layers.clear();
    let mut parents = Vec::<String>::new();
    let mut clipped = std::collections::HashSet::new();
    for mut rec in records.into_iter().rev() {
        if rec.section == 3 {
            if parents.pop().is_none() {
                warnings.push("Unbalanced Photoshop folder boundaries".into());
            }
            continue;
        }
        if rec.clipped {
            clipped.insert(rec.layer.id.clone());
        }
        rec.layer.parent = parents.last().cloned();
        if rec.layer.kind == "group" {
            parents.push(rec.layer.id.clone());
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
                if layer.kind == "paint" && kind == "paint" {
                    layer.clip_to = Some(String::clone(base));
                } else {
                    warnings.push(format!(
                        "Photoshop clipping involving a folder is not supported: {}",
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
    warnings.sort();
    warnings.dedup();
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
    control.progress("Ready", 1, 1)?;
    Ok(doc)
}
///16-bit Photoshop layer records live in Lr16 instead of the primary8-bit block.
fn layer_info(section: &[u8], depth: u16) -> Result<&[u8], String> {
    let mut r = Cursor::new(section);
    let primary = r.block()?;
    if depth == 8 || r.pos == section.len() {
        return Ok(primary);
    }
    r.block()?;
    let mut info = primary;
    while r.pos < section.len() {
        if section.len() - r.pos < 12 {
            if section[r.pos..].iter().all(|v| *v == 0) {
                break;
            }
            return Err("Truncated global layer information".into());
        }
        let signature = r.bytes(4)?;
        if !matches!(signature, b"8BIM" | b"8B64") {
            return Err("Invalid global layer signature".into());
        }
        let key = r.bytes(4)?;
        let payload = r.block()?;
        if key == b"Lr16" {
            info = payload;
        }
        r.bytes((4 - payload.len() % 4) % 4)?;
    }
    Ok(info)
}

/// Photoshop can store deeper layer-info and merged transparency as global tagged blocks.
/// Read only their bounded headers; unsupported source objects are never deserialized for a preview.
fn merged_transparency(section: &[u8], depth: u16) -> Result<bool, String> {
    if section.is_empty() {
        return Ok(false);
    }
    let mut r = Cursor::new(section);
    let info = r.block()?;
    let mut alpha = info.len() >= 2 && i16::from_be_bytes(info[..2].try_into().unwrap()) < 0;
    if r.pos == section.len() {
        return Ok(alpha);
    }
    r.block()?; // Global layer mask.
    while r.pos < section.len() {
        if section.len() - r.pos < 12 {
            if section[r.pos..].iter().all(|b| *b == 0) {
                break;
            }
            return Err("Truncated global PSD layer information".into());
        }
        let signature = r.bytes(4)?;
        if signature != b"8BIM" && signature != b"8B64" {
            return Err("Invalid global PSD layer information signature".into());
        }
        let key = r.bytes(4)?;
        let payload = r.block()?;
        if (depth == 8 && key == b"Mtrn") || (depth == 16 && key == b"Mt16") {
            alpha = true;
        }
        if ((depth == 16 && key == b"Lr16") || (depth == 8 && key == b"Layr")) && payload.len() >= 2
        {
            alpha |= i16::from_be_bytes(payload[..2].try_into().unwrap()) < 0;
        }
        // Global tagged blocks are aligned to four bytes; per-layer tags use two.
        let padding = (4 - payload.len() % 4) % 4;
        r.bytes(padding)?;
    }
    Ok(alpha)
}

/// Bounded display decode: no 16-bit sources are replaced or marked editable.
fn decode_planes_16(data: &[u8], w: u32, h: u32, count: usize) -> Result<Vec<u16>, String> {
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
            let lengths = (0..rows)
                .map(|_| r.u16().map(|n| n as usize))
                .collect::<Result<Vec<_>, _>>()?;
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

fn decode_channel_16(data: &[u8], w: u32, h: u32) -> Result<Vec<u16>, String> {
    decode_planes_16(data, w, h, 1)
}
pub fn validate(doc: &Document) -> Result<(), String> {
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
    let mut ids = std::collections::HashSet::new();
    let mut total = 0usize;
    for l in &doc.layers {
        validate_metadata(&l.psd_metadata)?;
        if let Some(source) = &l.source {
            if l.kind != "paint" {
                return Err("Editable text/vector sources require paint raster projections".into());
            }
            source.validate()?;
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
        total += l.pixels.bytes();
        total += l
            .pixels
            .retained
            .as_ref()
            .map(|s| s.pixels.bytes())
            .unwrap_or(0);
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
                total += s.pixels.bytes();
                total += s
                    .pixels
                    .retained
                    .as_ref()
                    .map(|s| s.pixels.bytes())
                    .unwrap_or(0);
                let valid = match s.kind.as_str() {
                    "blur" => (0.0..=64.0).contains(&s.value),
                    "levels" => (0.1..=5.0).contains(&s.value),
                    "fill" | "paint" | "invert" => (0.0..=255.0).contains(&s.value),
                    "curves" | "adjust" | "gaussian" => {
                        crate::effects::validate(
                            if s.kind == "gaussian" {
                                "blur"
                            } else {
                                &s.kind
                            },
                            &s.settings,
                        )?;
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
    fn record() -> Record {
        Record {
            layer: Layer::new("Thin source", "paint", 8192, 1),
            channels: vec![(0, 2)],
            bounds: [0, 0, 1, 8192],
            mask_bounds: None,
            mask_default: 255,
            mask_enabled: true,
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
