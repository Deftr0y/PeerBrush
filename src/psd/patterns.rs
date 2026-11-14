//! Bounded raw RGB pattern inspection; decoding assets never authorizes Photoshop editing.
//! Structure: https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/
use super::{Cursor, Layout};
use crate::{loading::Control, raster::Raster};
use std::{collections::HashSet, sync::Arc};

pub(super) const STANDARD_FORMAT: u32 = 96;
pub(super) const OFFSET_FORMAT: u32 = 97;
pub(super) const ROTATION_FORMAT: u32 = 98;
pub(super) const MAX_BLOCK: usize = 8 * 1024 * 1024;
const MAX_ASSETS: usize = 16;
const NAME: &str = "PeerBrush Pattern";

pub(crate) struct StandardFill<'a> {
    pub png: &'a str,
    placement: Placement,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Placement {
    pub phase: [i32; 2],
    pub angle: i32,
}

impl Placement {
    pub(super) fn from_settings(settings: &serde_json::Value) -> Option<Self> {
        let number = |key| settings.get(key).map_or(Some(0.), serde_json::Value::as_f64);
        Some(Self {
            phase: [integral_phase(number("phase_x")?)?, integral_phase(number("phase_y")?)?],
            angle: right_angle(number("bitmap_angle")?)?,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Geometry {
    Origin,
    Offset,
    Rotation,
}

impl Geometry {
    pub(super) fn for_source_format(format: u32) -> Self {
        match format {
            ROTATION_FORMAT.. => Self::Rotation,
            OFFSET_FORMAT.. => Self::Offset,
            _ => Self::Origin,
        }
    }

    pub(super) fn for_record_size(bytes: u64) -> Option<Self> {
        match usize::try_from(bytes).ok()? {
            DESCRIPTOR_BYTES => Some(Self::Origin),
            OFFSET_DESCRIPTOR_BYTES => Some(Self::Offset),
            ROTATION_DESCRIPTOR_BYTES | FULL_DESCRIPTOR_BYTES => Some(Self::Rotation),
            _ => None,
        }
    }
}

pub(super) fn integral_phase(value: f64) -> Option<i32> {
    ((-32768. ..=32768.).contains(&value) && value.fract() == 0.).then_some(value as i32)
}

pub(super) fn right_angle(value: f64) -> Option<i32> {
    ((-180. ..=180.).contains(&value) && value % 90. == 0.).then_some(value as i32)
}

pub(crate) fn standard(
    layer: crate::engine::view::LayerView<'_>, width: u32, height: u32, geometry: Geometry,
) -> Option<StandardFill<'_>> {
    if layer.kind() != "fill" || layer.color()[3] != 255 || layer.source().is_some()
        || layer.origin() != [0, 0] || layer.pixels().width != width || layer.pixels().height != height
        || layer.effect_count() != 1 || layer.blend() != "normal"
    { return None; }
    let effect = layer.effect(0)?;
    let v = effect.settings();
    let number = |key, default| v.get(key).map_or(Some(default), serde_json::Value::as_f64);
    if effect.kind() != "pattern_overlay" || !effect.enabled() || effect.weight() != 1.
        || v["pattern"] != "bitmap" || v["use_pattern_alpha"] != true || v["sampling"] != "nearest"
        || v.get("blend").is_some_and(|blend| blend != "normal") || number("opacity", 0.75) != Some(1.)
        || number("bitmap_scale", 1.) != Some(1.)
    { return None; }
    let placement = Placement::from_settings(v)?;
    if (geometry < Geometry::Offset && placement.phase != [0, 0])
        || (geometry < Geometry::Rotation && placement.angle != 0)
    { return None; }
    Some(StandardFill { png: v["bitmap_png"].as_str()?, placement })
}

type Digest = [u8; 32];
pub(super) type PatternId = [u8; 16];

#[derive(Clone, Copy)]
pub(crate) struct FillRecord {
    id: PatternId,
    placement: Placement,
}

impl FillRecord {
    pub(super) fn source_format(self) -> u32 {
        if self.placement.angle != 0 { ROTATION_FORMAT }
        else if self.placement.phase != [0, 0] { OFFSET_FORMAT }
        else { STANDARD_FORMAT }
    }
}

#[derive(Default)]
pub(crate) struct StandardPatterns {
    bytes: Vec<u8>,
    assets: [Digest; MAX_ASSETS],
    count: usize,
    _charge: Option<crate::large_document::Reservation>,
}

impl StandardPatterns {
    pub(super) fn prepare<'a>(
        sources: impl IntoIterator<Item = &'a str>,
        document_depth: u16,
        pool: &crate::large_document::ResidentBudget,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut result = Self::default();
        let mut decoded_bytes = 0u64;
        for source in sources {
            check()?;
            let digest = pattern_digest(source);
            let known = &result.assets[..result.count];
            if known.contains(&digest) || result.count == MAX_ASSETS { continue; }
            if known.iter().any(|prior| prior[..16] == digest[..16]) {
                return Err("Conflicting standard Photoshop pattern identifiers".into());
            }
            let png = crate::image_import::prepared_png::pattern(source, pool, &mut || check())?;
            let (width, height, depth) = (png.width(), png.height(), png.depth());
            if depth > document_depth { continue; }
            let pixels = png.into_pixels();
            let padded = u64::from(width.div_ceil(crate::raster::TILE))
                * u64::from(height.div_ceil(crate::raster::TILE)) * u64::from(crate::raster::TILE).pow(2)
                * u64::from(depth / 8) * 4;
            let opaque = pixels.chunks_exact(8).all(|pixel| pixel[6..] == [255; 2]);
            let channels = if opaque { 3 } else { 4 };
            let plane_bytes = width * height * u32::from(depth / 8);
            let array_bytes = 20 + channels * (31 + plane_bytes) + (5 - channels) * 4;
            let asset_bytes = 16 + 2 * (NAME.len() as u32 + 1) + 37 + 8 + array_bytes;
            if decoded_bytes + padded > 16 * 1024 * 1024
                || result.bytes.len() + 4 + asset_bytes.next_multiple_of(4) as usize > MAX_BLOCK
            { continue; }
            if result._charge.is_none() {
                result._charge = Some(pool.reserve_bytes(MAX_BLOCK, MAX_BLOCK).map_err(|e| e.to_string())?);
                result.bytes.try_reserve_exact(MAX_BLOCK).map_err(|_| "Photoshop pattern allocation failed")?;
                if result.bytes.capacity() != MAX_BLOCK { return Err("Photoshop pattern allocation exceeds admission".into()); }
            }
            let id: PatternId = digest[..16].try_into().unwrap();
            let uuid = uuid::Uuid::from_bytes(id);
            let mut text = [0u8; 36];
            let id_text = uuid.hyphenated().encode_lower(&mut text).as_bytes();
            let out = &mut result.bytes;
            let start = out.len();
            for word in [asset_bytes, 1, 3] { super::u32b(out, word); }
            super::u16b(out, height as u16);
            super::u16b(out, width as u16);
            super::u32b(out, NAME.len() as u32 + 1);
            for word in NAME.encode_utf16().chain([0]) { super::u16b(out, word); }
            out.push(id_text.len() as u8);
            out.extend_from_slice(id_text);
            for word in [3, array_bytes, 0, 0, height, width, 3] { super::u32b(out, word); }
            for component in [Some(0), Some(1), Some(2), None, (!opaque).then_some(3)] {
                let Some(component) = component else { super::u32b(out, 0); continue; };
                for word in [1, 23 + plane_bytes, u32::from(depth), 0, 0, height, width] { super::u32b(out, word); }
                super::u16b(out, depth);
                out.push(0);
                for (y, row) in pixels.chunks_exact(width as usize * 8).enumerate() {
                    if y % 32 == 0 { check()?; }
                    for pixel in row.chunks_exact(8) {
                        let at = component * 2;
                        out.push(pixel[at]);
                        if depth == 16 { out.push(pixel[at + 1]); }
                    }
                }
            }
            if out.len() - start != 4 + asset_bytes as usize {
                return Err("Photoshop pattern byte count differs from admission".into());
            }
            out.resize(out.len().next_multiple_of(4), 0);
            result.assets[result.count] = digest;
            result.count += 1;
            decoded_bytes += padded;
        }
        Ok(result)
    }

    pub(crate) fn record(&self, fill: StandardFill<'_>) -> Option<FillRecord> {
        Some(FillRecord {
            id: self.id(fill.png)?,
            placement: fill.placement,
        })
    }

    pub(super) fn id(&self, png: &str) -> Option<PatternId> {
        if self.count == 0 {
            return None;
        }
        let digest = pattern_digest(png);
        self.assets[..self.count].contains(&digest).then(|| digest[..16].try_into().unwrap())
    }
    pub(super) fn bytes(&self) -> &[u8] { &self.bytes }

    pub(super) fn encoded_bytes(&self) -> u64 {
        if self.bytes.is_empty() { 0 } else { 12 + self.bytes.len() as u64 }
    }

    pub(super) fn write(&self, output: &mut dyn std::io::Write) -> Result<(), String> {
        if !self.bytes.is_empty() {
            output.write_all(b"8BIMPatt").map_err(|e| e.to_string())?;
            output.write_all(&(self.bytes.len() as u32).to_be_bytes()).map_err(|e| e.to_string())?;
            output.write_all(&self.bytes).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

fn pattern_digest(source: &str) -> Digest {
    ring::digest::digest(&ring::digest::SHA256, source.as_bytes()).as_ref().try_into().unwrap()
}

const DESCRIPTOR_BYTES: usize = 48 + (16 + 2 * (NAME.len() + 1)) + (16 + 2 * 37) + 13 + 24;
const OFFSET_DESCRIPTOR_BYTES: usize = DESCRIPTOR_BYTES + 69;
const ROTATION_DESCRIPTOR_BYTES: usize = DESCRIPTOR_BYTES + 24;
const FULL_DESCRIPTOR_BYTES: usize = OFFSET_DESCRIPTOR_BYTES + 24;

pub(super) struct Descriptor {
    bytes: [u8; FULL_DESCRIPTOR_BYTES],
    len: usize,
}

impl Descriptor {
    pub(super) fn bytes(&self) -> &[u8] { &self.bytes[..self.len] }
}

pub(super) fn descriptor(fill: FillRecord) -> Descriptor {
    let mut out = Descriptor { bytes: [0; FULL_DESCRIPTOR_BYTES], len: 0 };
    use std::io::Write;
    let mut w = std::io::Cursor::new(out.bytes.as_mut_slice());
    let Placement { phase, angle } = fill.placement;
    let offset = phase != [0, 0];
    let rotation = angle != 0;
    w.write_all(b"\0\0\0\x10\0\0\0\0\0\0\0\0null").unwrap();
    w.write_all(&(3 + u32::from(offset) + u32::from(rotation)).to_be_bytes()).unwrap();
    write_identity(&mut w, fill.id).unwrap();
    w.write_all(b"\0\0\0\0Lnkdbool\0\0\0\0\0Scl UntF#Prc").unwrap();
    w.write_all(&100f64.to_be_bytes()).unwrap();
    debug_assert_eq!(w.position(), DESCRIPTOR_BYTES as u64);
    if offset {
        write_phase(&mut w, phase).unwrap();
        debug_assert_eq!(w.position(), OFFSET_DESCRIPTOR_BYTES as u64);
    }
    if rotation {
        write_angle(&mut w, angle).unwrap();
    }
    out.len = w.position() as usize;
    out
}

pub(super) fn write_identity(w: &mut impl std::io::Write, id: PatternId) -> std::io::Result<()> {
    let uuid = uuid::Uuid::from_bytes(id);
    let mut text = [0; 36];
    let id = uuid.hyphenated().encode_lower(&mut text);
    w.write_all(b"\0\0\0\0PtrnObjc\0\0\0\0\0\0\0\0Ptrn\0\0\0\x02")?;
    for (key, value) in [(b"Nm  ", NAME), (b"Idnt", &*id)] {
        w.write_all(&[0; 4])?;
        w.write_all(key)?;
        w.write_all(b"TEXT")?;
        w.write_all(&(value.len() as u32 + 1).to_be_bytes())?;
        for code in value.encode_utf16().chain([0]) {
            w.write_all(&code.to_be_bytes())?;
        }
    }
    Ok(())
}

pub(super) fn write_phase(w: &mut impl std::io::Write, [x, y]: [i32; 2]) -> std::io::Result<()> {
    w.write_all(b"\0\0\0\x05phaseObjc\0\0\0\0\0\0\0\0Pnt \0\0\0\x02")?;
    for (key, value) in [(b"\0\0\0\0Hrzn", x), (b"\0\0\0\0Vrtc", y)] {
        w.write_all(key)?;
        w.write_all(b"doub")?;
        w.write_all(&f64::from(value).to_be_bytes())?;
    }
    Ok(())
}

pub(super) fn write_angle(w: &mut impl std::io::Write, angle: i32) -> std::io::Result<()> {
    w.write_all(b"\0\0\0\0AnglUntF#Ang")?;
    w.write_all(&f64::from(angle).to_be_bytes())
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Source {
    pub source_index: usize,
    pub tag: String,
    pub name: String,
    pub pattern_id: String,
    /// Asset coordinates, not a placement in the parent document.
    pub bounds: [i32; 4],
    pub width: u32,
    pub height: u32,
    pub bit_depth: u16,
    pub color_space: &'static str,
    #[serde(skip)]
    pub pixels: Arc<Raster>,
}
fn rectangle(c: &mut Cursor<'_>) -> Result<[i32; 4], String> {
    let top = c.i32()?;
    let left = c.i32()?;
    let bottom = c.i32()?;
    let right = c.i32()?;
    if [left, top, right, bottom]
        .iter()
        .any(|v| v.abs_diff(0) > 32768)
        || left >= right
        || top >= bottom
    {
        return Err("Invalid pattern rectangle".into());
    }
    Ok([left, top, right, bottom])
}
// Validate one complete zlib stream with a fixed scratch buffer. Decoding below
// still uses the shared native row reader; oversized output, concatenation and
// incomplete/checksum-invalid streams never substitute pixels.
fn validate_zip(payload: &[u8], expected: usize, control: &Control) -> Result<(), String> {
    let mut zip = flate2::Decompress::new(true);
    let mut scratch = [0u8; 16384];
    loop {
        control.check()?;
        let before = (zip.total_in(), zip.total_out());
        let status = zip
            .decompress(
                &payload[before.0 as usize..],
                &mut scratch,
                flate2::FlushDecompress::None,
            )
            .map_err(|_| "Invalid pattern ZIP stream")?;
        if zip.total_out() > expected as u64 {
            return Err("Pattern ZIP output exceeds its native channel dimensions".into());
        }
        if status == flate2::Status::StreamEnd {
            return if zip.total_out() == expected as u64 && zip.total_in() == payload.len() as u64 {
                Ok(())
            } else {
                Err("Pattern ZIP stream length does not match its native channel".into())
            };
        }
        if before == (zip.total_in(), zip.total_out()) {
            return Err("Incomplete pattern ZIP stream".into());
        }
    }
}
fn text(c: &mut Cursor<'_>) -> Result<String, String> {
    let count = c.u32()? as usize;
    if count > 1024 {
        return Err("Pattern name exceeds 1024 code units".into());
    }
    let words = c
        .bytes(count * 2)?
        .chunks_exact(2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .collect::<Vec<_>>();
    Ok(String::from_utf16(&words)
        .map_err(|_| "Invalid pattern Unicode")?
        .trim_end_matches('\0')
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect())
}
pub fn decode(data: &[u8], tag: &str, control: &Control) -> Result<Vec<Source>, String> {
    decode_admitted(data, tag, control, &mut |_| Ok(()))
}

pub(super) fn decode_admitted(
    data: &[u8], tag: &str, control: &Control,
    admit: &mut dyn FnMut(usize) -> Result<(), String>,
) -> Result<Vec<Source>, String> {
    if data.len() > MAX_BLOCK {
        return Err("Pattern block exceeds 8 MiB".into());
    }
    if !matches!(tag, "Patt" | "Pat2" | "Pat3") {
        return Err("Unsupported pattern tag".into());
    }
    let mut list = Cursor::new(data);
    let mut sources = Vec::new();
    let mut ids = HashSet::new();
    let mut total = 0u64;
    while list.pos < data.len() {
        control.check()?;
        if sources.len() >= 16 {
            return Err("Pattern block exceeds 16 assets".into());
        }
        let bytes = list.block()?;
        let padding = list.bytes((4 - bytes.len() % 4) % 4)?;
        if padding.iter().any(|b| *b != 0) {
            return Err("Invalid pattern padding".into());
        }
        let mut c = Cursor::new(bytes);
        if c.u32()? != 1 {
            return Err("Unsupported pattern version".into());
        }
        if c.u32()? != 3 {
            return Err(
                "Only raw RGB pattern assets can be inspected; other color modes remain protected"
                    .into(),
            );
        }
        let point = [c.i16()?, c.i16()?];
        let name = text(&mut c)?;
        let count = c.u8()? as usize;
        if count == 0 || count > 128 {
            return Err("Pattern identifier must contain 1–128 bytes".into());
        }
        let id = c.bytes(count)?;
        if !id.iter().all(|b| (32..=126).contains(b)) {
            return Err("Invalid pattern identifier".into());
        }
        let pattern_id = std::str::from_utf8(id).unwrap().to_string();
        if !ids.insert(pattern_id.clone()) {
            return Err("Duplicate pattern identifier".into());
        }
        if c.u32()? != 3 {
            return Err("Unsupported pattern array version".into());
        }
        let mut arrays = Cursor::new(c.block()?);
        let bounds = rectangle(&mut arrays)?;
        let width = (bounds[2] - bounds[0]) as u32;
        let height = (bounds[3] - bounds[1]) as u32;
        if width > 512 || height > 512 || point != [height as i16, width as i16] {
            return Err("Pattern size must match its rectangle within 512 × 512 pixels".into());
        }
        let count = arrays.u32()? as usize;
        if !(3..=32).contains(&count) {
            return Err("Unsupported pattern channel count".into());
        }
        let mut channels = Vec::new();
        let mut depth = None;
        for index in 0..count + 2 {
            let written = arrays.u32()?;
            if written > 1 {
                return Err("Invalid pattern written flag".into());
            }
            if written == 0 {
                if index < 3 {
                    return Err("Missing RGB pattern channel".into());
                }
                continue;
            }
            let bytes = arrays.block()?;
            if bytes.is_empty() {
                return Err("Empty written pattern channel".into());
            }
            // RGB occupies the first three color slots. Photoshop presets put
            // optional transparency in the final slot, after the reserved slot.
            // Never reinterpret an extra color plane or mask as transparency.
            if index >= 3 && index != count + 1 {
                return Err("Pattern masks and extra written channels remain protected".into());
            }
            let mut channel = Cursor::new(bytes);
            let d = channel.u32()?;
            if ![8, 16].contains(&d) || depth.is_some_and(|prior| prior != d) {
                return Err("Pattern channels require consistent native 8/16-bit samples".into());
            }
            depth = Some(d);
            if rectangle(&mut channel)? != bounds || channel.u16()? != d as u16 {
                return Err("Pattern channel bounds/depth do not match the asset".into());
            }
            let compression = channel.u8()?;
            let payload = channel.bytes(bytes.len() - channel.pos)?;
            let row_bytes = width as usize * (d as usize / 8);
            // Observed mode 1 is PackBits with 16-bit row counts, including in PSB.
            // ZIP framing is checked separately before using the shared row reader.
            let expected = match compression {
                0 => row_bytes * height as usize,
                1 => {
                    let mut rows = Cursor::new(payload);
                    let mut n = height as usize * 2;
                    for _ in 0..height {
                        let size = rows.u16()? as usize;
                        if size == 0 || size > row_bytes * 2 + 16 {
                            return Err("Invalid pattern RLE row length".into());
                        }
                        n += size;
                    }
                    n
                }
                2 | 3 => {
                    validate_zip(payload, row_bytes * height as usize, control)?;
                    payload.len()
                }
                _ => {
                    return Err(
                        "Only raw, PackBits, ZIP and ZIP-predicted RGB pattern channels can be inspected".into(),
                    )
                }
            };
            if payload.len() != expected {
                return Err("Pattern channel length does not match its native rows".into());
            }
            channels.push((if index < 3 { index } else { 3 }, compression, payload));
        }
        if arrays.pos != arrays.data.len() || c.pos != bytes.len() {
            return Err("Unknown trailing pattern data".into());
        }
        let d = depth.ok_or("Missing pattern channel precision")? as u16;
        let bytes = u64::from(width.div_ceil(crate::raster::TILE))
            * u64::from(height.div_ceil(crate::raster::TILE))
            * u64::from(crate::raster::TILE).pow(2)
            * if d == 16 { 8 } else { 4 };
        total += bytes;
        if total > 16 * 1024 * 1024 {
            return Err("Decoded pattern assets exceed 16 MiB".into());
        }
        admit(bytes as usize)?;
        let mut pixels = Raster::new_depth(width, height, d);
        for (index, compression, payload) in &channels {
            let mut encoded = Vec::with_capacity(payload.len() + 2);
            encoded.extend_from_slice(&u16::from(*compression).to_be_bytes());
            encoded.extend_from_slice(payload);
            super::stream_layer_channel(
                &encoded,
                &mut pixels,
                *index,
                *index == 0,
                Layout::Psd,
                control,
            )?;
        }
        sources.push(Source {
            source_index: sources.len(),
            tag: tag.into(),
            name,
            pattern_id,
            bounds,
            width,
            height,
            bit_depth: d,
            color_space: "unmanaged_source_samples",
            pixels: Arc::new(pixels),
        });
    }
    Ok(sources)
}
