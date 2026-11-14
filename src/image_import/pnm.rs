//! Native Netpbm rows reuse the installed decoder's channel and sample rules.
use super::source_file::{FileLimits, FileSource};
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    large_document::ScratchBytes,
    raster::{CanvasLimits, NATIVE_EDGE},
    selection::NativeRows,
    workspace::source_runtime::SourceSession,
};
use image::{
    codecs::pnm::{PnmDecoder, PnmHeader},
    ColorType, ImageDecoder,
};
use std::{
    cell::RefCell,
    fs::File,
    io::{self, BufReader, Cursor, Read},
    path::Path,
};

const HEADER_LIMIT: u64 = 64 * 1024;
// Includes the buffered reader and bounded header parser/string growth peaks.
const DECODER_BYTES: usize = 512 * 1024;
pub(super) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    // A 16000-square binary RGBA16 PAM contains about 2 GB of samples.
    encoded_bytes: 3 * 1024 * 1024 * 1024,
};

pub(crate) fn matches(path: &Path) -> bool {
    extension(path).is_some()
}
fn extension(path: &Path) -> Option<&str> {
    path.extension().and_then(|e| e.to_str()).filter(|e| {
        ["pbm", "pgm", "ppm", "pam", "pnm"]
            .iter()
            .any(|known| e.eq_ignore_ascii_case(known))
    })
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Header {
    width: u32,
    height: u32,
    color: ColorType,
    row: [u8; 256],
    row_len: usize,
}
impl Header {
    fn depth(&self) -> u16 {
        self.color.bits_per_pixel() / u16::from(self.color.channel_count())
    }
    fn read(input: &mut impl Read) -> Result<Self, String> {
        let decoder = PnmDecoder::new(input.take(HEADER_LIMIT))
            .map_err(|e| format!("Netpbm header refused (64 KiB limit): {e}"))?;
        let (width, height) = decoder.dimensions();
        let color = decoder.color_type();
        CanvasLimits::Native.check(width, height)?;
        let (limited, header) = decoder.into_inner();
        if limited.limit() == 0 {
            return Err("Netpbm header reaches the 64 KiB limit".into());
        }
        let row_header: PnmHeader = if let Some(h) = header.as_bitmap() {
            image::codecs::pnm::BitmapHeader { height: 1, ..*h }.into()
        } else if let Some(h) = header.as_graymap() {
            image::codecs::pnm::GraymapHeader { height: 1, ..*h }.into()
        } else if let Some(h) = header.as_pixmap() {
            image::codecs::pnm::PixmapHeader { height: 1, ..*h }.into()
        } else {
            let mut h = header
                .as_arbitrary()
                .ok_or("Unknown Netpbm header")?
                .clone();
            h.height = 1;
            h.into()
        };
        let mut row = [0; 256];
        let mut output = Cursor::new(&mut row[..]);
        row_header
            .write(&mut output)
            .map_err(|_| "Canonical Netpbm row header exceeds admission")?;
        let row_len = output.position() as usize;
        Ok(Self {
            width,
            height,
            color,
            row,
            row_len,
        })
    }
}

pub(super) fn inspect(path: &Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let file = File::open(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > FILE_LIMITS.encoded_bytes {
        return Err("Encoded Netpbm image must be a file at most 3 GiB".into());
    }
    let header = Header::read(&mut BufReader::with_capacity(64 * 1024, file))?;
    Ok(super::Info {
        format: "Pnm".into(),
        width: header.width,
        height: header.height,
        bit_depth: header.depth(),
        choice: None,
        count: 1,
    })
}

struct Controlled<'a, R> {
    input: R,
    check: &'a dyn Fn() -> Result<(), String>,
    failure: &'a RefCell<Option<String>>,
}
impl<R: Read> Read for Controlled<'_, R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let result = (self.check)()
            .map_err(io::Error::other)
            .and_then(|()| self.input.read(out));
        if let Err(error) = &result {
            // The installed ASCII parser can treat an I/O error as a delimiter.
            self.failure
                .borrow_mut()
                .get_or_insert_with(|| error.to_string());
        }
        result
    }
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let extension = extension(path).ok_or("Native Netpbm source requires a Netpbm extension")?;
    let input = FileSource::open(
        session,
        path,
        FILE_LIMITS,
        extension,
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
    let failure = RefCell::new(None::<String>);
    let checked = || {
        if let Some(reason) = failure.borrow().as_ref() {
            return Err(reason.clone());
        }
        check()
    };
    let check: &dyn Fn() -> Result<(), String> = &checked;
    let _decoder = base
        .budget()
        .reserve_bytes(DECODER_BYTES, DECODER_BYTES)
        .map_err(|e| e.to_string())?;
    input.start_read()?;
    let header = {
        let mut stream = BufReader::with_capacity(
            64 * 1024,
            Controlled {
                input: &mut input,
                check,
                failure: &failure,
            },
        );
        let header = Header::read(&mut stream);
        check()?;
        header?
    };
    input.admit(&mut || check())?;
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        input.start_read()?;
        let mut stream = BufReader::with_capacity(
            64 * 1024,
            Controlled {
                input: &mut input,
                check,
                failure: &failure,
            },
        );
        let reread = Header::read(&mut stream);
        check()?;
        if reread? != header {
            return Err("Netpbm header changed after admission".into());
        }
        let mut writer =
            NativeRows::new(header.width, header.height, header.depth(), store.clone())
                .map_err(|e| e.to_string())?;
        let bytes = header.width as usize * usize::from(header.color.bytes_per_pixel());
        let mut raw = ScratchBytes::new(base.budget(), bytes, bytes).map_err(|e| e.to_string())?;
        let bytes = header.width as usize * 8;
        let mut row = ScratchBytes::new(base.budget(), bytes, bytes).map_err(|e| e.to_string())?;
        decode(&mut stream, &header, &mut raw, &mut row, check, |y, row| {
            writer
                .row(y, row, &mut || guard())
                .map_err(|e| e.to_string())
        })?;
        drop(stream);
        input.finish(&mut || check())?;
        check()?;
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })
}
fn decode(
    input: &mut impl Read,
    header: &Header,
    raw: &mut [u8],
    row: &mut [u8],
    check: &dyn Fn() -> Result<(), String>,
    mut emit: impl FnMut(u32, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let stride = usize::from(header.color.bytes_per_pixel());
    if raw.len() != header.width as usize * stride || row.len() != header.width as usize * 8 {
        return Err("Netpbm row differs from admitted channels".into());
    }
    for y in 0..header.height {
        check()?;
        // The decoder has no cross-row state. A canonical one-row header keeps
        // its PBM packing, PAM tuples, endian conversion and MAXVAL rounding.
        let decoder =
            PnmDecoder::new(Cursor::new(&header.row[..header.row_len]).chain(&mut *input))
                .map_err(|e| e.to_string())?;
        let decoded = decoder.read_image(raw);
        check()?;
        decoded.map_err(|e| e.to_string())?;
        for (pixel, out) in raw.chunks_exact(stride).zip(row.chunks_exact_mut(8)) {
            let value = |c: usize| {
                if header.depth() == 16 {
                    u16::from_ne_bytes([pixel[c * 2], pixel[c * 2 + 1]])
                } else {
                    u16::from(pixel[c]) * 257
                }
            };
            let p = match header.color.channel_count() {
                1 => [value(0), value(0), value(0), 65535],
                2 => [value(0), value(0), value(0), value(1)],
                3 => [value(0), value(1), value(2), 65535],
                4 => [value(0), value(1), value(2), value(3)],
                _ => return Err("Unsupported Netpbm channels".into()),
            };
            for (out, v) in out.chunks_exact_mut(2).zip(p) {
                out.copy_from_slice(&v.to_be_bytes());
            }
        }
        emit(y, row)?;
    }
    check()
}

#[cfg(test)]
pub(crate) mod tests;
