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
    io::{BufRead, Cursor, Read, Seek, SeekFrom},
};

#[path = "apng.rs"]
pub(crate) mod apng;
#[path = "png_file.rs"]
mod file;
pub(crate) use file::{FileLimits, FilePng};

const DECODER_LIMIT: usize = 8 * 1024 * 1024;
// Includes decoder-limit allocations, raw-chunk reallocation peaks, inflater,
// transform/unfilter buffers and row/interlace bookkeeping for png0.18.1.
const DECODER_BYTES: usize = 32 * 1024 * 1024;
const PATTERN_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ColorPolicy {
    Raw,
    /// Existing selection imports ignore ICC; do not decompress unused iCCP.
    /// Exact encoded-source hash and chunk CRC still bind those original bytes.
    RawSelection,
    /// Validate import color declarations, but keep staged pass samples raw.
    /// Native import preparation must convert complete rows and apply EXIF.
    ImportSource,
    ToSrgb,
}
impl ColorPolicy {
    fn managed(self) -> bool {
        matches!(self, Self::ImportSource | Self::ToSrgb)
    }
}
#[derive(Clone, Copy)]
pub(crate) struct Limits {
    pub edge: u32,
    pub pixels: u64,
    pub encoded: usize,
}
impl Limits {
    pub(crate) const NATIVE_COMMAND: Self = Self {
        edge: crate::raster::NATIVE_EDGE,
        pixels: 256_000_000,
        encoded: 128 * 1024 * 1024,
    };
    pub(crate) const PATTERN: Self = Self {
        edge: 512,
        pixels: 512 * 512,
        encoded: PATTERN_BYTES,
    };
    pub(crate) const SELECTION: Self = Self {
        edge: crate::raster::NATIVE_EDGE,
        pixels: 256_000_000,
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
/// Opaque captured source proof; only admitted transports can construct it.
#[derive(Clone)]
pub(super) struct SourceBinding {
    source: [u8; 32],
    transport: Option<[u8; 32]>,
    pool: ResidentBudget,
}
impl SourceBinding {
    pub(super) fn file(source: [u8; 32], pool: ResidentBudget) -> Self {
        Self {
            source,
            transport: None,
            pool,
        }
    }
    pub(super) fn source_sha256(&self) -> [u8; 32] {
        self.source
    }
    pub(super) fn base64_sha256(&self) -> Option<[u8; 32]> {
        self.transport
    }
    pub(super) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.pool.same_pool(pool)
    }
}
pub(crate) struct EncodedPng {
    bytes: PinnedBytes,
    length: usize,
    source_sha256: [u8; 32],
    base64_sha256: Option<[u8; 32]>,
}
impl EncodedPng {
    pub(super) fn binding(&self, pool: &ResidentBudget) -> Result<SourceBinding, String> {
        if !self.uses_budget(pool) {
            return Err("PNG source uses a foreign resident pool".into());
        }
        Ok(SourceBinding {
            source: self.source_sha256,
            transport: self.base64_sha256,
            pool: pool.clone(),
        })
    }
    pub(crate) fn source_sha256(&self) -> [u8; 32] {
        self.source_sha256
    }
    pub(crate) fn base64_sha256(&self) -> Option<[u8; 32]> {
        self.base64_sha256
    }
    pub(crate) fn uses_budget(&self, pool: &ResidentBudget) -> bool {
        self.bytes.uses_budget(pool)
    }
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
impl Header {
    fn oriented_size(self) -> [u32; 2] {
        if matches!(
            self.orientation,
            Orientation::Rotate90
                | Orientation::Rotate270
                | Orientation::Rotate90FlipH
                | Orientation::Rotate270FlipH
        ) {
            [self.height, self.width]
        } else {
            [self.width, self.height]
        }
    }
}
pub(crate) struct Complete {
    header: Header,
    source_sha256: [u8; 32],
    base64_sha256: Option<[u8; 32]>,
    profile: Option<PinnedBytes>,
    color_policy: ColorPolicy,
}
impl Complete {
    pub(crate) fn color_policy(&self) -> ColorPolicy {
        self.color_policy
    }
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
struct Checked<'b, 'c, R> {
    input: R,
    check: &'b RefCell<&'c mut dyn FnMut() -> Result<(), String>>,
}
impl<R: BufRead + Seek> Read for Checked<'_, '_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        (self.check.borrow_mut())().map_err(std::io::Error::other)?;
        let length = bytes.len().min(64 * 1024);
        self.input.read(&mut bytes[..length])
    }
}
impl<R: BufRead + Seek> Seek for Checked<'_, '_, R> {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        (self.check.borrow_mut())().map_err(std::io::Error::other)?;
        self.input.seek(to)
    }
}
impl<R: BufRead + Seek> BufRead for Checked<'_, '_, R> {
    fn fill_buf(&mut self) -> std::io::Result<&[u8]> {
        (self.check.borrow_mut())().map_err(std::io::Error::other)?;
        self.input.fill_buf()
    }
    fn consume(&mut self, n: usize) {
        self.input.consume(n)
    }
}

/// Coordinates for one decoded native pass row. Pass0 is a complete ordinary
/// row; passes1..7 are sparse Adam7 samples. These are provisional source facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PassRow {
    pub pass: u8,
    pub y: u32,
    pub first_x: u32,
    pub step_x: u32,
    pub samples: u32,
}

/// Existing small-image compatibility adapter. Interlaced rows are explicitly
/// reconstructed in resident native bands; this API does not become out of core.
pub(crate) fn decode_rows(
    encoded: &EncodedPng,
    limits: Limits,
    color: ColorPolicy,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut row: impl FnMut(Header, u32, &[u8]) -> Result<(), String>,
) -> Result<Complete, String> {
    let mut bands = None;
    let complete = decode_native_pass_rows(
        encoded,
        limits,
        color,
        pool,
        check,
        |header, event, data| {
            if event.pass == 0 && !matches!(color, ColorPolicy::ToSrgb) {
                return row(header, event.y, data);
            }
            if bands.is_none() {
                let native_bytes = if header.depth == 16 { 8 } else { 4 };
                bands = Some(BandedFrame::new(
                    header.width as usize * native_bytes,
                    header.height,
                    pool,
                )?);
            }
            let target = bands.as_mut().unwrap().row_mut(event.y)?;
            for (sample, value) in data.chunks_exact(8).enumerate() {
                let x = (event.first_x + sample as u32 * event.step_x) as usize;
                if header.depth == 16 {
                    target[x * 8..x * 8 + 8].copy_from_slice(value);
                } else {
                    for c in 0..4 {
                        if value[c * 2] != value[c * 2 + 1] {
                            return Err(
                                "Prepared native8 Adam7 sample contains16-bit residuals".into()
                            );
                        }
                        target[x * 4 + c] = value[c * 2];
                    }
                }
            }
            Ok(())
        },
    )?;
    if let Some(mut bands) = bands {
        let header = complete.header;
        // Preserve legacy full-row CMS grouping, including SIMD tail arithmetic.
        let conversion = if matches!(color, ColorPolicy::ToSrgb) {
            complete
                .profile()
                .map(|p| UncachedTransforms::new(p, pool, check))
                .transpose()?
        } else {
            None
        };
        let mut rgba = if header.depth == 8 || conversion.is_some() {
            Some(
                ScratchBytes::new(pool, header.width as usize * 8, header.width as usize * 8)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let mut output8 = if header.depth == 8 && conversion.is_some() {
            Some(
                ScratchBytes::new(pool, header.width as usize * 4, header.width as usize * 4)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let mut input16 = if header.depth == 16 && conversion.is_some() {
            Some(ChargedWords::new(header.width as usize * 4, pool)?)
        } else {
            None
        };
        let mut output16 = if header.depth == 16 && conversion.is_some() {
            Some(ChargedWords::new(header.width as usize * 4, pool)?)
        } else {
            None
        };
        for y in 0..header.height {
            check()?;
            let native = bands.row_mut(y)?;
            if header.depth == 8 {
                let values = if let Some(conversion) = &conversion {
                    let output = output8.as_mut().unwrap();
                    conversion.convert8(native, output)?;
                    &**output
                } else {
                    &*native
                };
                let rgba = rgba.as_mut().unwrap();
                for (&value, target) in values.iter().zip(rgba.chunks_exact_mut(2)) {
                    target.copy_from_slice(&(value as u16 * 257).to_be_bytes());
                }
                row(header, y, rgba)?;
            } else if let Some(conversion) = &conversion {
                let input = input16.as_mut().unwrap();
                let output = output16.as_mut().unwrap();
                for (target, value) in input.iter_mut().zip(native.chunks_exact(2)) {
                    *target = u16::from_be_bytes(value.try_into().unwrap());
                }
                conversion.convert16(input, output)?;
                let rgba = rgba.as_mut().unwrap();
                for (&value, target) in output.iter().zip(rgba.chunks_exact_mut(2)) {
                    target.copy_from_slice(&value.to_be_bytes());
                }
                row(header, y, rgba)?;
            } else {
                row(header, y, native)?;
            }
        }
    }
    check()?;
    Ok(complete)
}

/// Worker event source: no full-frame allocation, including for Adam7. A durable
/// sink must preserve previous pass pixels and remain unpublished through the
/// final completion, source checks and its independent guarded engine adoption.
pub(crate) fn decode_pass_rows(
    encoded: &EncodedPng,
    limits: Limits,
    color: ColorPolicy,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut row: impl FnMut(Header, PassRow, &[u8]) -> Result<(), String>,
) -> Result<Complete, String> {
    if matches!(color, ColorPolicy::ToSrgb) {
        return Err(
            "Pass rows preserve native source color; convert completed full rows explicitly".into(),
        );
    }
    decode_native_pass_rows(encoded, limits, color, pool, check, row)
}

fn decode_native_pass_rows(
    encoded: &EncodedPng,
    limits: Limits,
    color: ColorPolicy,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut row: impl FnMut(Header, PassRow, &[u8]) -> Result<(), String>,
) -> Result<Complete, String> {
    check()?;
    limits.validate()?;
    if !encoded.bytes.uses_budget(pool) || encoded.length > limits.encoded {
        return Err("Prepared PNG input pool or transport admission differs".into());
    }
    let facts = container(encoded.bytes(), limits, color, check)?;
    decode_input(
        Cursor::new(encoded.bytes()),
        facts,
        encoded.binding(pool)?,
        color,
        pool,
        check,
        row,
    )
}

fn decode_input(
    input: impl BufRead + Seek,
    (width, height, declared_profile): (u32, u32, bool),
    binding: SourceBinding,
    color: ColorPolicy,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut row: impl FnMut(Header, PassRow, &[u8]) -> Result<(), String>,
) -> Result<Complete, String> {
    check()?;
    if !binding.uses_budget(pool) {
        return Err("PNG decoder input pool differs".into());
    }
    let decoder_charge = pool
        .reserve_bytes(DECODER_BYTES, DECODER_BYTES)
        .map_err(|e| e.to_string())?;
    // The decoder and every allocation it can retain drop before this charge.
    let cancellation = RefCell::new(check);
    let mut checked = Checked {
        input,
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
    // png0.18.1 intentionally swallows iCCP parsing/decompression/limit
    // errors. A declared source profile must never become untagged sRGB.
    if declared_profile
        && !matches!(color, ColorPolicy::RawSelection)
        && reader.info().icc_profile.is_none()
    {
        return Err("Declared PNG ICC profile could not be decoded within admission".into());
    }
    if (reader.info().width, reader.info().height) != (width, height)
        || (!matches!(color, ColorPolicy::RawSelection)
            && (reader.info().animation_control.is_some() || reader.info().frame_control.is_some()))
    {
        return Err("Prepared PNG header differs from its static container".into());
    }
    let complete = decode_frame(
        &mut reader,
        width,
        height,
        binding,
        color,
        pool,
        &mut || (cancellation.borrow_mut())(),
        row,
    )?;
    reader.finish().map_err(|e| e.to_string())?;
    validate_metadata(reader.info(), &complete)?;
    drop(reader);
    drop(checked);
    drop(decoder_charge);
    (cancellation.borrow_mut())()?;
    Ok(complete)
}

// The same provisional row expansion serves static images and selected APNG frames.
fn decode_frame(
    reader: &mut png::Reader<impl BufRead + Seek>,
    width: u32,
    height: u32,
    binding: SourceBinding,
    color: ColorPolicy,
    pool: &ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
    mut row: impl FnMut(Header, PassRow, &[u8]) -> Result<(), String>,
) -> Result<Complete, String> {
    let cancellation = RefCell::new(check);
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
    let pixel_bytes = channels * usize::from(depth / 8);
    let mut rgba = ScratchBytes::new(pool, width as usize * 8, width as usize * 8)
        .map_err(|e| e.to_string())?;
    let mut emit = |event: PassRow, data: &[u8]| -> Result<(), String> {
        (cancellation.borrow_mut())()?;
        if data.len() != event.samples as usize * pixel_bytes {
            return Err("Prepared PNG row length differs".into());
        }
        if depth == 16 && channels == 4 {
            return row(header, event, data);
        }
        for (source, target) in data.chunks_exact(pixel_bytes).zip(rgba.chunks_exact_mut(8)) {
            let get = |c: usize| {
                if depth == 16 {
                    u16::from_be_bytes([source[c * 2], source[c * 2 + 1]])
                } else {
                    u16::from(source[c]) * 257
                }
            };
            let value = [
                get(0),
                if channels <= 2 { get(0) } else { get(1) },
                if channels <= 2 { get(0) } else { get(2) },
                match channels {
                    2 => get(1),
                    4 => get(3),
                    _ => 65535,
                },
            ];
            for (value, target) in value.into_iter().zip(target.chunks_exact_mut(2)) {
                target.copy_from_slice(&value.to_be_bytes());
            }
        }
        row(header, event, &rgba[..event.samples as usize * 8])
    };
    if reader.info().interlaced {
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
                emit(
                    PassRow {
                        pass: (pass + 1) as u8,
                        y: sy + line * dy,
                        first_x: sx,
                        step_x: dx,
                        samples,
                    },
                    part.data(),
                )?;
            }
        }
        if reader
            .next_interlaced_row()
            .map_err(|e| e.to_string())?
            .is_some()
        {
            return Err("Prepared PNG has excess Adam7 rows".into());
        }
    } else {
        for y in 0..height {
            let data = reader
                .next_row()
                .map_err(|e| e.to_string())?
                .ok_or("Prepared PNG ended before all rows")?;
            emit(
                PassRow {
                    pass: 0,
                    y,
                    first_x: 0,
                    step_x: 1,
                    samples: width,
                },
                data.data(),
            )?;
        }
        if reader.next_row().map_err(|e| e.to_string())?.is_some() {
            return Err("Prepared PNG has excess rows".into());
        }
    }
    (cancellation.borrow_mut())()?;
    Ok(Complete {
        header,
        source_sha256: binding.source,
        base64_sha256: binding.transport,
        profile,
        color_policy: color,
    })
}

fn validate_metadata(info: &png::Info<'_>, complete: &Complete) -> Result<(), String> {
    if info.icc_profile.as_deref() != complete.profile()
        || info.coding_independent_code_points.map(|v| {
            [
                v.color_primaries,
                v.transfer_function,
                v.matrix_coefficients,
                u8::from(v.is_video_full_range_image),
            ]
        }) != complete.header.cicp
        || info
            .exif_metadata
            .as_deref()
            .and_then(Orientation::from_exif_chunk)
            .unwrap_or(Orientation::NoTransforms)
            != complete.header.orientation
    {
        return Err("Prepared PNG color/orientation metadata changed after image rows".into());
    }
    Ok(())
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
            let [target_width, _] = header.oriented_size();
            for (x, value) in row.chunks_exact(8).enumerate() {
                let (x, y) = oriented(x as u32, y, header);
                let at = (y as usize * target_width as usize + x as usize) * 8;
                pixels.as_mut().unwrap()[at..at + 8].copy_from_slice(value);
            }
            Ok(())
        },
    )?;
    let [width, height] = complete.header.oriented_size();
    Ok(PreparedPng {
        width,
        height,
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
) -> Result<(u32, u32, bool), String> {
    container_stream(
        &mut Cursor::new(bytes),
        bytes.len() as u64,
        limits.edge,
        limits.pixels,
        color,
        check,
    )
}

/// Reads every original chunk, including ignored text/ICC/APNG chunks, with a
/// fixed prefix and CRC state. No chunk length becomes an allocation request.
fn container_stream(
    input: &mut impl BufRead,
    length: u64,
    edge: u32,
    pixels: u64,
    color: ColorPolicy,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(u32, u32, bool), String> {
    container_stream_animation(input, length, edge, pixels, color, false, check).map(|v| v.0)
}

/// Validate the complete original clipboard container before a bitmap decoder
/// allocates pixels. Clipboard paste cannot silently choose an animation frame.
pub(crate) fn validate_clipboard(bytes: &[u8]) -> Result<bool, String> {
    inspect_clipboard(bytes, &mut || Ok(())).map(|(_, _, profile)| profile)
}

pub(crate) fn clipboard_dimensions(
    bytes: &[u8],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<[u32; 2], String> {
    check()?;
    let facts = inspect_clipboard(bytes, check)?;
    // The clipboard owns the bounded encoded bytes. Reserve only decoder,
    // profile, and one expanded row of working storage here.
    let pool = ResidentBudget::new(
        DECODER_BYTES + crate::color_profile::MAX_PROFILE_BYTES + facts.0 as usize * 8,
    ).map_err(|error| error.to_string())?;
    let binding = SourceBinding { source: sha(bytes), transport: None, pool: pool.clone() };
    let complete = decode_input(
        Cursor::new(bytes), facts, binding, ColorPolicy::ImportSource, &pool,
        check, |_, _, _| Ok(()),
    )?;
    if let Some(profile) = complete.profile() {
        crate::color_profile::supported(profile)?;
    }
    check()?;
    Ok(complete.header.oriented_size())
}

fn inspect_clipboard(
    bytes: &[u8],
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<(u32, u32, bool), String> {
    let limits = Limits::NATIVE_COMMAND;
    if bytes.len() > limits.encoded {
        return Err("Clipboard PNG exceeds 128 MiB".into());
    }
    let (facts, frames) = container_stream_animation(
        &mut Cursor::new(bytes), bytes.len() as u64, limits.edge, limits.pixels,
        ColorPolicy::ImportSource, true, check,
    )?;
    if frames.is_some() {
        return Err("Choose an explicit frame before importing this PNG animation".into());
    }
    Ok(facts)
}

fn container_stream_animation(
    input: &mut impl BufRead,
    length: u64,
    edge: u32,
    pixels: u64,
    color: ColorPolicy,
    allow_animation: bool,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<((u32, u32, bool), Option<u32>), String> {
    let mut animation = apng::Inspection::default();
    check()?;
    let mut signature = [0; 8];
    input
        .read_exact(&mut signature)
        .map_err(|_| "Truncated prepared PNG signature")?;
    if &signature != b"\x89PNG\r\n\x1a\n" {
        return Err("Prepared source is not a PNG".into());
    }
    let mut at = 8u64;
    let mut shape = None;
    let mut profiles = 0u32;
    let mut managed = false;
    let mut nonstandard = false;
    while at < length {
        check()?;
        let mut head = [0; 8];
        input
            .read_exact(&mut head)
            .map_err(|_| "Truncated prepared PNG chunk")?;
        let size = u32::from_be_bytes(head[..4].try_into().unwrap()) as u64;
        let end = at
            .checked_add(size)
            .and_then(|v| v.checked_add(12))
            .ok_or("PNG chunk overflow")?;
        if end > length {
            return Err("Truncated prepared PNG payload".into());
        }
        let kind: &[u8; 4] = head[4..].try_into().unwrap();
        let mut crc = !0u32;
        crc_update(&mut crc, kind);
        let mut prefix = [0; 32];
        let mut consumed = 0u64;
        while consumed < size {
            check()?;
            let part = input.fill_buf().map_err(|e| e.to_string())?;
            let count = part.len().min((size - consumed).min(64 * 1024) as usize);
            if count == 0 {
                return Err("Truncated prepared PNG payload".into());
            }
            crc_update(&mut crc, &part[..count]);
            if consumed < prefix.len() as u64 {
                let copied = count.min(prefix.len() - consumed as usize);
                prefix[consumed as usize..consumed as usize + copied]
                    .copy_from_slice(&part[..copied]);
            }
            input.consume(count);
            consumed += count as u64;
        }
        let mut expected = [0; 4];
        input
            .read_exact(&mut expected)
            .map_err(|_| "Truncated prepared PNG checksum")?;
        if !crc != u32::from_be_bytes(expected) {
            return Err("Prepared PNG original chunk checksum differs".into());
        }
        let data = &prefix[..(size.min(32) as usize)];
        if at == 8 && kind != b"IHDR" {
            return Err("Prepared PNG starts without IHDR".into());
        }
        if allow_animation {
            animation.chunk(kind, data, size)?;
        }
        match kind {
            b"IHDR" => {
                if shape.is_some() || size != 13 {
                    return Err("Invalid prepared PNG header".into());
                }
                let width = u32::from_be_bytes(data[..4].try_into().unwrap());
                let height = u32::from_be_bytes(data[4..8].try_into().unwrap());
                if width == 0
                    || height == 0
                    || width > edge
                    || height > edge
                    || u64::from(width) * u64::from(height) > pixels
                {
                    return Err("Prepared PNG dimensions exceed admission".into());
                }
                shape = Some((width, height));
            }
            b"acTL" | b"fcTL" | b"fdAT"
                if !allow_animation && color != ColorPolicy::RawSelection =>
            {
                return Err(
                    "Prepared PNG requires one static image; choose an animation frame explicitly"
                        .into(),
                );
            }
            b"iCCP" => {
                managed = true;
                profiles += 1;
                if profiles > 1 {
                    return Err("Prepared PNG has ambiguous ICC profiles".into());
                }
            }
            b"sRGB" => managed = true,
            b"cICP" if color.managed() && (size != 4 || data != [1, 13, 0, 1]) => {
                return Err("Prepared PNG HDR/non-sRGB CICP requires explicit conversion".into());
            }
            b"gAMA" => {
                if size != 4 {
                    return Err("Invalid prepared PNG gamma chunk".into());
                }
                nonstandard |= u32::from_be_bytes(data.try_into().unwrap()) != 45455;
            }
            b"cHRM" => {
                if size != 32 {
                    return Err("Invalid prepared PNG chromaticity chunk".into());
                }
                nonstandard |= data
                    .chunks_exact(4)
                    .map(|p| u32::from_be_bytes(p.try_into().unwrap()))
                    .ne([31270, 32900, 64000, 33000, 30000, 60000, 15000, 6000]);
            }
            b"IEND" => {
                if size != 0 || end != length {
                    return Err("Prepared PNG end or trailing data differs".into());
                }
                if color.managed() && nonstandard && !managed {
                    return Err(
                        "Prepared PNG non-sRGB gamma/chromaticities require explicit conversion"
                            .into(),
                    );
                }
                return shape
                    .map(|(w, h)| ((w, h, profiles == 1), animation.frames()))
                    .ok_or("Prepared PNG has no shape".into());
            }
            _ => {}
        }
        at = end;
    }
    Err("Prepared PNG has no complete end chunk".into())
}
fn crc_update(crc: &mut u32, bytes: &[u8]) {
    for &byte in bytes {
        *crc = (*crc >> 8) ^ CRC_TABLE[(((*crc) ^ u32::from(byte)) & 255) as usize];
    }
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
pub(super) mod tests {
    use super::*;
    use std::{borrow::Cow, io::Write};
    pub(in crate::image_import) fn fixture(
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
    pub(in crate::image_import) fn chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
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
    pub(in crate::image_import) fn adam7_fixture(depth: u16) -> Vec<u8> {
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
                        if depth == 16 {
                            scan.extend_from_slice(&value.to_be_bytes());
                        } else {
                            scan.push(crate::raster::project16(value));
                        }
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
        ihdr.extend_from_slice(&[depth as u8, 6, 0, 0, 1]);
        chunk(&mut bytes, b"IHDR", &ihdr);
        chunk(&mut bytes, b"IDAT", &compressed.finish().unwrap());
        chunk(&mut bytes, b"IEND", &[]);
        bytes
    }
    #[test]
    fn charged_interlace_rows_match_existing_decoder_at_original_precision() {
        compare_pattern(adam7_fixture(16));
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
        assert!(clipboard_dimensions(&bad, &mut || Ok(())).is_err());
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
    #[test]
    fn adam7_pass_events_are_native_exact_once_and_keep_only_one_charged_row() {
        for depth in [8, 16] {
            let bytes = adam7_fixture(depth);
            let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
            let encoded = EncodedPng::base64(
                &STANDARD.encode(bytes),
                Limits::SELECTION,
                &pool,
                &mut || Ok(()),
            )
            .unwrap();
            let held = pool.used();
            let mut visited = std::collections::BTreeSet::new();
            let mut pass = 0;
            let complete = decode_pass_rows(
                &encoded,
                Limits::SELECTION,
                ColorPolicy::RawSelection,
                &pool,
                &mut || Ok(()),
                |header, event, row| {
                    assert_eq!(header.depth, depth);
                    assert_eq!((header.width, header.height), (9, 7));
                    assert_eq!(
                        pool.used(),
                        held + DECODER_BYTES + header.width as usize * 8
                    );
                    assert!(event.pass >= pass && event.pass <= 7);
                    pass = event.pass;
                    assert_eq!(row.len(), event.samples as usize * 8);
                    for (sample, value) in row.chunks_exact(8).enumerate() {
                        let x = event.first_x + sample as u32 * event.step_x;
                        let y = event.y;
                        assert!(x < 9 && y < 7);
                        assert!(visited.insert((x, y)));
                        let original = [
                            12347 + x as u16 * 173,
                            33559 + y as u16 * 97,
                            51237,
                            if (x + y) % 3 == 0 { 0 } else { 45679 },
                        ];
                        for (expected, actual) in original.into_iter().zip(value.chunks_exact(2)) {
                            let expected = if depth == 16 {
                                expected
                            } else {
                                u16::from(crate::raster::project16(expected)) * 257
                            };
                            assert_eq!(u16::from_be_bytes(actual.try_into().unwrap()), expected);
                        }
                    }
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(pass, 7);
            assert_eq!(visited.len(), 9 * 7);
            assert_eq!(pool.used(), held);
            drop(complete);
            drop(encoded);
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn pass_source_failure_has_no_completion_and_does_not_normalize_color_implicitly() {
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let encoded = EncodedPng::base64(
            &STANDARD.encode(adam7_fixture(16)),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let held = pool.used();
        let mut delivered = 0;
        assert!(decode_pass_rows(
            &encoded,
            Limits::SELECTION,
            ColorPolicy::RawSelection,
            &pool,
            &mut || Ok(()),
            |_, _, _| {
                delivered += 1;
                if delivered > 1 {
                    Err("Provisional pass sink refused".into())
                } else {
                    Ok(())
                }
            }
        )
        .is_err());
        assert_eq!(delivered, 2);
        assert_eq!(pool.used(), held);
        assert!(decode_pass_rows(
            &encoded,
            Limits::SELECTION,
            ColorPolicy::ToSrgb,
            &pool,
            &mut || Ok(()),
            |_, _, _| Ok(())
        )
        .is_err());
        assert_eq!(pool.used(), held);
        drop(encoded);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn interlace_profile_conversion_keeps_legacy_completed_row_grouping() {
        for depth in [8, 16] {
            let original = adam7_fixture(depth);
            let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
            let mut compressor =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            compressor.write_all(&profile).unwrap();
            let mut tag = b"Profile\0\0".to_vec();
            tag.extend_from_slice(&compressor.finish().unwrap());
            let mut bytes = original[..33].to_vec();
            chunk(&mut bytes, b"iCCP", &tag);
            bytes.extend_from_slice(&original[33..]);
            compare_pattern(bytes);
        }
    }
    #[test]
    fn declared_icc_parse_failures_cannot_become_untagged_srgb_even_with_managed_gamma() {
        let original = fixture(png::ColorType::Rgb, png::BitDepth::Eight, None, 1);
        for profile in [
            b"Profile\0\0not-zlib".as_slice(),
            b"Profile\0\0\x78\x9c\x03".as_slice(),
        ] {
            let mut bytes = original[..33].to_vec();
            chunk(&mut bytes, b"gAMA", &100000u32.to_be_bytes());
            chunk(&mut bytes, b"iCCP", profile);
            bytes.extend_from_slice(&original[33..]);
            let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
            let encoded = EncodedPng::base64(
                &STANDARD.encode(&bytes),
                Limits::SELECTION,
                &pool,
                &mut || Ok(()),
            )
            .unwrap();
            let held = pool.used();
            for policy in [ColorPolicy::Raw, ColorPolicy::ToSrgb] {
                let mut rows = 0;
                let error = match decode_rows(
                    &encoded,
                    Limits::SELECTION,
                    policy,
                    &pool,
                    &mut || Ok(()),
                    |_, _, _| {
                        rows += 1;
                        Ok(())
                    },
                ) {
                    Ok(_) => panic!("Declared invalid ICC was silently dropped"),
                    Err(error) => error,
                };
                assert!(error.contains("Declared PNG ICC"), "{error}");
                assert_eq!(rows, 0);
                assert_eq!(pool.used(), held);
            }
            let complete = decode_rows(
                &encoded,
                Limits::SELECTION,
                ColorPolicy::RawSelection,
                &pool,
                &mut || Ok(()),
                |_, _, _| Ok(()),
            )
            .unwrap();
            assert!(complete.profile().is_none());
            assert_eq!(complete.source_sha256(), sha(&bytes));
            drop(complete);
            drop(encoded);
            assert_eq!(pool.used(), 0);
        }
    }
    #[test]
    fn swallowed_decoder_icc_expansion_limit_still_refuses_the_declared_profile() {
        let bytes = fixture(
            png::ColorType::Rgb,
            png::BitDepth::Eight,
            Some(vec![0; DECODER_LIMIT + 1]),
            1,
        );
        assert!(bytes.len() < 128 * 1024);
        let pool = ResidentBudget::new(64 * 1024 * 1024).unwrap();
        let encoded = EncodedPng::base64(
            &STANDARD.encode(bytes),
            Limits::SELECTION,
            &pool,
            &mut || Ok(()),
        )
        .unwrap();
        let held = pool.used();
        let mut rows = 0;
        let error = match decode_rows(
            &encoded,
            Limits::SELECTION,
            ColorPolicy::ToSrgb,
            &pool,
            &mut || Ok(()),
            |_, _, _| {
                rows += 1;
                Ok(())
            },
        ) {
            Ok(_) => panic!("Declared excessive ICC was silently dropped"),
            Err(error) => error,
        };
        assert!(error.contains("Declared PNG ICC"), "{error}");
        assert_eq!(rows, 0);
        assert_eq!(pool.used(), held);
        drop(encoded);
        assert_eq!(pool.used(), 0);
    }
    #[test]
    fn odd_width_icc_and_exif_match_existing_native_conversion_together() {
        let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        for bits in [png::BitDepth::Eight, png::BitDepth::Sixteen] {
            for color in [
                png::ColorType::Rgb,
                png::ColorType::Rgba,
                png::ColorType::Grayscale,
                png::ColorType::GrayscaleAlpha,
            ] {
                for orientation in 1..=8 {
                    let mut info = png::Info::with_size(9, 7);
                    info.color_type = color;
                    info.bit_depth = bits;
                    info.icc_profile = Some(Cow::Owned(profile.clone()));
                    let mut exif = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
                    exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
                    info.exif_metadata = Some(Cow::Owned(exif));
                    let channels = match color {
                        png::ColorType::Rgb => 3,
                        png::ColorType::Rgba => 4,
                        png::ColorType::Grayscale => 1,
                        _ => 2,
                    };
                    let edges = [
                        0u16, 1, 127, 128, 255, 256, 257, 258, 4095, 12347, 32767, 32768, 51237,
                        65534, 65535,
                    ];
                    let mut data = Vec::new();
                    for pixel in 0..9 * 7 {
                        for channel in 0..channels {
                            let value = edges[(pixel * 3 + channel) % edges.len()];
                            if bits == png::BitDepth::Sixteen {
                                data.extend_from_slice(&value.to_be_bytes());
                            } else {
                                data.push(crate::raster::project16(value));
                            }
                        }
                    }
                    let mut bytes = Vec::new();
                    {
                        let mut writer = png::Encoder::with_info(&mut bytes, info)
                            .unwrap()
                            .write_header()
                            .unwrap();
                        writer.write_image_data(&data).unwrap();
                        writer.finish().unwrap();
                    }
                    compare_pattern(bytes);
                }
            }
        }
    }
}
