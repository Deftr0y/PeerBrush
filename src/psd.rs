//! Bounded PSD v1 / PSB v2 codecs. Unsupported Photoshop features are never silently rewritten.
pub mod patterns;
mod styles;
use crate::{
    engine::{id, Document, Layer, Mask, MaskStep},
    raster::{check_size, Raster},
};
use flate2::{read::ZlibDecoder, write::ZlibEncoder, Compression};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::io::{Read, Seek, SeekFrom, Write};

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

pub(crate) struct StandardPlan {
    pub composite: bool,
    pub hidden_sources: bool,
}
pub(crate) fn standard_plan(doc: &Document) -> StandardPlan {
    StandardPlan {
        composite: !doc.filters.is_empty()
            || doc.layers.iter().any(|layer| layer.kind == "adjustment")
            || !standard_clipping(doc),
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
                out.push((layer, 1));
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
pub(crate) fn standard_bounds(doc: &Document, layer: &Layer, section: u32) -> [i32; 4] {
    if section != 0 {
        [0; 4]
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

fn has_letter_spacing(doc: &Document) -> bool {
    doc.layers.iter().any(|layer| {
        matches!(layer.source.as_ref().map(|source| &source.content),
            Some(crate::source::Content::Text { letter_spacing, .. }) if *letter_spacing != 0.)
    })
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

fn default_blending_ranges(bytes: &[u8], channels: u16) -> bool {
    bytes.is_empty()
        // Photoshop may include transparency/mask channel pairs in addition to
        // RGB and the composite gray pair. Every pair must remain a no-op.
        || (bytes.len() <= (usize::from(channels) + 1) * 8
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
        b"lspf" if bytes == [0; 4] || bytes == [0, 0, 0, 7] => Ok(()),
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

/// The preferred global-mask kind delegates mask values to individual layers.
/// Retain its overlay bytes in standard PSD/PSB data without changing pixels.
/// Other kinds, color spaces and payload extensions remain protected.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalMaskSettings {
    pub data: Vec<u8>,
}
impl GlobalMaskSettings {
    fn validate(&self) -> Result<(), String> {
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
        _ => Err(invalid()),
    }
}

pub fn validate_metadata(entries: &[LayerMetadata]) -> Result<(), String> {
    if entries.len() > 5 {
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
pub(crate) fn encode_to<S: Read + Write + Seek>(
    doc: &Document,
    output: &mut dyn Write,
    spool: &mut S,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    encode_layout_progress_to(doc, Layout::Psd, output, spool, check, progress)
}
pub(crate) fn encode_psb_to<S: Read + Write + Seek>(
    doc: &Document,
    output: &mut dyn Write,
    spool: &mut S,
    check: &dyn Fn() -> Result<(), String>,
    progress: &dyn Fn(&str, usize, usize) -> Result<(), String>,
) -> Result<(), String> {
    encode_layout_progress_to(doc, Layout::Psb, output, spool, check, progress)
}
fn check_output_size(doc: &Document, plan: &StandardPlan) -> Result<(), String> {
    // Validation bounds every frame and record count; u64 also covers their aggregate.
    let bytes = u64::from(doc.bit_depth / 8);
    let canvas = u64::from(doc.width) * u64::from(doc.height);
    let mut minimum = 40 + 4 * canvas * bytes;
    let mut records = usize::from(plan.composite);
    if plan.composite {
        minimum += 4 * canvas * bytes + 8;
    }
    if !plan.composite || plan.hidden_sources {
        for (layer, section) in standard_order(doc) {
            records += 1;
            let [x, y, right, bottom] = standard_bounds(doc, layer, section);
            minimum +=
                4 * (u64::from((right - x) as u32) * u64::from((bottom - y) as u32) * bytes + 2);
            if layer.mask.is_some() && section != 3 {
                let pixels = if crate::effects::canvas_frame(layer) {
                    canvas
                } else {
                    u64::from(layer.pixels.width) * u64::from(layer.pixels.height)
                };
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
    check_output_size(doc, &plan)?;
    let high = doc.bit_depth == 16;
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
    let composite16 = if high {
        Some(crate::depth16::render(doc)?)
    } else {
        None
    };
    check()?;
    // Ordinary raster clipping remains standard, editable PSD layer data.
    // Adjustments and unsupported clipping units still need a standard bake.
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
    let mut channel_bytes = 0u64;
    let mut layer_rows = 0usize;
    for (l, section) in &ordered {
        let [x, y, right, bottom] = standard_bounds(doc, l, *section);
        let (w, h) = ((right - x) as u32, (bottom - y) as u32);
        i32b(&mut records, y);
        i32b(&mut records, x);
        i32b(&mut records, y + h as i32);
        i32b(&mut records, x + w as i32);
        let mask = l.mask.as_ref();
        // Folder masks cover the standard canvas; private sources retain off-canvas frames.
        let (mask_w, mask_h, mask_x, mask_y) = if crate::effects::canvas_frame(l) {
            (doc.width, doc.height, 0, 0)
        } else {
            (l.pixels.width, l.pixels.height, l.x, l.y)
        };
        let has_mask = mask.is_some() && *section != 3;
        u16b(&mut records, if has_mask { 5 } else { 4 });
        for ch in [-1i16, 0, 1, 2]
            .into_iter()
            .chain(if has_mask { Some(-2) } else { None })
        {
            let (cw, ch_height) = if ch == -2 { (mask_w, mask_h) } else { (w, h) };
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
        records.push(if l.visible { 0 } else { 2 } | u8::from(l.locked && *section != 3));
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
        if l.locked && *section != 3 {
            tag(&mut extra, b"lspf", &7u32.to_be_bytes());
        }
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
        let [x, y, right, bottom] = standard_bounds(doc, l, *section);
        let (w, h) = ((right - x) as u32, (bottom - y) as u32);
        let mask = l.mask.as_ref();
        let (mask_w, mask_h, mask_x, mask_y) = if crate::effects::canvas_frame(l) {
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
        for ch in [-1i16, 0, 1, 2]
            .into_iter()
            .chain(if has_mask { Some(-2) } else { None })
        {
            standard
                .write_all(&0u16.to_be_bytes())
                .map_err(|e| e.to_string())?;
            let (cw, ch_height) = if ch == -2 { (mask_w, mask_h) } else { (w, h) };
            let raw_native = ch != -2
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
                            row[xx as usize * 2..xx as usize * 2 + 2]
                                .copy_from_slice(&value.to_be_bytes());
                        } else {
                            row[xx as usize] = if ch == -2 {
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
        suffix.extend(b"8BIMMt16");
        layout.write_length(&mut suffix, 0)?;
    } else {
        block(
            &mut suffix,
            doc.psd_global_mask
                .as_ref()
                .map_or(&[], |mask| mask.data.as_slice()),
        );
    }
    standard.write_all(&suffix).map_err(|e| e.to_string())?;
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
    if let Some(image) = &composite16 {
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
    let embedded = Embedded {
        // Older readers cannot interpret soft selections or smooth curve sources.
        format: if has_shaped_text(doc) {
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
        document: doc.clone(),
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
        let mut z = BoundedWriter {
            inner: ZlibEncoder::new(source, Compression::fast()),
            written: 0,
            limit: if high {
                512 * 1024 * 1024
            } else {
                128 * 1024 * 1024
            },
            check,
            error: "Editable PSD source data exceeds its bounded serialization budget",
        };
        serde_json::to_writer(&mut z, &embedded).map_err(|e| e.to_string())?;
        let mut source = z.inner.finish().map_err(|e| e.to_string())?;
        source.flush().map_err(|e| e.to_string())?;
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
    control.progress("Resolving saved composite transparency", 0, 1)?;
    // PSD compatibility colors are composited over white. Restore straight RGB in tiles.
    let max = if depth == 16 { 65535i64 } else { 255 };
    let restore = |pixel: &mut [u16]| {
        let a = pixel[3] as i64;
        let valid = pixel[..3].iter().all(|v| i64::from(*v) >= max - a);
        for v in &mut pixel[..3] {
            *v = if a == 0 {
                0
            } else {
                (((*v as i64 + a - max) * max + a / 2) / a).clamp(0, max) as u16
            };
        }
        valid
    };
    let mut white_valid = true;
    for (&(tx, ty), tile) in &mut raster.samples16 {
        control.check()?;
        for (index, pixel) in std::sync::Arc::make_mut(tile)
            .chunks_exact_mut(4)
            .enumerate()
        {
            let valid = restore(pixel);
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
            let valid = restore(&mut p);
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
    let high = pixels.depth == 16;
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
    let mut row = vec![0; row_bytes];
    let left = (-i64::from(offset[0])).clamp(0, i64::from(w)) as u32;
    let right = (i64::from(pixels.width) - i64::from(offset[0])).clamp(0, i64::from(w)) as u32;
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
        let yy = i64::from(y) + i64::from(offset[1]);
        if (0..i64::from(pixels.height)).contains(&yy) {
            for x in left..right {
                let v = if high {
                    u16::from_be_bytes([row[x as usize * 2], row[x as usize * 2 + 1]])
                } else {
                    u16::from(row[x as usize]) * 257
                };
                pixels.set16(x as i32 + offset[0], yy as i32, [v, v, v, 65535]);
            }
        }
    }
    if let Some(zip) = &mut zip {
        // Reading through EOF also requires zlib's stream end/checksum. A short
        // stream, extra decoded byte or corrupt trailer cannot authorize a mask.
        let mut extra = [0; 1];
        if zip.read(&mut extra).map_err(|e| e.to_string())? != 0 {
            return Err("Invalid ZIP mask channel size".into());
        }
    }
    control.check()
}

/// Stream the composite into native tiles; never buffer the complete encoded file.
pub fn decode_reader<R: Read + std::io::Seek>(
    reader: &mut R,
    control: &crate::loading::Control,
) -> Result<Document, String> {
    decode_sections(reader, control, |reader, length, large, control, stage| {
        read_section(reader, length, large, control, stage).map(Cow::Owned)
    })
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
) -> Result<Document, String> {
    let mut rr = Cursor::new(resources);
    let mut embedded = None;
    let mut warnings = global_warnings(layer_section, depth, layout)?;
    let (_, global_mask, _) = global_blocks(layer_section, layout)?;
    let global_mask_settings = (!global_mask.is_empty()
        && validate_global_mask_settings(global_mask).is_ok())
    .then(|| GlobalMaskSettings {
        data: global_mask.to_vec(),
    });
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
    #[cfg(test)]
    observe_open_phase("preview_ready", 0);
    let high = depth == 16;
    if high && !has_merged_composite {
        return Err("This 16-bit PSD has no saved compatibility image. Re-save a copy in Photoshop with Maximize PSD Compatibility enabled to preview it in PeerBrush".into());
    }
    // Supplementary sources cannot override an unsupported standard resource
    // (for example a profile added without changing layer/composite hashes).
    if let Some(bytes) = embedded.filter(|_| warnings.is_empty()) {
        control.progress("Restoring editable sources", 0, 1)?;
        let limit = if high {
            512 * 1024 * 1024
        } else {
            128 * 1024 * 1024
        };
        #[cfg(test)]
        observe_open_phase("json_start", bytes.len());
        let result = embedded_source(ZlibDecoder::new(bytes), limit, control);
        #[cfg(test)]
        observe_open_phase("source_decoded", 0);
        control.check()?;
        if let Ok(mut e) = result {
            if (1..=34).contains(&e.format)
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
                && e.standard_hash == hash(layer_section) ^ composite_hash
                && e.document.psd_global_mask == global_mask_settings
                && validate(&e.document).is_ok()
                && metadata_sources_match(layer_section, layout, &e.document, control) == Ok(true)
            {
                #[cfg(test)]
                observe_open_phase("source_accepted", 0);
                control.progress("Ready", 1, 1)?;
                e.document.srgb_tagged |= srgb_tagged;
                return Ok(e.document);
            }
        }
        warnings.push(
            "PeerBrush definitions were changed or removed externally; using saved PSD pixels"
                .into(),
        );
    }
    let mut source_patterns: Vec<patterns::Source> = vec![];
    for (key, payload) in global_blocks(layer_section, layout)?.2 {
        if matches!(key, b"Patt" | b"Pat2" | b"Pat3") {
            let tag = std::str::from_utf8(key).unwrap();
            match patterns::decode(payload, tag, control) {
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
                    channels.push((channel, lr.length(layout.large())?));
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
                if !default_blending_ranges(ex.block()?, nc) {
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
                let mut locked = false;
                let mut psd_metadata = vec![];
                let mut imported_styles = vec![];
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
                        b"fxrp" | b"lclr" | b"lnsr" | b"shmd" | b"lyvr" => {
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
                            } else if key == b"lspf" {
                                locked = payload == [0, 0, 0, 7];
                            }
                        }
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
                                    let imported = if signature == b"8BIM" && section == 0 && blend == "normal" && imported_styles.is_empty() {
                                        styles::native_styles(payload, &source_patterns, depth).and_then(|(effects, pattern)| {
                                            if effects.iter().any(|e| matches!(e.kind.as_str(), "drop_shadow" | "inner_shadow"))
                                                && (!maskdata.is_empty() || clipping != 0) {
                                                Err("Hard Photoshop shadows with masks or clipping remain protected".into())
                                            } else if effects.iter().any(|e| e.kind == "stroke")
                                                && (!maskdata.is_empty() || clipping != 0) {
                                                Err("Photoshop strokes with masks or clipping remain protected".into())
                                            } else { Ok((effects, pattern)) }
                                        })
                                    } else { Err("Photoshop styles with folders, layer blending or duplicate blocks remain protected".into()) };
                                    match imported {
                                        Ok((effects, pattern)) => {
                                            imported_styles = effects;
                                            if let Some(index) = pattern { used_patterns.insert(index); }
                                        },
                                        Err(reason) => {
                                            warnings.push(format!("Photoshop layer styles ({tag_name}) remain protected: {reason}; using the saved composite"));
                                            warnings.extend(details);
                                        }
                                    }
                                }
                                Err(reason) => warnings.push(format!("Cannot inspect Photoshop layer styles ({tag_name}): {reason}; using the protected saved composite")),
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
                if flags & 1 != 0 && !locked {
                    warnings
                        .push("Partial Photoshop transparency protection is not supported".into());
                }
                if section == 0 && flags & 0x18 == 0x18 {
                    warnings.push("Photoshop layer pixels do not represent its appearance".into());
                }
                if section != 0 && clipping != 0 {
                    warnings.push("Photoshop clipping involving a folder is not supported".into());
                }
                if section != 0 && !imported_styles.is_empty() {
                    warnings.push("Photoshop folder styles remain protected".into());
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
                layer.locked = locked;
                layer.opacity = opacity;
                layer.blend = blend.into();
                layer.psd_metadata = psd_metadata;
                layer.effects = imported_styles;
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
                    has_mask: mask_bounds.is_some(),
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
                    mask_default,
                    mask_enabled,
                    section,
                    clipped: clipping == 1,
                });
            }
            // Admit pattern resource blocks only when every asset is retained in a supported
            // native style. Unused assets, duplicate tags and unrelated sources stay protected.
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
                                let (pw, ph) = if rec.layer.kind == "group" {
                                    (w, h)
                                } else {
                                    (lw, lh)
                                };
                                let mut pixels = Raster::new_depth(pw, ph, depth);
                                stream_mask_channel(
                                    bytes,
                                    mw as u32,
                                    mh as u32,
                                    &mut pixels,
                                    [b[1] - rec.bounds[1], b[0] - rec.bounds[0]],
                                    layout,
                                    control,
                                )?;
                                rec.layer.mask = Some(Mask {
                                    linked: true,
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
                            stream_layer_channel(
                                bytes,
                                &mut pixels,
                                c,
                                !initialized,
                                layout,
                                control,
                            )?;
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
                    if rec.layer.effects.iter().any(|e| e.kind == "stroke") {
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
                            warnings.push(
                                "Photoshop strokes with partial source alpha remain protected"
                                    .into(),
                            );
                        }
                    }
                }
            }
        }
    }
    let mut doc = Document::new(w, h)?;
    doc.source_patterns = source_patterns;
    doc.bit_depth = depth;
    doc.icc_profile = icc_profile;
    doc.srgb_tagged = srgb_tagged;
    doc.psd_global_mask = global_mask_settings;
    doc.layers.clear();
    let mut parents = Vec::<String>::new();
    let mut source_indices = std::collections::HashMap::new();
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
        rec.source.parent_index = rec
            .layer
            .parent
            .as_ref()
            .and_then(|parent| source_indices.get(parent).copied());
        source_indices.insert(rec.layer.id.clone(), rec.source.source_index);
        doc.source_structure.push(rec.source);
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
    // New style import never authorizes editing on approximate visual equivalence.
    // Compare exact standard white-matte RGB and alpha samples. Straight-alpha
    // restoration can shift RGB by a word; it must not reject matching encoded
    // samples or introduce an approximate tolerance. Out-of-range matte samples
    // cannot be recovered after clamping and therefore never authorize editing.
    if warnings.is_empty() && doc.layers.iter().any(|l| !l.effects.is_empty()) {
        control.check()?;
        let matches = if merged_white_valid {
            imported_style_composite_matches(&doc, &merged).and_then(|matches| {
                if matches {
                    return Ok(true);
                }
                control.check()?;
                // Preserve already accepted placement. The bounded alternate
                // uses the same integer-center renderer as native gradients,
                // and is accepted only by the complete exact-composite gate.
                // Try one document-wide candidate, never combinations of
                // per-effect guesses or a pixel tolerance.
                let mut candidate = doc.clone();
                let mut changed = false;
                for effect in candidate.layers.iter_mut().flat_map(|l| &mut l.effects) {
                    if effect.kind == "gradient_overlay"
                        && effect.enabled
                        && effect.settings["pixel_grid"] != true
                    {
                        std::sync::Arc::make_mut(&mut effect.settings)["pixel_grid"] =
                            serde_json::json!(true);
                        changed = true;
                    }
                }
                if changed {
                    // The first render has populated derived effect caches.
                    // This alternate source must not reuse the clone's old
                    // keys: both native8/native16 and ancestor projections
                    // need fresh derived identities without a history edit.
                    crate::effects::invalidate(
                        &mut candidate,
                        &[serde_json::json!({"op":"document.settings"})],
                    );
                }
                if changed && imported_style_composite_matches(&candidate, &merged)? {
                    doc = candidate;
                    Ok(true)
                } else {
                    Ok(false)
                }
            })
        } else {
            Ok(false)
        };
        match matches {
            Ok(true) => {},
            Ok(false) => warnings.push("Photoshop style rendering differs from the native saved composite; original styles remain protected".into()),
            Err(reason) => warnings.push(format!("Cannot reproduce Photoshop style composite: {reason}; original styles remain protected")),
        }
        control.check()?;
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
    if !doc.read_only {
        doc.source_structure.clear();
        doc.source_patterns.clear();
    }
    control.progress("Ready", 1, 1)?;
    Ok(doc)
}
fn imported_style_composite_matches(doc: &Document, saved: &Raster) -> Result<bool, String> {
    if saved.is16() {
        crate::depth16::render(doc).map(|image| same_standard_composite(&image.words, saved))
    } else {
        doc.preview_native8(None, doc.width.max(doc.height), None, false)
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
                && ((depth == 8 && key == b"Mtrn") || (depth == 16 && key == b"Mt16")));
        if !supported {
            warnings.push(format!(
                "Unsupported global Photoshop feature: {}",
                String::from_utf8_lossy(key)
            ));
        }
        if depth == 8 && key == b"Layr" && !primary.is_empty() {
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
    control: &crate::loading::Control,
) -> Result<bool, String> {
    let plan = standard_plan(doc);
    let mut expected = if !plan.composite || plan.hidden_sources {
        standard_order(doc)
            .into_iter()
            .map(|(layer, section)| {
                if section == 3 {
                    vec![]
                } else {
                    layer.psd_metadata.clone()
                }
            })
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    if plan.composite {
        expected.insert(0, vec![]);
    }
    if expected.is_empty() {
        expected.push(vec![]);
    }
    expected.reverse();
    let info = layer_info(section, doc.bit_depth, layout)?;
    let mut records = Cursor::new(info);
    let count = usize::from(records.i16()?.unsigned_abs());
    if count > 200 || count != expected.len() {
        return Ok(false);
    }
    for expected in expected {
        control.check()?;
        records.bytes(16)?;
        let channels = records.u16()?;
        if channels > 8 {
            return Ok(false);
        }
        for _ in 0..channels {
            records.i16()?;
            records.length(layout.large())?;
        }
        records.bytes(12)?;
        let extra = records.block()?;
        let mut tags = Cursor::new(extra);
        tags.block()?;
        tags.block()?;
        let name = usize::from(tags.u8()?);
        tags.bytes(name)?;
        tags.bytes((name + 1).div_ceil(4) * 4 - name - 1)?;
        let mut actual = vec![];
        while tags.pos + 12 <= extra.len() {
            let signature = tags.bytes(4)?;
            let key = tags.bytes(4)?;
            let payload = layout.tag_block(&mut tags, key)?;
            tags.bytes(payload.len() % 2)?;
            if matches!(key, b"fxrp" | b"lclr" | b"lnsr" | b"shmd" | b"lyvr") {
                if signature != b"8BIM" || payload.len() > 128 {
                    return Ok(false);
                }
                actual.push(LayerMetadata {
                    key: std::str::from_utf8(key).unwrap().into(),
                    data: payload.to_vec(),
                });
            }
        }
        if extra[tags.pos..].iter().any(|v| *v != 0)
            || validate_metadata(&actual).is_err()
            || actual.len() != expected.len()
            || actual.iter().any(|m| !expected.contains(m))
        {
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
    let mut ids = std::collections::HashSet::new();
    let mut total = 0usize;
    for l in &doc.layers {
        validate_metadata(&l.psd_metadata)?;
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

    #[test]
    fn source_resource_framing_keeps_native_sources_history_and_standard_bytes() {
        for depth in [8, 16] {
            for tagged in [false, true] {
                let mut engine = crate::engine::Engine::new();
                engine.doc = streamed_fixture(depth);
                engine.doc.srgb_tagged = tagged;
                let layer = &mut engine.doc.layers[0];
                layer.pixels.retained =
                    Some(Box::new(crate::retained::Original::from(&layer.pixels)));
                let mask = &mut layer.mask.as_mut().unwrap().steps[0].pixels;
                mask.retained = Some(Box::new(crate::retained::Original::from(mask)));
                let layer_id = layer.id.clone();
                for opacity in [0.5, 1.0, 0.25] {
                    engine.edit(
                        "human",
                        &[serde_json::json!({"op":"layer.update","layer":layer_id,"opacity":opacity})],
                        Some(engine.doc.revision),
                        None,
                        "Resource fixture opacity",
                    ).unwrap();
                }
                engine.undo("human").unwrap();
                assert_eq!((engine.undo.len(), engine.redo.len()), (2, 1));
                assert_eq!(engine.doc.layers[0].opacity, 1.0);
                let history = |engine: &crate::engine::Engine| {
                    let entries = |items: &[crate::engine::History]| {
                        items.iter().map(|h| {
                        serde_json::json!({"before":h.before,"after":h.after,"actor":h.actor,
                            "label":h.label,"task":h.task,"scopes":h.scopes,"gesture":h.gesture,
                            "reverted":h.reverted})
                    }).collect::<Vec<_>>()
                    };
                    serde_json::json!({"undo":entries(&engine.undo),"redo":entries(&engine.redo),
                        "saved_revision":engine.saved_revision,"leases":engine.leases})
                };
                let before = serde_json::to_value(&engine.doc).unwrap();
                let history_before = history(&engine);
                for layout in [Layout::Psd, Layout::Psb] {
                    let bytes = encode_layout(&engine.doc, layout).unwrap();
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
                    assert_eq!(embedded.format, 29);
                    assert_eq!(serde_json::to_value(&embedded.document).unwrap(), before);
                    // Independent previous Vec-compressor route, same source and
                    // settings: append/replay must preserve actual PBR1 bytes.
                    let original = Embedded {
                        format: embedded.format,
                        standard_hash: embedded.standard_hash,
                        document: engine.doc.clone(),
                    };
                    assert_eq!(json, serde_json::to_vec(&original).unwrap());
                    let mut old = ZlibEncoder::new(Vec::new(), Compression::fast());
                    serde_json::to_writer(&mut old, &original).unwrap();
                    assert_eq!(compressed, old.finish().unwrap());
                    let section = layout.block(&mut file).unwrap();
                    let merged = &bytes[file.pos..];
                    let standard = expected_standard(&engine.doc, layout.large());
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
                    assert_eq!(png, engine.doc.export_png().unwrap());
                    assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
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
        record.extend(50u32.to_be_bytes());
        record.extend([0, 0, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        record.extend(doc.width.to_be_bytes());
        record.extend([255, 2, 0, 0]);
        record.extend([0, 0, 0, 0, 1, b'X', 0, 0]);
        record.extend(b"8BIMluni");
        record.extend([0, 0, 0, 6, 0, 0, 0, 1, 0, b'X']);
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
        let mut doc = Document::new_depth(4, 4, 16).unwrap();
        doc.layers[0].pixels = Raster::new_depth(8192, 3277, 16);
        validate(&doc).unwrap();
        check_output_size(&doc, &standard_plan(&doc)).unwrap();
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
        assert!(check_output_size(&doc, &standard_plan(&doc))
            .unwrap_err()
            .contains("Uncompressed Photoshop channels alone"));
        doc.layers[0].mask = None;
        doc.layers[0].pixels = Raster::new_depth(8192, 4096, 16);
        assert!(check_output_size(&doc, &standard_plan(&doc)).is_err());

        // An adjustment-only standard bake stores the tiny current canvas;
        // the large sparse editable sources remain in supplementary data.
        let mut adjustment = Layer::new("Adjustment", "adjustment", 4, 4);
        adjustment.pixels = Raster::new_depth(4, 4, 16);
        doc.layers.insert(0, adjustment);
        validate(&doc).unwrap();
        check_output_size(&doc, &standard_plan(&doc)).unwrap();
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
        assert!(check_output_size(&doc, &standard_plan(&doc)).is_err());

        let mut empty = Document::new_depth(8192, 4096, 8).unwrap();
        empty.layers.clear();
        assert!(
            check_output_size(&empty, &standard_plan(&empty)).is_err(),
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
        check_output_size(&doc, &standard_plan(&doc)).unwrap();
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

#[cfg(test)]
mod mask_rows_tests;

#[cfg(test)]
mod source_stream_tests;

#[cfg(test)]
pub(crate) use source_stream_tests::observe_phase as observe_open_phase;
