//! Bounded TGA packet transport; sample expansion stays with the installed codec.
use super::source_file::{FileLimits, FileSource};
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    large_document::{ResidentBudget, ScratchBytes},
    raster::{CanvasLimits, NATIVE_EDGE},
    selection::NativeRows,
    workspace::source_runtime::SourceSession,
};
use image::{codecs::tga::TgaDecoder, ColorType, ImageDecoder};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::Path,
};

pub(super) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    encoded_bytes: 2 * 1024 * 1024 * 1024,
};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    bytes: [u8; 18],
    width: u32,
    height: u32,
    color: ColorType,
}
impl Header {
    fn read(input: &mut impl Read) -> Result<Self, String> {
        let mut bytes = [0; 18];
        input
            .read_exact(&mut bytes)
            .map_err(|_| "Truncated TGA header")?;
        if !matches!(bytes[2], 1 | 2 | 3 | 9 | 10 | 11) || bytes[1] > 1 {
            return Err("Unsupported TGA image or color map type".into());
        }
        if bytes[17] & 0xc0 != 0 {
            return Err("Interleaved TGA images are unsupported".into());
        }
        let mapped = matches!(bytes[2], 1 | 9);
        if mapped && (bytes[1] != 1 || !matches!(bytes[16], 8 | 16) || bytes[16] > bytes[7]) {
            return Err("Invalid TGA color map indices".into());
        }
        if bytes[1] == 1 && !matches!(bytes[7], 15 | 16 | 24 | 32) {
            return Err("Unsupported TGA palette sample size".into());
        }
        let width = u32::from(u16::from_le_bytes([bytes[12], bytes[13]]));
        let height = u32::from(u16::from_le_bytes([bytes[14], bytes[15]]));
        CanvasLimits::Native.check(width, height)?;
        let mut header = Self {
            bytes,
            width,
            height,
            color: ColorType::L8,
        };
        if mapped && header.palette_len() == 0 {
            return Err("TGA color map is empty".into());
        }
        let decoder =
            TgaDecoder::new(Cursor::new(header.plain(1, mapped))).map_err(|e| e.to_string())?;
        header.color = decoder.color_type();
        Ok(header)
    }
    fn mapped(&self) -> bool {
        matches!(self.bytes[2], 1 | 9)
    }
    fn palette_len(&self) -> usize {
        if self.bytes[1] == 1 {
            usize::from(u16::from_le_bytes([self.bytes[5], self.bytes[6]]))
        } else {
            0
        }
    }
    fn raw_stride(&self) -> usize {
        usize::from(self.bytes[16]).div_ceil(8)
    }
    fn check_extent(&self, length: u64) -> Result<(), String> {
        let prefix = 18 + usize::from(self.bytes[0]) + self.palette_bytes();
        let pixels = u64::from(self.width) * u64::from(self.height);
        let stride = self.raw_stride() as u64;
        let minimum = if self.bytes[2] >= 9 {
            pixels.div_ceil(128) * (1 + stride)
        } else {
            pixels * stride
        };
        if prefix as u64 + minimum > length {
            return Err("Truncated TGA pixel data".into());
        }
        Ok(())
    }
    fn palette_bytes(&self) -> usize {
        self.palette_len() * usize::from(self.bytes[7]).div_ceil(8)
    }
    fn plain(&self, width: u16, palette: bool) -> [u8; 18] {
        let mut bytes = self.bytes;
        bytes[..12].fill(0);
        bytes[2] = if palette || !matches!(self.bytes[2], 3 | 11) {
            2
        } else {
            3
        };
        bytes[12..14].copy_from_slice(&width.to_le_bytes());
        bytes[14..16].copy_from_slice(&1u16.to_le_bytes());
        bytes[16] = if palette {
            self.bytes[7]
        } else {
            self.bytes[16]
        };
        bytes[17] = self.bytes[17] & 15 | 0x20;
        bytes
    }
}

pub(super) fn inspect(path: &Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let mut file = File::open(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > FILE_LIMITS.encoded_bytes {
        return Err("Encoded TGA image must be a file at most 2 GiB".into());
    }
    let header = Header::read(&mut file)?;
    header.check_extent(metadata.len())?;
    Ok(super::Info {
        format: "Tga".into(),
        width: header.width,
        height: header.height,
        bit_depth: 8,
        choice: None,
        count: 1,
    })
}

pub(super) fn check_header(input: &mut FileSource<'_>) -> Result<(), String> {
    Header::read(input)?.check_extent(input.length())
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
        "tga",
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
    input.start_read()?;
    let header = Header::read(&mut input)?;
    header.check_extent(input.length())?;
    input.admit(&mut || check())?;
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        input.start_read()?;
        if Header::read(&mut input)? != header {
            return Err("TGA header changed after admission".into());
        }
        let mut writer = NativeRows::from_scanlines(
            header.width,
            header.height,
            8,
            store.clone(),
            header.bytes[17] & 0x20 == 0,
        )
        .map_err(|e| e.to_string())?;
        let mut checked = || guard();
        decode(
            &mut input.decoder_stream(),
            &header,
            base.budget(),
            check,
            |y, row| {
                if row.iter().all(|&byte| byte == 0) {
                    return Ok(());
                }
                writer.zero_until(y, &mut checked).map_err(|e| e.to_string())?;
                writer.row(y, row, &mut checked).map_err(|e| e.to_string())
            },
        )?;
        writer.zero_until(header.height, &mut checked).map_err(|e| e.to_string())?;
        input.finish(&mut || check())?;
        check()?;
        writer.finish(&mut checked).map_err(|e| e.to_string())
    })
}

struct Packets {
    remaining: u64,
    left: usize,
    repeat: bool,
    pixel: [u8; 4],
}
impl Packets {
    fn row(
        &mut self,
        input: &mut impl Read,
        raw: &mut [u8],
        stride: usize,
        encoded: bool,
    ) -> Result<(), String> {
        if !encoded {
            input.read_exact(raw).map_err(|_| "Truncated TGA pixels")?;
            self.remaining -= (raw.len() / stride) as u64;
            return Ok(());
        }
        let mut at = 0;
        while at < raw.len() {
            if self.left == 0 {
                let mut tag = [0];
                input
                    .read_exact(&mut tag)
                    .map_err(|_| "Truncated TGA packet")?;
                self.left = usize::from(tag[0] & 127) + 1;
                if self.left as u64 > self.remaining {
                    return Err("TGA packet exceeds the declared image".into());
                }
                self.repeat = tag[0] & 128 != 0;
                if self.repeat {
                    input
                        .read_exact(&mut self.pixel[..stride])
                        .map_err(|_| "Truncated TGA repeated pixel")?;
                }
            }
            let count = self.left.min((raw.len() - at) / stride);
            let out = &mut raw[at..at + count * stride];
            if self.repeat {
                for pixel in out.chunks_exact_mut(stride) {
                    pixel.copy_from_slice(&self.pixel[..stride]);
                }
            } else {
                input
                    .read_exact(out)
                    .map_err(|_| "Truncated TGA raw packet")?;
            }
            at += out.len();
            self.left -= count;
            self.remaining -= count as u64;
        }
        Ok(())
    }
}

fn decode(
    input: &mut impl Read,
    header: &Header,
    pool: &ResidentBudget,
    check: &dyn Fn() -> Result<(), String>,
    mut emit: impl FnMut(u32, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    check()?;
    let mut id = [0; 255];
    input
        .read_exact(&mut id[..usize::from(header.bytes[0])])
        .map_err(|_| "Truncated TGA image ID")?;
    let channels = usize::from(header.color.channel_count());
    let palette_bytes = header.palette_bytes();
    // The codec uses one temporary raw copy when expanding RGB5 or a color map.
    let temporary = palette_bytes.max(header.width as usize * header.raw_stride());
    let _decoder = pool
        .reserve_bytes(temporary, temporary)
        .map_err(|e| e.to_string())?;
    let mut palette = ScratchBytes::new(
        pool,
        if header.mapped() {
            header.palette_len() * channels
        } else {
            0
        },
        palette_bytes.max(header.palette_len() * channels),
    )
    .map_err(|e| e.to_string())?;
    {
        let mut raw_palette =
            ScratchBytes::new(pool, palette_bytes, palette_bytes).map_err(|e| e.to_string())?;
        input
            .read_exact(&mut raw_palette)
            .map_err(|_| "Truncated TGA palette")?;
        check()?;
        if header.mapped() {
            TgaDecoder::new(
                Cursor::new(header.plain(header.palette_len() as u16, true))
                    .chain(Cursor::new(&*raw_palette)),
            )
            .map_err(|e| e.to_string())?
            .read_image(&mut palette)
            .map_err(|e| e.to_string())?;
        }
    }
    let width = header.width as usize;
    let mut raw = ScratchBytes::new(
        pool,
        width * header.raw_stride(),
        width * header.raw_stride(),
    )
    .map_err(|e| e.to_string())?;
    let mut expanded =
        ScratchBytes::new(pool, width * channels, width * channels).map_err(|e| e.to_string())?;
    let mut row = ScratchBytes::new(pool, width * 8, width * 8).map_err(|e| e.to_string())?;
    let mut packets = Packets {
        remaining: u64::from(header.width) * u64::from(header.height),
        left: 0,
        repeat: false,
        pixel: [0; 4],
    };
    let origin = usize::from(u16::from_le_bytes([header.bytes[3], header.bytes[4]]));
    for y in 0..header.height {
        check()?;
        packets.row(input, &mut raw, header.raw_stride(), header.bytes[2] >= 9)?;
        if header.mapped() {
            for (index, pixel) in raw
                .chunks_exact(header.raw_stride())
                .zip(expanded.chunks_exact_mut(channels))
            {
                let index = if index.len() == 1 {
                    usize::from(index[0])
                } else {
                    usize::from(u16::from_le_bytes([index[0], index[1]]))
                };
                let index = index
                    .checked_sub(origin)
                    .filter(|i| *i < header.palette_len())
                    .ok_or("Invalid TGA color map index")?;
                pixel.copy_from_slice(&palette[index * channels..(index + 1) * channels]);
            }
        } else {
            TgaDecoder::new(
                Cursor::new(header.plain(header.width as u16, false)).chain(Cursor::new(&*raw)),
            )
            .map_err(|e| e.to_string())?
            .read_image(&mut expanded)
            .map_err(|e| e.to_string())?;
        }
        for (x, pixel) in expanded.chunks_exact(channels).enumerate() {
            let rgba = match channels {
                1 => [pixel[0], pixel[0], pixel[0], 255],
                2 => [pixel[0], pixel[0], pixel[0], pixel[1]],
                3 => [pixel[0], pixel[1], pixel[2], 255],
                4 => [pixel[0], pixel[1], pixel[2], pixel[3]],
                _ => return Err("Unsupported TGA channels".into()),
            };
            let x = if header.bytes[17] & 0x10 != 0 {
                width - 1 - x
            } else {
                x
            };
            for (out, value) in row[x * 8..(x + 1) * 8].chunks_exact_mut(2).zip(rgba) {
                out.copy_from_slice(&(u16::from(value) * 257).to_be_bytes());
            }
        }
        emit(y, &row)?;
    }
    check()
}

#[cfg(test)]
pub(crate) mod tests;
