//! Stream BMP rows and sparse RLE into private source tiles.
use super::source_file::{FileLimits, FileSource};
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    large_document::{ResidentBudget, ScratchBytes},
    raster::{CanvasLimits, NATIVE_EDGE},
    selection::NativeRows,
    workspace::source_runtime::SourceSession,
};
use image::{
    codecs::{bmp::BmpDecoder, ico::IcoDecoder},
    ColorType, ImageDecoder,
};
use std::{
    fs::File,
    io::{Cursor, Read, Seek},
    path::Path,
};
mod validation;
pub(crate) use validation::{check_reader, validate_dib};

// All supported DIB headers, external masks and the maximum 256-entry palette.
const PREFIX: usize = 1200;
const METADATA_BYTES: usize = 4096;
pub(super) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    encoded_bytes: 2 * 1024 * 1024 * 1024,
};
#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    prefix: [u8; PREFIX],
    length: usize,
    dib: usize,
    offset: usize,
    width: u32,
    height: u32,
    bits: usize,
    compression: u32,
    top_down: bool,
    color: ColorType,
    icon: bool,
}
impl Header {
    fn read(input: &mut impl Read) -> Result<Self, String> {
        Self::read_kind(input, false)
    }
    fn read_kind(input: &mut impl Read, icon: bool) -> Result<Self, String> {
        let mut prefix = [0; PREFIX];
        if icon {
            prefix[..2].copy_from_slice(b"BM");
        } else {
            input
                .read_exact(&mut prefix[..14])
                .map_err(|_| "Truncated BMP file header")?;
        }
        input
            .read_exact(&mut prefix[14..18])
            .map_err(|_| "Truncated BMP header")?;
        if &prefix[..2] != b"BM" {
            return Err("Invalid BMP signature".into());
        }
        let dib = u32::from_le_bytes(prefix[14..18].try_into().unwrap()) as usize;
        if !matches!(dib, 12 | 40 | 52 | 56 | 108 | 124) {
            return Err("Unsupported BMP DIB header".into());
        }
        let mut length = 14 + dib;
        input
            .read_exact(&mut prefix[18..length])
            .map_err(|_| "Truncated BMP DIB header")?;
        let bits_at = if dib == 12 { 24 } else { 28 };
        let bits = usize::from(u16::from_le_bytes(
            prefix[bits_at..bits_at + 2].try_into().unwrap(),
        ));
        let compression = if dib == 12 {
            0
        } else {
            u32::from_le_bytes(prefix[30..34].try_into().unwrap())
        };
        let masks =
            usize::from(compression == 3 && (dib == 40 || (icon && matches!(dib, 108 | 124)))) * 12;
        let palette = if matches!(bits, 1 | 2 | 4 | 8) && compression <= 2 {
            let used = if dib == 12 {
                0
            } else {
                u32::from_le_bytes(prefix[46..50].try_into().unwrap()) as usize
            };
            if used > 1 << bits {
                return Err("BMP palette exceeds its index depth".into());
            }
            (if used == 0 { 1 << bits } else { used }) * if dib == 12 { 3 } else { 4 }
        } else {
            0
        };
        let end = length + masks + palette;
        if end > PREFIX {
            return Err("BMP metadata exceeds admission".into());
        }
        input
            .read_exact(&mut prefix[length..end])
            .map_err(|_| "Truncated BMP masks or palette")?;
        length = end;
        if icon {
            prefix[10..14].copy_from_slice(&(length as u32).to_le_bytes());
        }
        let offset = u32::from_le_bytes(prefix[10..14].try_into().unwrap()) as usize;
        if offset < length || offset as u64 >= FILE_LIMITS.encoded_bytes {
            return Err("BMP pixel offset lies outside admitted file data".into());
        }
        validate_dib(&prefix[14..length])?;
        let (width, height, color) = if icon {
            let mut encoded = [0; PREFIX + 8];
            icon_prefix(&prefix[..length], &mut encoded[..length + 8], 1, 1);
            let decoder =
                IcoDecoder::new(Cursor::new(&encoded[..length + 8])).map_err(|e| e.to_string())?;
            let (w, h) = decoder.dimensions();
            (w, h, decoder.color_type())
        } else {
            let decoder =
                BmpDecoder::new(Cursor::new(&prefix[..length])).map_err(|e| e.to_string())?;
            let (w, h) = decoder.dimensions();
            (w, h, decoder.color_type())
        };
        if icon {
            super::check_budget(width, height, 8, super::BUDGET)?;
        } else {
            CanvasLimits::Native.check(width, height)?;
        }
        let top_down = dib != 12 && i32::from_le_bytes(prefix[22..26].try_into().unwrap()) < 0;
        Ok(Self {
            prefix,
            length,
            dib,
            offset,
            width,
            height,
            bits,
            compression,
            top_down,
            color,
            icon,
        })
    }
    fn stride(&self) -> usize {
        (self.width as usize * self.bits).div_ceil(32) * 4
    }
    fn check_extent(&self, length: u64) -> Result<(), String> {
        let minimum_pixels = if matches!(self.compression, 1 | 2) {
            2 // At least one RLE instruction.
        } else {
            self.stride() as u64 * u64::from(self.height)
        };
        if self.offset as u64 + minimum_pixels > length {
            return Err("Truncated BMP pixel data".into());
        }
        Ok(())
    }
    fn row_prefix(&self, output: &mut [u8]) {
        let length = output.len() as u32;
        output[..self.length].copy_from_slice(&self.prefix[..self.length]);
        output[10..14].copy_from_slice(&(self.length as u32).to_le_bytes());
        output[2..6].copy_from_slice(&length.to_le_bytes());
        if self.dib == 12 {
            output[20..22].copy_from_slice(&1u16.to_le_bytes());
        } else {
            output[22..26].copy_from_slice(&1i32.to_le_bytes());
            output[34..38].copy_from_slice(&(self.stride() as u32).to_le_bytes());
        }
    }
}

fn icon_prefix(bmp: &[u8], output: &mut [u8], width: u32, height: u32) {
    output[..22].fill(0);
    output[..6].copy_from_slice(&[0, 0, 1, 0, 1, 0]);
    output[6] = width.min(256) as u8;
    output[7] = height.min(256) as u8;
    output[10..12].copy_from_slice(&1u16.to_le_bytes());
    let length = (output.len() - 22) as u32;
    output[14..18].copy_from_slice(&length.to_le_bytes());
    output[18..22].copy_from_slice(&22u32.to_le_bytes());
    output[22..bmp.len() + 8].copy_from_slice(&bmp[14..]);
}

pub(super) fn inspect(path: &Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let mut file = File::open(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > FILE_LIMITS.encoded_bytes {
        return Err("Encoded BMP image must be a file at most 2 GiB".into());
    }
    let header = Header::read(&mut file)?;
    header.check_extent(metadata.len())?;
    Ok(super::Info {
        format: "Bmp".into(),
        width: header.width,
        height: header.height,
        bit_depth: 8,
        choice: None,
        count: 1,
    })
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let input = FileSource::open(
        session,
        path,
        FILE_LIMITS,
        "bmp",
        &mut || check(),
    )?;
    prepare_source(base, session, input, check)
}

pub(super) fn prepare_source(
    base: &NativeDocument,
    session: &SourceSession,
    mut input: FileSource<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let _metadata = base
        .budget()
        .reserve_bytes(METADATA_BYTES, METADATA_BYTES)
        .map_err(|e| e.to_string())?;
    input.start_read()?;
    let header = Header::read(&mut input)?;
    header.check_extent(input.length())?;
    input.admit(&mut || check())?;
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        input.start_read()?;
        if Header::read(&mut input)? != header {
            return Err("BMP header changed after admission".into());
        }
        let mut writer = NativeRows::from_scanlines(
            header.width,
            header.height,
            8,
            store.clone(),
            !header.top_down,
        )
        .map_err(|e| e.to_string())?;
        decode(&mut input.decoder_stream(), &header, base.budget(), check, |y, row| {
            writer
                .row(y, row, &mut || guard())
                .map_err(|e| e.to_string())
        })?;
        input.finish(&mut || check())?;
        check()?;
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })
}

pub(super) fn prepare_icon(
    base: &NativeDocument,
    session: &SourceSession,
    mut input: FileSource<'_>,
    entry: super::ico::Entry,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let _metadata = base
        .budget()
        .reserve_bytes(METADATA_BYTES, METADATA_BYTES)
        .map_err(|e| e.to_string())?;
    input.start_read()?;
    input.skip(entry.offset, &mut || check())?;
    let header = Header::read_kind(&mut (&mut input).take(entry.length), true)?;
    entry.check_dimensions(header.width, header.height, false)?;
    input.finish(&mut || check())?;
    let mut mask_at = 0;
    let decoded = crate::retained::transaction::stage(base, session, check, |store, guard| {
        input.start_read()?;
        input.skip(entry.offset, &mut || check())?;
        let mut payload = (&mut input).take(entry.length);
        if Header::read_kind(&mut payload, true)? != header {
            return Err("Icon DIB changed after admission".into());
        }
        let mut writer = NativeRows::new(header.width, header.height, 8, store.clone())
            .map_err(|e| e.to_string())?;
        decode(&mut payload, &header, base.budget(), check, |y, row| {
            writer
                .row(y, row, &mut || guard())
                .map_err(|e| e.to_string())
        })?;
        drop(payload);
        mask_at = input.stream_position().map_err(|e| e.to_string())?;
        input.finish(&mut || check())?;
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })?;
    let masked = (|| {
        let stride = header.width.div_ceil(32) as usize * 4;
        let remaining = (entry.offset + entry.length)
            .checked_sub(mask_at)
            .ok_or("Icon pixels exceed their payload")?;
        if remaining == 0 {
            return Ok(None);
        }
        if remaining < stride as u64 * u64::from(header.height) {
            return Err("Truncated icon transparency mask".into());
        }
        let output = crate::retained::transaction::stage(base, session, check, |store, guard| {
            input.start_read()?;
            input.skip(mask_at, &mut || check())?;
            let mut mask =
                ScratchBytes::new(base.budget(), stride, stride).map_err(|e| e.to_string())?;
            let width = header.width as usize;
            let mut raw = ScratchBytes::new(base.budget(), width * 4, width * 4)
                .map_err(|e| e.to_string())?;
            let mut row = ScratchBytes::new(base.budget(), width * 8, width * 8)
                .map_err(|e| e.to_string())?;
            let mut writer = NativeRows::new(header.width, header.height, 8, store.clone())
                .map_err(|e| e.to_string())?;
            for y in 0..header.height {
                check()?;
                input
                    .read_exact(&mut mask)
                    .map_err(|_| "Truncated icon transparency mask")?;
                let source_y = if header.top_down {
                    header.height - 1 - y
                } else {
                    y
                };
                let region = crate::raster::backed::Region::new([
                    0,
                    source_y as i32,
                    header.width as i32,
                    source_y as i32 + 1,
                ])
                .map_err(|e| e.to_string())?;
                let pixels = decoded
                    .surface()
                    .prepare_region(region, &mut || guard())
                    .map_err(|e| e.to_string())?;
                pixels
                    .copy_row(source_y as i32, &mut raw)
                    .map_err(|e| e.to_string())?;
                drop(pixels);
                for (x, (pixel, out)) in
                    raw.chunks_exact(4).zip(row.chunks_exact_mut(8)).enumerate()
                {
                    write_pixel(
                        out,
                        [
                            pixel[0],
                            pixel[1],
                            pixel[2],
                            if mask[x / 8] & (128 >> (x % 8)) != 0 {
                                0
                            } else {
                                pixel[3]
                            },
                        ],
                    );
                }
                writer
                    .row(y, &row, &mut || guard())
                    .map_err(|e| e.to_string())?;
            }
            input.finish(&mut || check())?;
            writer.finish(&mut || guard()).map_err(|e| e.to_string())
        })?;
        Ok(Some(output))
    })();
    let (output, top_down) = match masked {
        Ok(None) => (decoded, header.top_down),
        result => match (result, decoded.abort(session, &|| session.cleanup_check())) {
            (Ok(Some(output)), Ok(())) => (output, false),
            (Err(reason), Ok(())) => return Err(reason),
            (Err(reason), Err(cleanup)) => {
                return Err(format!("{reason}; icon retirement refused: {cleanup}"))
            }
            (Ok(Some(output)), Err(reason)) => {
                return match output.abort(session, &|| session.cleanup_check()) {
                    Ok(()) => Err(reason),
                    Err(cleanup) => Err(format!(
                        "{reason}; icon candidate retirement refused: {cleanup}"
                    )),
                };
            }
            (Ok(None), _) => unreachable!(),
        },
    };
    super::native::orient_staged(
        base,
        session,
        output,
        if top_down || header.height == 1 {
            image::metadata::Orientation::NoTransforms
        } else {
            image::metadata::Orientation::FlipVertical
        },
        check,
    )
}

fn decode(
    input: &mut impl Read,
    header: &Header,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
    mut emit: impl FnMut(u32, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let mut gap = header.offset - header.length;
    let mut buffer = [0; 8192];
    while gap > 0 {
        check()?;
        let count = gap.min(buffer.len());
        input
            .read_exact(&mut buffer[..count])
            .map_err(|_| "Truncated BMP pixel offset")?;
        gap -= count;
    }
    let bytes = header.width as usize * 8;
    let mut row = ScratchBytes::new(pool, bytes, bytes).map_err(|e| e.to_string())?;
    if matches!(header.compression, 1 | 2) {
        return rle(input, header, &mut row, check, emit);
    }
    let temporary = header.stride() + METADATA_BYTES;
    let _decoder = pool
        .reserve_bytes(temporary, temporary)
        .map_err(|e| e.to_string())?;
    let prefix_length = header.length + usize::from(header.icon) * 8;
    let bytes = prefix_length + header.stride();
    let mut encoded = ScratchBytes::new(pool, bytes, bytes).map_err(|e| e.to_string())?;
    if header.icon {
        icon_prefix(
            &header.prefix[..header.length],
            &mut encoded,
            header.width,
            1,
        );
        if header.dib == 12 {
            encoded[28..30].copy_from_slice(&2u16.to_le_bytes());
        } else {
            encoded[30..34].copy_from_slice(&2i32.to_le_bytes());
        }
    } else {
        header.row_prefix(&mut encoded);
    }
    let channels = usize::from(header.color.channel_count());
    let bytes = header.width as usize * channels;
    let mut expanded = ScratchBytes::new(pool, bytes, bytes).map_err(|e| e.to_string())?;
    for y in 0..header.height {
        check()?;
        input
            .read_exact(&mut encoded[prefix_length..])
            .map_err(|_| "Truncated BMP row")?;
        if header.icon {
            IcoDecoder::new(Cursor::new(&*encoded))
                .map_err(|e| e.to_string())?
                .read_image(&mut expanded)
                .map_err(|e| e.to_string())?;
        } else {
            BmpDecoder::new(Cursor::new(&*encoded))
                .map_err(|e| e.to_string())?
                .read_image(&mut expanded)
                .map_err(|e| e.to_string())?;
        }
        for (pixel, out) in expanded.chunks_exact(channels).zip(row.chunks_exact_mut(8)) {
            write_pixel(
                out,
                [
                    pixel[0],
                    pixel[1],
                    pixel[2],
                    if channels == 4 { pixel[3] } else { 255 },
                ],
            );
        }
        emit(y, &row)?;
    }
    check()
}
fn write_pixel(out: &mut [u8], rgba: [u8; 4]) {
    for (word, value) in out.chunks_exact_mut(2).zip(rgba) {
        word.copy_from_slice(&(u16::from(value) * 257).to_be_bytes());
    }
}
fn black(row: &mut [u8], icon: bool) {
    row.fill(0);
    if !icon {
        for pixel in row.chunks_exact_mut(8) {
            pixel[6..8].fill(255);
        }
    }
}
fn rle(
    input: &mut impl Read,
    header: &Header,
    row: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    mut emit: impl FnMut(u32, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let decoder =
        BmpDecoder::new(Cursor::new(&header.prefix[..header.length])).map_err(|e| e.to_string())?;
    let palette = decoder.get_palette().ok_or("BMP RLE palette missing")?;
    let (mut x, mut y) = (0usize, 0u32);
    let mut until_check = 0;
    black(row, header.icon);
    while y < header.height {
        if until_check == 0 {
            check()?;
            until_check = 256;
        }
        until_check -= 1;
        let mut pair = [0; 2];
        input
            .read_exact(&mut pair)
            .map_err(|_| "Truncated BMP RLE instruction")?;
        if pair[0] == 0 {
            match pair[1] {
                0 => {
                    emit(y, row)?;
                    y += 1;
                    x = 0;
                    black(row, header.icon);
                    continue;
                }
                1 => {
                    while y < header.height {
                        check()?;
                        emit(y, row)?;
                        y += 1;
                        black(row, header.icon);
                    }
                    return check();
                }
                2 => {
                    let mut delta = [0; 2];
                    input
                        .read_exact(&mut delta)
                        .map_err(|_| "Truncated BMP RLE delta")?;
                    let next_x = x + usize::from(delta[0]);
                    let next_y = y + u32::from(delta[1]);
                    if next_x > header.width as usize || next_y >= header.height {
                        return Err("BMP RLE delta exceeds the image".into());
                    }
                    while y < next_y {
                        check()?;
                        emit(y, row)?;
                        y += 1;
                        black(row, header.icon);
                    }
                    x = next_x;
                    continue;
                }
                _ => {}
            }
        }
        let count = usize::from(if pair[0] == 0 { pair[1] } else { pair[0] });
        if x + count > header.width as usize {
            return Err("BMP RLE run exceeds its row".into());
        }
        let mut literals = [0; 256];
        if pair[0] == 0 {
            let bytes = if header.bits == 4 {
                count.div_ceil(2)
            } else {
                count
            };
            let bytes = bytes + bytes % 2;
            input
                .read_exact(&mut literals[..bytes])
                .map_err(|_| "Truncated BMP RLE literals")?;
        }
        for i in 0..count {
            let packed = if pair[0] != 0 {
                pair[1]
            } else {
                literals[if header.bits == 4 { i / 2 } else { i }]
            };
            let index = if header.bits == 4 {
                if i % 2 == 0 {
                    packed >> 4
                } else {
                    packed & 15
                }
            } else {
                packed
            };
            let [r, g, b] = palette[usize::from(index)];
            // The installed ICO RLE decoder leaves palette alpha zero.
            write_pixel(
                &mut row[(x + i) * 8..(x + i + 1) * 8],
                [r, g, b, if header.icon { 0 } else { 255 }],
            );
        }
        x += count;
    }
    check()
}

#[cfg(test)]
pub(crate) mod tests;
