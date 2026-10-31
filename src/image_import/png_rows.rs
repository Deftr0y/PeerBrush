//! Native rows for the explicitly supported static PNG layout; no display sampling.
use crate::{
    color_profile,
    loading::Control,
    raster::{Raster, TILE},
};
use image::metadata::Orientation;
use std::{
    io::{BufReader, Cursor, Read, Seek, SeekFrom},
    sync::Arc,
};

pub(super) fn supported(bytes: &[u8], orientation: Orientation) -> bool {
    orientation == Orientation::NoTransforms
        && bytes.get(24).is_some_and(|depth| matches!(*depth, 8 | 16))
        && bytes.get(25).is_some_and(|color| matches!(*color, 2 | 6))
        && bytes.get(28) == Some(&0)
}

struct Checked<'a> {
    input: Cursor<&'a [u8]>,
    control: &'a Control,
}
impl Read for Checked<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.control.check().map_err(std::io::Error::other)?;
        let length = bytes.len().min(64 * 1024);
        self.input.read(&mut bytes[..length])
    }
}
impl Seek for Checked<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.control.check().map_err(std::io::Error::other)?;
        self.input.seek(position)
    }
}

fn store8(pixels: &mut Raster, y: u32, row: &[u8]) {
    let offset = (y % TILE * TILE * 4) as usize;
    for (x, part) in row.chunks(TILE as usize * 4).enumerate() {
        let key = (x as u32, y / TILE);
        if part.iter().all(|sample| *sample == 0) && !pixels.tiles.contains_key(&key) {
            continue;
        }
        let tile = pixels
            .tiles
            .entry(key)
            .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
        Arc::make_mut(tile)[offset..offset + part.len()].copy_from_slice(part);
    }
}
fn store16(pixels: &mut Raster, y: u32, row: &[u16]) {
    let offset = (y % TILE * TILE * 4) as usize;
    for (x, part) in row.chunks(TILE as usize * 4).enumerate() {
        let key = (x as u32, y / TILE);
        if part.iter().all(|sample| *sample == 0) && !pixels.samples16.contains_key(&key) {
            continue;
        }
        let tile = pixels
            .samples16
            .entry(key)
            .or_insert_with(|| Arc::new(vec![0; (TILE * TILE * 4) as usize]));
        Arc::make_mut(tile)[offset..offset + part.len()].copy_from_slice(part);
    }
}

pub(super) fn decode(
    bytes: &[u8],
    width: u32,
    height: u32,
    depth: u16,
    profile: Option<&[u8]>,
    control: &Control,
) -> Result<Raster, String> {
    control.check()?;
    let input = Checked {
        input: Cursor::new(bytes),
        control,
    };
    let mut decoder = ::png::Decoder::new_with_limits(
        BufReader::with_capacity(64 * 1024, input),
        ::png::Limits {
            bytes: super::BUDGET,
        },
    );
    decoder.ignore_checksums(false);
    decoder.set_ignore_text_chunk(false);
    decoder.set_transformations(::png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|error| error.to_string())?;
    let info = reader.info();
    if (info.width, info.height) != (width, height)
        || info.interlaced
        || info.animation_control.is_some()
        || info.frame_control.is_some()
        || !matches!(
            info.color_type,
            ::png::ColorType::Rgb | ::png::ColorType::Rgba
        )
        || info.bit_depth as u16 != depth
        || info.icc_profile.as_deref() != profile
    {
        return Err("PNG rows differ from the inspected static native source".into());
    }
    let (color, bits) = reader.output_color_type();
    let channels = match color {
        ::png::ColorType::Rgb => 3,
        ::png::ColorType::Rgba => 4,
        _ => return Err("PNG row color differs from its native RGB source".into()),
    };
    if bits as u16 != depth {
        return Err("PNG rows did not retain their native precision".into());
    }
    // Acquire one owned existing CMS executor, not one full-profile cache lookup per row.
    let conversion = profile
        .filter(|value| !color_profile::is_srgb_profile(value))
        .map(color_profile::transforms)
        .transpose()?;
    let mut pixels = Raster::new_depth(width, height, depth);
    let mut rgba8 = if depth == 8 {
        vec![0; width as usize * 4]
    } else {
        Vec::new()
    };
    let mut rgba16 = if depth == 16 {
        vec![0; width as usize * 4]
    } else {
        Vec::new()
    };
    for y in 0..height {
        control.progress("Decoding image", y as usize, height as usize)?;
        let row = reader
            .next_row()
            .map_err(|error| error.to_string())?
            .ok_or("PNG ended before all native rows were decoded")?;
        let data = row.data();
        if data.len() != width as usize * channels * usize::from(depth / 8) {
            return Err("PNG row length differs from its native dimensions".into());
        }
        if depth == 16 {
            for (source, target) in data
                .chunks_exact(channels * 2)
                .zip(rgba16.chunks_exact_mut(4))
            {
                target[0] = u16::from_be_bytes([source[0], source[1]]);
                target[1] = u16::from_be_bytes([source[2], source[3]]);
                target[2] = u16::from_be_bytes([source[4], source[5]]);
                target[3] = if channels == 4 {
                    u16::from_be_bytes([source[6], source[7]])
                } else {
                    65535
                };
            }
            if let Some(conversion) = &conversion {
                conversion.convert16(&mut rgba16)?;
            }
            store16(&mut pixels, y, &rgba16);
        } else {
            for (source, target) in data.chunks_exact(channels).zip(rgba8.chunks_exact_mut(4)) {
                target[..3].copy_from_slice(&source[..3]);
                target[3] = if channels == 4 { source[3] } else { 255 };
            }
            if let Some(conversion) = &conversion {
                conversion.convert8(&mut rgba8)?;
            }
            store8(&mut pixels, y, &rgba8);
        }
        #[cfg(test)]
        tests::observe(
            Some(y + 1),
            data.len(),
            rgba8.len() + rgba16.len() * 2,
            pixels.stored_bytes(),
        );
    }
    control.check()?;
    if reader
        .next_row()
        .map_err(|error| error.to_string())?
        .is_some()
    {
        return Err("PNG contains unexpected native rows".into());
    }
    control.check()?;
    // Complete the IDAT stream and validate all chunks through IEND, not just pixels.
    reader.finish().map_err(|error| error.to_string())?;
    if reader.info().icc_profile.as_deref() != profile {
        return Err("PNG color profile changed after its image data".into());
    }
    #[cfg(test)]
    tests::observe(None, 0, 0, pixels.stored_bytes());
    control.progress("Decoding image", height as usize, height as usize)?;
    Ok(pixels)
}

#[cfg(test)]
mod tests;
