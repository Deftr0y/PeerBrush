//! Charged static PNG preparation. Every delivered row is provisional until
//! complete chunk/checksum/color validation returns a completion owner.
use crate::{
    color_profile::prepared::{ChargedWords, UncachedTransforms},
    large_document::{PinnedBytes, Reservation, ResidentBudget, ScratchBytes},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::metadata::Orientation;
use ring::digest::{digest, SHA256};
use std::{
    cell::RefCell,
    io::{Cursor, Read, Seek, SeekFrom},
};

const DECODER_LIMIT: usize = 8 * 1024 * 1024;
// Includes decoder-limit allocations, raw-chunk reallocation peaks, inflater,
// transform/unfilter buffers and row/interlace bookkeeping for png0.18.1.
const DECODER_BYTES: usize = 32 * 1024 * 1024;
const PATTERN_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum ColorPolicy {
    Raw,
    /// Existing selection imports ignore ICC; do not decompress unused iCCP.
    /// Exact encoded-source hash and chunk CRC still bind those original bytes.
    RawSelection,
    ToSrgb,
}
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub edge: u32,
    pub pixels: u64,
    pub encoded: usize,
}
impl Limits {
    pub(crate) const PATTERN: Self = Self {
        edge: 512,
        pixels: 512 * 512,
        encoded: PATTERN_BYTES,
    };
    pub(crate) const SELECTION: Self = Self {
        edge: 8192,
        pixels: 16_000_000,
        encoded: 64 * 1024 * 1024,
    };
    fn validate(self) -> Result<(), String> {
        if self.edge == 0
            || self.edge > 16000
            || self.pixels == 0
            || self.pixels > 256_000_000
            || self.encoded == 0
            || self.encoded > 128 * 1024 * 1024
        {
            return Err("Invalid prepared PNG admission limits".into());
        }
        Ok(())
    }
}
pub(crate) struct EncodedPng {
    bytes: PinnedBytes,
    length: usize,
    source_sha256: [u8; 32],
    base64_sha256: Option<[u8; 32]>,
}
impl EncodedPng {
    /// The file reader owns byte/identity/stamp checks outside engine locks.
    /// This constructor accepts only its immutable already-charged result and
    /// does not grant path, lease or document authority.
    pub(crate) fn from_charged_bytes(
        bytes: PinnedBytes,
        length: usize,
        limits: Limits,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        limits.validate()?;
        if !bytes.uses_budget(pool)
            || length == 0
            || length > bytes.len()
            || length > limits.encoded
        {
            return Err("Prepared PNG byte owner, length or admission differs".into());
        }
        let source_sha256 = sha(&bytes[..length]);
        check()?;
        Ok(Self {
            bytes,
            length,
            source_sha256,
            base64_sha256: None,
        })
    }
    pub(crate) fn base64(
        source: &str,
        limits: Limits,
        pool: &ResidentBudget,
        check: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<Self, String> {
        check()?;
        limits.validate()?;
        if source.is_empty() || source.len() > limits.encoded.div_ceil(3) * 4 {
            return Err("Prepared PNG exceeds its encoded limit".into());
        }
        if source.len() % 4 != 0 {
            return Err("Prepared PNG is not canonical base64".into());
        }
        let padding = if source.ends_with("==") {
            2
        } else if source.ends_with('=') {
            1
        } else {
            0
        };
        let capacity = source.len() / 4 * 3 - padding;
        if capacity > limits.encoded {
            return Err("Prepared PNG exceeds its decoded transport limit".into());
        }
        let mut bytes = ScratchBytes::new(pool, capacity, capacity).map_err(|e| e.to_string())?;
        let length = STANDARD
            .decode_slice(source, &mut bytes)
            .map_err(|_| "Prepared PNG is not valid base64")?;
        if length > limits.encoded {
            return Err("Prepared PNG exceeds its decoded transport limit".into());
        }
        check()?;
        Ok(Self {
            source_sha256: sha(&bytes[..length]),
            base64_sha256: Some(sha(source.as_bytes())),
            bytes: bytes.freeze(),
            length,
        })
    }
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes[..self.length]
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Header {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    pub has_alpha: bool,
    pub grayscale: bool,
    pub cicp: Option<[u8; 4]>,
    pub orientation: Orientation,
    /// RawSelection uses the same default IDAT image as ImageReader, not an
    /// animation compositor. Patterns remain static-only.
    pub animated: bool,
}
pub(crate) struct Complete {
    header: Header,
    source_sha256: [u8; 32],
    base64_sha256: Option<[u8; 32]>,
    profile: Option<PinnedBytes>,
}
impl Complete {
    pub(crate) fn header(&self) -> Header {
        self.header
    }
    pub(crate) fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }
    pub(crate) fn base64_sha256(&self) -> Option<[u8; 32]> {
        self.base64_sha256
    }
    pub(crate) fn profile(&self) -> Option<&[u8]> {
        self.profile.as_deref()
    }
}

/// The input factory runs cancellation checks even inside decoder reads. The
/// caller cannot inject an uncharged encoded buffer through this interface.
struct Checked<'a, 'b, 'c> {
    input: Cursor<&'a [u8]>,
    check: &'b RefCell<&'c mut dyn FnMut() -> Result<(), String>>,
}
impl Read for Checked<'_, '_, '_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        (self.check.borrow_mut())().map_err(std::io::Error::other)?;
        let length = bytes.len().min(64 * 1024);
        self.input.read(&mut bytes[..length])
    }
}
impl Seek for Checked<'_, '_, '_> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        (self.check.borrow_mut())().map_err(std::io::Error::other)?;
        self.input.seek(to)
    }
}
impl std::io::BufRead for Checked<'_, '_, '_> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        (self.check.borrow_mut())().map_err(std::io::Error::other)?;
        self.input.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.input.consume(n)
    }
}

pub(crate) fn decode_rows(
    encoded: &EncodedPng,
    limits: Limits,
    color: ColorPolicy,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut row: impl FnMut(Header, u32, &[u8]) -> Result<(), String>,
) -> Result<Complete, String> {
    check()?;
    limits.validate()?;
    if !encoded.bytes.uses_budget(pool) || encoded.length > limits.encoded {
        return Err("Prepared PNG input pool or transport admission differs".into());
    }
    let (width, height) = container(encoded.bytes(), limits, color, check)?;
    let decoder_charge = pool
        .reserve_bytes(DECODER_BYTES, DECODER_BYTES)
        .map_err(|e| e.to_string())?;
    // The decoder and every allocation it can retain drop before this charge.
    let cancellation = RefCell::new(check);
    let mut checked = Checked {
        input: Cursor::new(encoded.bytes()),
        check: &cancellation,
    };
    let mut decoder = png::Decoder::new_with_limits(
        &mut checked,
        png::Limits {
            bytes: DECODER_LIMIT,
        },
    );
    decoder.ignore_checksums(false);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(matches!(color, ColorPolicy::RawSelection));
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    if (reader.info().width, reader.info().height) != (width, height)
        || (!matches!(color, ColorPolicy::RawSelection)
            && (reader.info().animation_control.is_some() || reader.info().frame_control.is_some()))
    {
        return Err("Prepared PNG header differs from its static container".into());
    }
    let orientation = reader
        .info()
        .exif_metadata
        .as_deref()
        .and_then(Orientation::from_exif_chunk)
        .unwrap_or(Orientation::NoTransforms);
    let has_alpha = matches!(
        reader.info().color_type,
        png::ColorType::Rgba | png::ColorType::GrayscaleAlpha
    ) || reader.info().trns.is_some();
    let profile = reader
        .info()
        .icc_profile
        .as_deref()
        .map(|profile| -> Result<PinnedBytes, String> {
            if profile.len() > crate::color_profile::MAX_PROFILE_BYTES {
                return Err("Prepared PNG ICC exceeds4MiB".into());
            }
            let mut bytes =
                ScratchBytes::new(pool, profile.len(), profile.len()).map_err(|e| e.to_string())?;
            bytes.copy_from_slice(profile);
            Ok(bytes.freeze())
        })
        .transpose()?;
    let conversion = if matches!(color, ColorPolicy::ToSrgb) {
        profile
            .as_deref()
            .map(|p| UncachedTransforms::new(p, pool, &mut || (cancellation.borrow_mut())()))
            .transpose()?
    } else {
        None
    };
    let (output_color, bits) = reader.output_color_type();
    let depth = match bits {
        png::BitDepth::Eight => 8,
        png::BitDepth::Sixteen => 16,
        _ => return Err("Prepared PNG did not expand native channels".into()),
    };
    let channels = match output_color {
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        _ => return Err("Prepared PNG color did not expand".into()),
    };
    let header = Header {
        width,
        height,
        depth,
        has_alpha,
        grayscale: matches!(
            output_color,
            png::ColorType::Grayscale | png::ColorType::GrayscaleAlpha
        ),
        cicp: reader.info().coding_independent_code_points.map(|v| {
            [
                v.color_primaries,
                v.transfer_function,
                v.matrix_coefficients,
                u8::from(v.is_video_full_range_image),
            ]
        }),
        orientation,
        animated: reader.info().animation_control.is_some(),
    };
    let row_length = width as usize * channels * usize::from(depth / 8);
    let mut rgba = ScratchBytes::new(pool, width as usize * 8, width as usize * 8)
        .map_err(|e| e.to_string())?;
    let mut input8 = if depth == 8 {
        Some(
            ScratchBytes::new(pool, width as usize * 4, width as usize * 4)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let mut output8 = if depth == 8 && conversion.is_some() {
        Some(
            ScratchBytes::new(pool, width as usize * 4, width as usize * 4)
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let mut input16 = if depth == 16 {
        Some(ChargedWords::new(width as usize * 4, pool)?)
    } else {
        None
    };
    let mut output16 = if depth == 16 && conversion.is_some() {
        Some(ChargedWords::new(width as usize * 4, pool)?)
    } else {
        None
    };
    let mut emit = |y: u32, data: &[u8]| -> Result<(), String> {
        (cancellation.borrow_mut())()?;
        if data.len() != row_length {
            return Err("Prepared PNG row length differs".into());
        }
        if depth == 8 {
            let input = input8.as_mut().unwrap();
            for (source, target) in data.chunks_exact(channels).zip(input.chunks_exact_mut(4)) {
                target[0] = source[0];
                target[1] = if channels <= 2 { source[0] } else { source[1] };
                target[2] = if channels <= 2 { source[0] } else { source[2] };
                target[3] = match channels {
                    2 => source[1],
                    4 => source[3],
                    _ => 255,
                };
            }
            let values = if let Some(conversion) = &conversion {
                let output = output8.as_mut().unwrap();
                conversion.convert8(input, output)?;
                &**output
            } else {
                &**input
            };
            for (&value, target) in values.iter().zip(rgba.chunks_exact_mut(2)) {
                target.copy_from_slice(&(value as u16 * 257).to_be_bytes());
            }
        } else {
            let input = input16.as_mut().unwrap();
            for (source, target) in data
                .chunks_exact(channels * 2)
                .zip(input.chunks_exact_mut(4))
            {
                let get = |c: usize| u16::from_be_bytes([source[c * 2], source[c * 2 + 1]]);
                target[0] = get(0);
                target[1] = if channels <= 2 { get(0) } else { get(1) };
                target[2] = if channels <= 2 { get(0) } else { get(2) };
                target[3] = match channels {
                    2 => get(1),
                    4 => get(3),
                    _ => 65535,
                };
            }
            let values = if let Some(conversion) = &conversion {
                let output = output16.as_mut().unwrap();
                conversion.convert16(input, output)?;
                &**output
            } else {
                &**input
            };
            for (&value, target) in values.iter().zip(rgba.chunks_exact_mut(2)) {
                target.copy_from_slice(&value.to_be_bytes());
            }
        }
        row(header, y, &rgba)
    };
    if reader.info().interlaced {
        let mut decoded = BandedFrame::new(row_length, height, pool)?;
        let pixel_bytes = channels * usize::from(depth / 8);
        for (pass, (sx, sy, dx, dy)) in [
            (0u32, 0u32, 8u32, 8u32),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
        .into_iter()
        .enumerate()
        {
            if sx >= width || sy >= height {
                continue;
            }
            let samples = (width - sx).div_ceil(dx);
            for line in 0..(height - sy).div_ceil(dy) {
                (cancellation.borrow_mut())()?;
                let part = reader
                    .next_interlaced_row()
                    .map_err(|e| e.to_string())?
                    .ok_or("Prepared PNG ended before all Adam7 rows")?;
                let expected = png::Adam7Info::new((pass + 1) as u8, line, width);
                if !matches!(part.interlace(),png::InterlaceInfo::Adam7(actual) if *actual==expected)
                    || part.data().len() != samples as usize * pixel_bytes
                {
                    return Err("Prepared PNG Adam7 row differs from its declared pass".into());
                }
                let target = decoded.row_mut(sy + line * dy)?;
                for (sample, value) in part.data().chunks_exact(pixel_bytes).enumerate() {
                    let at = (sx + sample as u32 * dx) as usize * pixel_bytes;
                    target[at..at + pixel_bytes].copy_from_slice(value);
                }
            }
        }
        if reader
            .next_interlaced_row()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("Prepared PNG has excess Adam7 rows".into());
        }
        for y in 0..height {
            emit(y, decoded.row_mut(y)?)?;
        }
    } else {
        for y in 0..height {
            let data = reader
                .next_row()
                .map_err(|e| e.to_string())?
                .ok_or("Prepared PNG ended before all rows")?;
            emit(y, data.data())?;
        }
        if reader.next_row().map_err(|e| e.to_string())?.is_some() {
            return Err("Prepared PNG has excess rows".into());
        }
    }
    reader.finish().map_err(|e| e.to_string())?;
    if reader.info().icc_profile.as_deref() != profile.as_deref()
        || reader.info().coding_independent_code_points.map(|v| {
            [
                v.color_primaries,
                v.transfer_function,
                v.matrix_coefficients,
                u8::from(v.is_video_full_range_image),
            ]
        }) != header.cicp
        || reader
            .info()
            .exif_metadata
            .as_deref()
            .and_then(Orientation::from_exif_chunk)
            .unwrap_or(Orientation::NoTransforms)
            != orientation
    {
        return Err("Prepared PNG color/orientation metadata changed after image rows".into());
    }
    drop(reader);
    drop(checked);
    drop(decoder_charge);
    (cancellation.borrow_mut())()?;
    Ok(Complete {
        header,
        source_sha256: encoded.source_sha256,
        base64_sha256: encoded.base64_sha256,
        profile,
    })
}

/// Interlace reconstruction is segmented into1MiB admitted allocations. This
/// preserves the64MiB single-buffer guard and charges every band/index until
/// the final source row is consumed; it is not an out-of-core frame guarantee.
struct BandedFrame {
    bands: Vec<ScratchBytes>,
    row_bytes: usize,
    rows_per_band: usize,
    height: u32,
    _index_charge: Reservation,
}
impl BandedFrame {
    fn new(row_bytes: usize, height: u32, pool: &ResidentBudget) -> Result<Self, String> {
        if row_bytes == 0 || row_bytes > 16000 * 8 || height == 0 || height > 16000 {
            return Err("Invalid prepared PNG interlace row shape".into());
        }
        let rows_per_band = (1024 * 1024 / row_bytes).max(1);
        let count = (height as usize).div_ceil(rows_per_band);
        let metadata = count
            .checked_mul(std::mem::size_of::<ScratchBytes>())
            .and_then(|v| v.checked_mul(2))
            .ok_or("PNG band index overflow")?;
        let total = row_bytes
            .checked_mul(height as usize)
            .and_then(|v| v.checked_add(metadata))
            .ok_or("PNG interlace admission overflow")?;
        if total > pool.limit().saturating_sub(pool.used()) {
            return Err("Prepared PNG interlace frame exceeds resident admission".into());
        }
        let index_charge = pool
            .reserve_bytes(metadata, metadata)
            .map_err(|e| e.to_string())?;
        let mut bands = Vec::new();
        bands
            .try_reserve_exact(count)
            .map_err(|_| "PNG band index allocation failed")?;
        if bands.capacity() * std::mem::size_of::<ScratchBytes>() > metadata {
            return Err("PNG band index capacity exceeds admission".into());
        }
        for index in 0..count {
            let rows = rows_per_band.min(height as usize - index * rows_per_band);
            let length = rows * row_bytes;
            bands.push(ScratchBytes::new(pool, length, length).map_err(|e| e.to_string())?);
        }
        Ok(Self {
            bands,
            row_bytes,
            rows_per_band,
            height,
            _index_charge: index_charge,
        })
    }
    fn row_mut(&mut self, y: u32) -> Result<&mut [u8], String> {
        if y >= self.height {
            return Err("PNG interlace row is outside its prepared frame".into());
        }
        let index = y as usize / self.rows_per_band;
        let at = (y as usize % self.rows_per_band) * self.row_bytes;
        Ok(&mut self.bands[index][at..at + self.row_bytes])
    }
}

pub(crate) struct PreparedPng {
    width: u32,
    height: u32,
    depth: u16,
    source_sha256: [u8; 32],
    pixels: PinnedBytes,
}
impl PreparedPng {
    pub(crate) fn width(&self) -> u32 {
        self.width
    }
    pub(crate) fn height(&self) -> u32 {
        self.height
    }
    pub(crate) fn depth(&self) -> u16 {
        self.depth
    }
    pub(crate) fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }
    pub(crate) fn into_pixels(self) -> PinnedBytes {
        self.pixels
    }
}
pub(crate) fn pattern(
    source: &str,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<PreparedPng, String> {
    let encoded = EncodedPng::base64(source, Limits::PATTERN, pool, check)?;
    let mut pixels = None;
    let complete = decode_rows(
        &encoded,
        Limits::PATTERN,
        ColorPolicy::ToSrgb,
        pool,
        check,
        |header, y, row| {
            if pixels.is_none() {
                let length = header.width as usize * header.height as usize * 8;
                pixels = Some(ScratchBytes::new(pool, length, length).map_err(|e| e.to_string())?);
            }
            let swap = matches!(
                header.orientation,
                Orientation::Rotate90
                    | Orientation::Rotate270
                    | Orientation::Rotate90FlipH
                    | Orientation::Rotate270FlipH
            );
            let target_width = if swap { header.height } else { header.width };
            for (x, value) in row.chunks_exact(8).enumerate() {
                let (x, y) = oriented(x as u32, y, header);
                let at = (y as usize * target_width as usize + x as usize) * 8;
                pixels.as_mut().unwrap()[at..at + 8].copy_from_slice(value);
            }
            Ok(())
        },
    )?;
    let swap = matches!(
        complete.header.orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    Ok(PreparedPng {
        width: if swap {
            complete.header.height
        } else {
            complete.header.width
        },
        height: if swap {
            complete.header.width
        } else {
            complete.header.height
        },
        depth: complete.header.depth,
        source_sha256: complete
            .base64_sha256
            .ok_or("Prepared pattern lacks its base64 source binding")?,
        pixels: pixels.ok_or("Prepared PNG has no rows")?.freeze(),
    })
}
fn oriented(x: u32, y: u32, h: Header) -> (u32, u32) {
    match h.orientation {
        Orientation::NoTransforms => (x, y),
        Orientation::Rotate90 => (h.height - 1 - y, x),
        Orientation::Rotate180 => (h.width - 1 - x, h.height - 1 - y),
        Orientation::Rotate270 => (y, h.width - 1 - x),
        Orientation::FlipHorizontal => (h.width - 1 - x, y),
        Orientation::FlipVertical => (x, h.height - 1 - y),
        Orientation::Rotate90FlipH => (y, x),
        Orientation::Rotate270FlipH => (h.height - 1 - y, h.width - 1 - x),
    }
}
fn sha(bytes: &[u8]) -> [u8; 32] {
    digest(&SHA256, bytes).as_ref().try_into().unwrap()
}
fn container(
    bytes: &[u8],
    limits: Limits,
    color: ColorPolicy,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(u32, u32), String> {
    if bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err("Prepared source is not a PNG".into());
    }
    let mut at = 8usize;
    let mut shape = None;
    let mut profiles = 0;
    let mut managed = false;
    let mut nonstandard = false;
    while at < bytes.len() {
        check()?;
        let head = bytes
            .get(at..at.checked_add(8).ok_or("PNG offset overflow")?)
            .ok_or("Truncated prepared PNG chunk")?;
        let length = u32::from_be_bytes(head[..4].try_into().unwrap()) as usize;
        let end = at
            .checked_add(length)
            .and_then(|v| v.checked_add(12))
            .ok_or("PNG chunk overflow")?;
        if end > bytes.len() {
            return Err("Truncated prepared PNG payload".into());
        }
        // png0.18.1 skips CRC when a chunk (notably unused ICC/text) is ignored.
        // Validate the immutable original container before any row is staged.
        let expected = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
        let mut crc = !0u32;
        for part in bytes[at + 4..end - 4].chunks(64 * 1024) {
            check()?;
            for &byte in part {
                crc = (crc >> 8) ^ CRC_TABLE[((crc ^ u32::from(byte)) & 255) as usize];
            }
        }
        if !crc != expected {
            return Err("Prepared PNG original chunk checksum differs".into());
        }
        let kind = &head[4..8];
        let data = &bytes[at + 8..end - 4];
        if at == 8 && kind != b"IHDR" {
            return Err("Prepared PNG starts without IHDR".into());
        }
        match kind {
            b"IHDR" => {
                if shape.is_some() || length != 13 {
                    return Err("Invalid prepared PNG header".into());
                }
                let width = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap());
                let height = u32::from_be_bytes(bytes[at + 12..at + 16].try_into().unwrap());
                if width == 0
                    || height == 0
                    || width > limits.edge
                    || height > limits.edge
                    || u64::from(width) * u64::from(height) > limits.pixels
                {
                    return Err("Prepared PNG dimensions exceed admission".into());
                }
                shape = Some((width, height));
            }
            b"acTL" | b"fcTL" | b"fdAT" if !matches!(color, ColorPolicy::RawSelection) => {
                return Err(
                    "Prepared PNG requires one static image; choose an animation frame explicitly"
                        .into(),
                )
            }
            b"iCCP" => {
                managed = true;
                profiles += 1;
                if profiles > 1 {
                    return Err("Prepared PNG has ambiguous ICC profiles".into());
                }
            }
            b"sRGB" => managed = true,
            b"cICP" if matches!(color, ColorPolicy::ToSrgb) && data != [1, 13, 0, 1] => {
                return Err("Prepared PNG HDR/non-sRGB CICP requires explicit conversion".into())
            }
            b"gAMA" => {
                if length != 4 {
                    return Err("Invalid prepared PNG gamma chunk".into());
                }
                nonstandard |= u32::from_be_bytes(data.try_into().unwrap()) != 45455;
            }
            b"cHRM" => {
                if length != 32 {
                    return Err("Invalid prepared PNG chromaticity chunk".into());
                }
                nonstandard |= data
                    .chunks_exact(4)
                    .map(|p| u32::from_be_bytes(p.try_into().unwrap()))
                    .ne([31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000]);
            }
            b"IEND" => {
                if length != 0 || end != bytes.len() {
                    return Err("Prepared PNG end or trailing data differs".into());
                }
                if matches!(color, ColorPolicy::ToSrgb) && nonstandard && !managed {
                    return Err(
                        "Prepared PNG non-sRGB gamma/chromaticities require explicit conversion"
                            .into(),
                    );
                }
                return shape.ok_or("Prepared PNG has no shape".into());
            }
            _ => {}
        }
        at = end;
    }
    Err("Prepared PNG has no complete end chunk".into())
}

const CRC_TABLE: [u32; 256] = crc_table();
const fn crc_table() -> [u32; 256] {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut bit = 0;
        while bit < 8 {
            crc = (crc >> 1) ^ ((0u32.wrapping_sub(crc & 1)) & 0xedb88320);
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{borrow::Cow, io::Write};
    fn fixture(
        color: png::ColorType,
        bits: png::BitDepth,
        profile: Option<Vec<u8>>,
        orientation: u8,
    ) -> Vec<u8> {
        let mut info = png::Info::with_size(3, 2);
        info.color_type = color;
        info.bit_depth = bits;
        info.icc_profile = profile.map(Cow::Owned);
        if color == png::ColorType::Indexed {
            info.palette = Some(Cow::Owned(vec![
                17, 83, 219, 241, 59, 107, 71, 137, 211, 0, 0, 0,
            ]));
            info.trns = Some(Cow::Owned(vec![127, 0, 255, 47]));
        }
        if orientation != 1 {
            let mut exif = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
            info.exif_metadata = Some(Cow::Owned(exif));
        }
        let channels = match color {
            png::ColorType::Grayscale | png::ColorType::Indexed => 1,
            png::ColorType::GrayscaleAlpha => 2,
            png::ColorType::Rgb => 3,
            png::ColorType::Rgba => 4,
        };
        let mut pixels = Vec::new();
        if bits == png::BitDepth::Two {
            pixels.extend_from_slice(&[0b00011000, 0b11100100]);
        } else {
            for pixel in 0..6 {
                for c in 0..channels {
                    if bits == png::BitDepth::Sixteen {
                        pixels.extend_from_slice(
                            &(12347u16.wrapping_add(pixel * 139 + c * 337)).to_be_bytes(),
                        );
                    } else {
                        pixels.push(if color == png::ColorType::Indexed {
                            pixel as u8 % 4
                        } else {
                            (17 + pixel * 37 + c * 61) as u8
                        });
                    }
                }
            }
        }
        let mut bytes = Vec::new();
        {
            let mut writer = png::Encoder::with_info(&mut bytes, info)
                .unwrap()
                .write_header()
                .unwrap();
            writer.write_image_data(&pixels).unwrap();
            writer.finish().unwrap();
        }
        bytes
    }
    fn old(bytes: Vec<u8>) -> crate::raster::Raster {
        crate::image_import::Encoded::png(bytes)
            .unwrap()
            .decode(&serde_json::json!({}), 4 * 1024 * 1024)
            .unwrap()
    }
    fn compare_pattern(bytes: Vec<u8>) {
        let expected = old(bytes.clone());
        let source = STANDARD.encode(bytes);
        let pool = ResidentBudget::new(224 * 1024 * 1024).unwrap();
        let bitmap = pattern(&source, &pool, &mut || Ok(())).unwrap();
        assert_eq!(
            (bitmap.width, bitmap.height, bitmap.depth),
            (expected.width, expected.height, expected.depth)
        );
        assert_eq!(bitmap.source_sha256, sha(source.as_bytes()));
        let pixels = bitmap.into_pixels();
        let expected = expected.rgba16();
        for (actual, expected) in pixels.chunks_exact(2).zip(expected) {
            assert_eq!(u16::from_be_bytes(actual.try_into().unwrap()), expected);
        }
        assert_eq!(pool.used(), pixels.len());
        drop(pixels);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn prepared_static_png_preserves_all_native_layouts_and_exif_patterns() {
        for color in [
            png::ColorType::Grayscale,
            png::ColorType::GrayscaleAlpha,
            png::ColorType::Rgb,
            png::ColorType::Rgba,
        ] {
            for bits in [png::BitDepth::Eight, png::BitDepth::Sixteen] {
                compare_pattern(fixture(color, bits, None, 1));
            }
        }
        compare_pattern(fixture(
            png::ColorType::Indexed,
            png::BitDepth::Two,
            None,
            1,
        ));
        for orientation in 1..=8 {
            compare_pattern(fixture(
                png::ColorType::Rgba,
                png::BitDepth::Sixteen,
                None,
                orientation,
            ));
        }
    }
    #[test]
    fn prepared_icc_matches_existing_math_and_raw_selection_does_not_convert() {
        for bits in [png::BitDepth::Eight, png::BitDepth::Sixteen] {
            let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
            let bytes = fixture(png::ColorType::Rgba, bits, Some(profile.clone()), 1);
            compare_pattern(bytes.clone());
            let expected = image::load_from_memory(&bytes).unwrap().to_rgba16();
            let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
            let encoded = EncodedPng::base64(
                &STANDARD.encode(bytes),
                Limits::SELECTION,
                &pool,
                &mut || Ok(()),
            )
            .unwrap();
            let mut words = Vec::new();
            let complete = decode_rows(
                &encoded,
                Limits::SELECTION,
                ColorPolicy::Raw,
                &pool,
                &mut || Ok(()),
                |h, _, row| {
                    assert!(h.has_alpha);
                    words.extend(
                        row.chunks_exact(2)
                            .map(|v| u16::from_be_bytes(v.try_into().unwrap())),
                    );
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(words, expected.as_raw().as_slice());
            assert_eq!(complete.profile(), Some(profile.as_slice()));
            drop(complete);
            drop(encoded);
            assert_eq!(pool.used(), 0);
        }
        compare_pattern(fixture(
            png::ColorType::Rgba,
            png::BitDepth::Sixteen,
            Some(crate::color_profile::srgb_profile().to_vec()),
            1,
        ));
    }
    fn chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let start = bytes.len();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        let mut crc = !0u32;
        for &byte in &bytes[start..] {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ ((0u32.wrapping_sub(crc & 1)) & 0xedb88320);
            }
        }
        bytes.extend_from_slice(&(!crc).to_be_bytes());
    }
    #[test]
    fn charged_interlace_rows_match_existing_decoder_at_original_precision() {
        let (width, height) = (9u32, 7u32);
        let mut scan = Vec::new();
        for (sx, sy, dx, dy) in [
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ] {
            for y in (sy..height).step_by(dy) {
                if sx >= width {
                    continue;
                }
                scan.push(0);
                for x in (sx..width).step_by(dx) {
                    for value in [
                        12347 + x as u16 * 173,
                        33559 + y as u16 * 97,
                        51237,
                        if (x + y) % 3 == 0 { 0 } else { 45679 },
                    ] {
                        scan.extend_from_slice(&value.to_be_bytes());
                    }
                }
            }
        }
        let mut compressed =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressed.write_all(&scan).unwrap();
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[16, 6, 0, 0, 1]);
        chunk(&mut bytes, b"IHDR", &ihdr);
        chunk(&mut bytes, b"IDAT", &compressed.finish().unwrap());
        chunk(&mut bytes, b"IEND", &[]);
        compare_pattern(bytes);
    }
    #[test]
    fn incomplete_corrupt_animated_cancelled_or_refused_png_never_completes() {
        let bytes = fixture(png::ColorType::Rgba, png::BitDepth::Sixteen, None, 1);
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        for bad in [
            bytes[..bytes.len() - 1].to_vec(),
            {
                let mut b = bytes.clone();
                b.push(0);
                b
            },
            {
                let mut b = bytes.clone();
                b[40] ^= 1;
                b
            },
        ] {
            assert!(pattern(&STANDARD.encode(bad), &pool, &mut || Ok(())).is_err());
            assert_eq!(pool.used(), 0);
        }
        let mut animated = bytes[..33].to_vec();
        chunk(&mut animated, b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]);
        animated.extend_from_slice(&bytes[33..]);
        assert!(pattern(&STANDARD.encode(animated), &pool, &mut || Ok(())).is_err());
        assert_eq!(pool.used(), 0);
        let encoded = EncodedPng::base64(
            &STANDARD.encode(&bytes),
            Limits::PATTERN,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let held = pool.used();
        let mut checks = 0;
        assert!(decode_rows(
            &encoded,
            Limits::PATTERN,
            ColorPolicy::Raw,
            &pool,
            &mut || {
                checks += 1;
                if checks > 4 {
                    Err("Cancelled PNG".into())
                } else {
                    Ok(())
                }
            },
            |_, _, _| Ok(())
        )
        .is_err());
        assert_eq!(pool.used(), held);
        let foreign = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        assert!(decode_rows(
            &encoded,
            Limits::PATTERN,
            ColorPolicy::Raw,
            &foreign,
            &mut || Ok(()),
            |_, _, _| Ok(())
        )
        .is_err());
        assert_eq!(foreign.used(), 0);
        drop(encoded);
        assert_eq!(pool.used(), 0);
        let small = ResidentBudget::new(1024).unwrap();
        assert!(pattern(&STANDARD.encode(bytes), &small, &mut || Ok(())).is_err());
        assert_eq!(small.used(), 0);
    }
    #[test]
    fn final_chunk_checksum_is_checked_before_rows_and_metadata_bombs_are_bounded() {
        let mut bytes = fixture(png::ColorType::Rgba, png::BitDepth::Sixteen, None, 1);
        let last = bytes.len() - 1;
        bytes[last] ^= 1;
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let encoded =
            EncodedPng::base64(&STANDARD.encode(bytes), Limits::PATTERN, &pool, &mut || {
                Ok(())
            })
            .unwrap();
        let held = pool.used();
        let mut rows = 0;
        let result = decode_rows(
            &encoded,
            Limits::PATTERN,
            ColorPolicy::Raw,
            &pool,
            &mut || Ok(()),
            |_, _, _| {
                rows += 1;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(rows, 0);
        assert_eq!(pool.used(), held);
        drop(encoded);
        assert_eq!(pool.used(), 0);
        let inflated = fixture(
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            Some(vec![0; crate::color_profile::MAX_PROFILE_BYTES + 1]),
            1,
        );
        assert!(inflated.len() < 128 * 1024);
        let error = match pattern(&STANDARD.encode(inflated), &pool, &mut || Ok(())) {
            Ok(_) => panic!("Excessive ICC completed"),
            Err(e) => e,
        };
        assert!(error.contains("ICC exceeds4MiB"), "{error}");
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn raw_selection_skips_unused_icc_expansion_but_verifies_its_original_chunk_crc() {
        let bytes = fixture(
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            Some(vec![0; crate::color_profile::MAX_PROFILE_BYTES + 1]),
            1,
        );
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let encoded = EncodedPng::base64(
            &STANDARD.encode(&bytes),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let mut rows = 0;
        let complete = decode_rows(
            &encoded,
            Limits::SELECTION,
            ColorPolicy::RawSelection,
            &pool,
            &mut || Ok(()),
            |h, _, _| {
                assert!(!h.has_alpha);
                rows += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(rows, 2);
        assert!(complete.profile().is_none());
        assert_eq!(complete.source_sha256(), sha(&bytes));
        drop(complete);
        drop(encoded);
        assert_eq!(pool.used(), 0);
        let mut bad = bytes;
        let mut at = 8;
        while &bad[at + 4..at + 8] != b"iCCP" {
            at += u32::from_be_bytes(bad[at..at + 4].try_into().unwrap()) as usize + 12;
        }
        let end = at + u32::from_be_bytes(bad[at..at + 4].try_into().unwrap()) as usize + 12;
        bad[end - 1] ^= 1;
        let encoded =
            EncodedPng::base64(&STANDARD.encode(bad), Limits::SELECTION, &pool, &mut || {
                Ok(())
            })
            .unwrap();
        assert!(decode_rows(
            &encoded,
            Limits::SELECTION,
            ColorPolicy::RawSelection,
            &pool,
            &mut || Ok(()),
            |_, _, _| Ok(())
        )
        .is_err());
        drop(encoded);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn banded_interlace_frame_admits_each_owner_and_checks_complete_shape_before_allocation() {
        let pool = ResidentBudget::new(4 * 1024 * 1024).unwrap();
        let mut frame = BandedFrame::new(1024, 1025, &pool).unwrap();
        assert_eq!(frame.bands.len(), 2);
        assert!(frame.bands.iter().all(|b| b.len() <= 1024 * 1024));
        frame.row_mut(1023).unwrap().fill(17);
        frame.row_mut(1024).unwrap().fill(83);
        assert!(frame.row_mut(1023).unwrap().iter().all(|v| *v == 17));
        assert!(frame.row_mut(1024).unwrap().iter().all(|v| *v == 83));
        assert!(frame.row_mut(1025).is_err());
        drop(frame);
        assert_eq!(pool.used(), 0);
        assert!(BandedFrame::new(16000 * 8, 16000, &pool).is_err());
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn raw_selection_apng_default_image_matches_existing_image_reader() {
        for bits in [png::BitDepth::Eight, png::BitDepth::Sixteen] {
            for separate in [false, true] {
                let mut first = Vec::new();
                for pixel in 0u16..6 {
                    for channel in 0u16..4 {
                        let value = 12347 + pixel * 139 + channel * 337;
                        if bits == png::BitDepth::Sixteen {
                            first.extend_from_slice(&value.to_be_bytes());
                        } else {
                            first.push(crate::raster::project16(value));
                        }
                    }
                }
                let mut bytes = Vec::new();
                {
                    let mut encoder = png::Encoder::new(&mut bytes, 3, 2);
                    encoder.set_color(png::ColorType::Rgba);
                    encoder.set_depth(bits);
                    encoder.set_animated(2, 0).unwrap();
                    encoder.set_sep_def_img(separate).unwrap();
                    let mut writer = encoder.write_header().unwrap();
                    writer.write_image_data(&first).unwrap();
                    for _ in 0..if separate { 2 } else { 1 } {
                        writer.write_image_data(&vec![83; first.len()]).unwrap();
                    }
                    writer.finish().unwrap();
                }
                let expected = image::load_from_memory(&bytes).unwrap().to_rgba16();
                let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
                let encoded = EncodedPng::base64(
                    &STANDARD.encode(&bytes),
                    Limits::SELECTION,
                    &pool,
                    &mut || Ok(()),
                )
                .unwrap();
                let mut actual = Vec::new();
                let complete = decode_rows(
                    &encoded,
                    Limits::SELECTION,
                    ColorPolicy::RawSelection,
                    &pool,
                    &mut || Ok(()),
                    |header, _, row| {
                        assert!(header.animated);
                        actual.extend(
                            row.chunks_exact(2)
                                .map(|v| u16::from_be_bytes(v.try_into().unwrap())),
                        );
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(
                    actual,
                    expected.as_raw().as_slice(),
                    "APNG bits{bits:?} separate{separate}"
                );
                drop(complete);
                drop(encoded);
                assert_eq!(pool.used(), 0);
                assert!(pattern(&STANDARD.encode(bytes), &pool, &mut || Ok(())).is_err());
                assert_eq!(pool.used(), 0);
            }
        }
    }
    #[test]
    fn owned_file_bytes_keep_actual_pool_and_encoded_identity_without_base64_identity() {
        let bytes = fixture(png::ColorType::Rgba, png::BitDepth::Eight, None, 1);
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let mut owner = ScratchBytes::new(&pool, bytes.len(), bytes.len()).unwrap();
        owner.copy_from_slice(&bytes);
        let encoded = EncodedPng::from_charged_bytes(
            owner.freeze(),
            bytes.len(),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let complete = decode_rows(
            &encoded,
            Limits::SELECTION,
            ColorPolicy::RawSelection,
            &pool,
            &mut || Ok(()),
            |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(complete.source_sha256(), sha(&bytes));
        assert_eq!(complete.base64_sha256(), None);
        drop(complete);
        drop(encoded);
        assert_eq!(pool.used(), 0);
        let other = ResidentBudget::new(1024).unwrap();
        let foreign = ScratchBytes::new(&other, 8, 8).unwrap().freeze();
        assert!(
            EncodedPng::from_charged_bytes(foreign, 8, Limits::SELECTION, &pool, &mut || Ok(()))
                .is_err()
        );
        assert_eq!(other.used(), 0);
    }
    #[test]
    fn sink_failure_and_compressed_stream_checksum_return_no_completion() {
        let bytes = fixture(png::ColorType::Rgba, png::BitDepth::Sixteen, None, 1);
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let encoded = EncodedPng::base64(
            &STANDARD.encode(&bytes),
            Limits::PATTERN,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let held = pool.used();
        let mut delivered = 0;
        assert!(decode_rows(
            &encoded,
            Limits::PATTERN,
            ColorPolicy::Raw,
            &pool,
            &mut || Ok(()),
            |_, _, _| {
                delivered += 1;
                if delivered == 2 {
                    Err("Provisional row store refused".into())
                } else {
                    Ok(())
                }
            }
        )
        .is_err());
        assert_eq!(delivered, 2);
        assert_eq!(pool.used(), held);
        drop(encoded);
        assert_eq!(pool.used(), 0);
        let mut at = 8;
        while &bytes[at + 4..at + 8] != b"IDAT" {
            at += u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize + 12;
        }
        let length = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let mut data = bytes[at + 8..at + 8 + length].to_vec();
        let last = data.len() - 1;
        data[last] ^= 1;
        let mut bad = bytes[..at].to_vec();
        chunk(&mut bad, b"IDAT", &data);
        bad.extend_from_slice(&bytes[at + 12 + length..]);
        let encoded = EncodedPng::base64(
            &STANDARD.encode(bad),
            Limits::PATTERN,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        // PNG chunk CRC is correct; the decoder must still reject zlib Adler32.
        assert!(decode_rows(
            &encoded,
            Limits::PATTERN,
            ColorPolicy::Raw,
            &pool,
            &mut || Ok(()),
            |_, _, _| Ok(())
        )
        .is_err());
        drop(encoded);
        assert_eq!(pool.used(), 0);
    }
}
