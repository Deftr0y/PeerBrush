//! Bounded raw RGB pattern inspection; decoding assets never authorizes Photoshop editing.
//! Structure: https://www.adobe.com/devnet-apps/photoshop/fileformatashtml/
use super::{Cursor, Layout};
use crate::{loading::Control, raster::Raster};
use std::{collections::HashSet, sync::Arc};

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
    if data.len() > 8 * 1024 * 1024 {
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
                if index < 3 {
                    return Err("Empty RGB pattern channel".into());
                }
                continue;
            }
            if index >= 3 {
                return Err(
                    "Pattern masks, transparency and extra written channels remain protected"
                        .into(),
                );
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
            // Reject other modes explicitly; never guess a compression codec.
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
                _ => {
                    return Err(
                        "Only raw and PackBits RGB pattern channels can be inspected".into(),
                    )
                }
            };
            if payload.len() != expected {
                return Err("Pattern channel length does not match its native rows".into());
            }
            channels.push((compression, payload));
        }
        if arrays.pos != arrays.data.len() || c.pos != bytes.len() {
            return Err("Unknown trailing pattern data".into());
        }
        let d = depth.ok_or("Missing pattern channel precision")? as u16;
        total += u64::from(width.div_ceil(crate::raster::TILE))
            * u64::from(height.div_ceil(crate::raster::TILE))
            * u64::from(crate::raster::TILE).pow(2)
            * if d == 16 { 8 } else { 4 };
        if total > 16 * 1024 * 1024 {
            return Err("Decoded pattern assets exceed 16 MiB".into());
        }
        let mut pixels = Raster::new_depth(width, height, d);
        for (index, (compression, payload)) in channels.iter().enumerate() {
            let mut encoded = Vec::with_capacity(payload.len() + 2);
            encoded.extend_from_slice(&u16::from(*compression).to_be_bytes());
            encoded.extend_from_slice(payload);
            super::stream_layer_channel(
                &encoded,
                &mut pixels,
                index,
                index == 0,
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
