//! Admitted WebP decoding over an authenticated, session-owned byte snapshot.
use super::{Info, StaticAlpha};
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    image_import::source_file::{FileLimits, FileSource},
    large_document::{ByteRole, Reservation, ResidentBudget, ScratchBytes},
    raster::{Surface, NATIVE_EDGE, TILE},
    selection::{NativeRows, NativeTileWriter},
    workspace::source_runtime::{SessionBytes, SourceSession},
};
use image::metadata::Orientation;
use image_webp::{
    blend_animation_pixel, compressed_alpha_scratch_bytes, decode_compressed_alpha_residual_rows,
    decode_lossless_rgba_rows, lossless_row_scratch_bytes, vp8::Vp8Decoder,
    AnimationFrameHeader, DecodingError, RawAlphaRows, WebPDecodeOptions, WebPDecoder,
};
use std::{
    cell::RefCell,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};

const MAX_ALLOCATION: usize = 64 * 1024 * 1024;
const TABLE_BYTES: usize = 32 * 1024 * 1024;
const PARTITION_BYTES: usize = 32 * 1024 * 1024;
// Buffered cursor, small decoder tables, color caches, indexing expansion,
// top macroblocks and row borders. Pixel-sized allocations are separate below.
const FIXED_BYTES: usize = 1024 * 1024;
pub(in crate::image_import) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    encoded_bytes: super::NATIVE_FILE_BYTES,
};

fn admit(
    info: &Info, stream_lossy: bool, stream_lossless: bool, pool: &ResidentBudget,
) -> Result<[Option<Reservation>; 9], String> {
    let w = info.width as usize;
    let h = info.height as usize;
    let pixels = w.checked_mul(h).ok_or("WebP pixel count overflow")?;
    if !stream_lossy && !stream_lossless && pixels > MAX_ALLOCATION / 4 {
        return Err("WebP output exceeds native contiguous-buffer admission".into());
    }
    // Each frame fits the canvas. Lossless metadata can contain two transform
    // images (4 bytes/block each), plus entropy RGBA and u16 group indices.
    // VP8 uses padded YUV (384 bytes/MB) and a growing MacroBlock vector;
    // MacroBlock is asserted <=128 bytes in the pinned decoder. Three times
    // the element count covers both live sides of geometric Vec growth.
    let blocks = if info.lossless { w.div_ceil(4) * h.div_ceil(4) } else { 0 };
    let sizes = [
        if info.lossless { TABLE_BYTES } else { 0 },
        if info.lossy { PARTITION_BYTES } else { 0 },
        if info.alpha_chunk && !stream_lossy { pixels * 4 } else { 0 },
        if info.alpha_chunk && !stream_lossy { pixels } else { 0 },
        blocks * 4, // Predictor map.
        blocks * 4, // Color transform map.
        blocks * 4, // Entropy map while it is converted to group indices.
        blocks * 2, // Huffman group indices.
        if stream_lossy {
            (3 * 384 + 3 * 128) * w.div_ceil(16)
        } else if info.lossy {
            (384 + 3 * 128) * w.div_ceil(16) * h.div_ceil(16)
        } else {
            0
        },
    ];
    let mut charges = std::array::from_fn(|_| None);
    for (charge, bytes) in charges.iter_mut().zip(sizes) {
        if bytes != 0 {
            *charge = Some(
                pool.reserve_bytes(bytes, MAX_ALLOCATION)
                    .map_err(|_| "WebP decoder exceeds native memory admission")?,
            );
        }
    }
    Ok(charges)
}

fn metadata(
    input: &mut (impl Read + Seek),
    range: Option<&std::ops::Range<u64>>,
    pool: &ResidentBudget,
    limit: usize,
) -> Result<Option<ScratchBytes>, String> {
    range
        .map(|range| {
            let length = usize::try_from(range.end - range.start)
                .map_err(|_| "WebP metadata size overflow")?;
            let mut bytes = ScratchBytes::new(pool, length, limit).map_err(|e| e.to_string())?;
            input
                .seek(SeekFrom::Start(range.start))
                .map_err(|e| e.to_string())?;
            input.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            Ok(bytes)
        })
        .transpose()
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    command: Option<&serde_json::Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    check()?;
    let file = FileSource::open(
        session,
        path,
        FILE_LIMITS,
        "webp",
        &mut || check(),
    )?;
    prepare_source(base, session, file, command, check)
}

pub(in crate::image_import) fn prepare_source(
    base: &NativeDocument,
    session: &SourceSession,
    mut file: FileSource<'_>,
    command: Option<&serde_json::Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    if file.length() > crate::image_import::ENCODED_LIMIT as u64 {
        file.inspect_seekable(|file, length| {
            super::preflight_large(file, length, command.is_some(), check)
        })?;
    }
    let fixed = base
        .budget()
        .reserve_bytes(FIXED_BYTES, FIXED_BYTES)
        .map_err(|e| e.to_string())?;
    let encoded = file.snapshot(&mut || check())?;
    let failure = RefCell::new(None);
    let result = (|| {
        let check_source = || {
            if failure.borrow().is_some() {
                return Err(crate::large_document::Error::Cancelled);
            }
            if let Err(reason) = check().and_then(|()| encoded.check()) {
                *failure.borrow_mut() = Some(reason);
                return Err(crate::large_document::Error::Cancelled);
            }
            Ok(())
        };
        let mut io_check = || check_source();
        let cursor = encoded
            .bytes()
            .cursor(0, encoded.bytes().payload_length(), &mut io_check)
            .map_err(|e| e.to_string())?;
        let mut input = BufReader::with_capacity(64 * 1024, cursor);
        let info = super::inspect_native(
            &mut input,
            encoded.bytes().payload_length(),
            command.is_some(),
            check,
        )?;
        let selected = command.map_or(Ok(Some(0)), |c| {
            c.get("frame")
                .map(|value| {
                    value
                        .as_u64()
                        .filter(|index| *index < info.frames as u64)
                        .map(|n| n as usize)
                        .ok_or("Image selection index is outside the available items")
                })
                .transpose()
        })?;
        if info.frames > 1 && selected.is_none() {
            return Err(format!(
                "Choose an explicit frame index (0–{}) before importing this image",
                info.frames - 1
            ));
        }
        let selected = selected.unwrap_or(0);
        let stream_lossy = !info.animated && info.lossy && (!info.alpha_chunk || info.static_alpha.is_some());
        let pixels_count = info.width as usize * info.height as usize;
        let direct_rgba = !info.animated && !info.lossy;
        let stream_lossless = direct_rgba && pixels_count > MAX_ALLOCATION / 4;
        let charges = (!info.animated).then(|| admit(&info, stream_lossy, stream_lossless, base.budget())).transpose()?;
        let profile = metadata(
            &mut input,
            info.profile.as_ref(),
            base.budget(),
            crate::color_profile::MAX_PROFILE_BYTES,
        )?;
        if let Some(profile) = &profile {
            crate::color_profile::supported(profile)?;
        }
        let exif = metadata(&mut input, info.exif.as_ref(), base.budget(), 1024 * 1024)?;
        let orientation = exif
            .as_ref()
            .and_then(|bytes| Orientation::from_exif_chunk(bytes))
            .unwrap_or(Orientation::NoTransforms);
        drop(exif);
        input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let mut options = WebPDecodeOptions::default();
        options.lossless_huffman_limit = TABLE_BYTES;
        options.lossy_partition_limit = PARTITION_BYTES;
        let mut decoder =
            WebPDecoder::new_with_options(input, options).map_err(|e| e.to_string())?;
        decoder.set_memory_limit(crate::color_profile::MAX_PROFILE_BYTES);
        if decoder.dimensions() != (info.width, info.height)
            || decoder.is_animated() != info.animated
            || info.animated && decoder.num_frames() as usize != info.frames
            || stream_lossy && decoder.has_alpha() != info.static_alpha.is_some()
        {
            return Err("WebP decoder header differs from source admission".into());
        }
        let output_channels = if decoder.has_alpha() { 4 } else { 3 };
        if decoder.output_buffer_size() != Some(pixels_count * output_channels) {
            return Err("WebP output shape differs".into());
        }
        let channels = if stream_lossy { 3 } else if direct_rgba { 4 } else { output_channels };
        let stride = info.width as usize * channels;
        let source = if info.animated {
            let result = stage_animation(base, session, &encoded, &info, &mut decoder, selected, &|| {
                check().and_then(|()| encoded.check())
            });
            drop((decoder, fixed));
            result?
        } else if stream_lossy {
            let result = stage_lossy_rows(base, session, &encoded, &info, check, |scratch, write| {
                decoder.read_static_lossy_color_rows(scratch, write)
            });
            drop((decoder, charges, fixed));
            result?
        } else if stream_lossless {
            let size = decoder.lossless_row_scratch_bytes().ok_or("WebP lossless scratch size overflow")?;
            let mut scratch = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
            let result = stage_pixels(base, session, &info, 4, check, |write| {
                decoder.read_static_lossless_rgba_rows(&mut scratch, |y, row| {
                    write(y, row, None).map_err(DecodingError::InvalidParameter)
                }).map_err(decoder_error)
            });
            drop((decoder, charges, fixed));
            result?
        } else {
            let mut pixels = ScratchBytes::new(base.budget(), pixels_count * channels, MAX_ALLOCATION)
                .map_err(|e| e.to_string())?;
            if direct_rgba {
                decoder.read_static_lossless_rgba(&mut pixels).map_err(|e| e.to_string())?;
            } else {
                decoder.read_image(&mut pixels).map_err(|e| e.to_string())?;
            }
            drop((decoder, charges, fixed));
            if let Some(reason) = failure.borrow_mut().take() {
                return Err(reason);
            }
            stage_pixels(base, session, &info, channels, check, |write| {
                for (y, row) in pixels.chunks_exact(stride).enumerate() {
                    write(y as u32, row, None)?;
                }
                Ok(())
            })?
        };
        if let Some(reason) = failure.borrow_mut().take() {
            return retire(Err(reason), source, session);
        }
        let output = if profile.is_none() && orientation == Orientation::NoTransforms {
            source
        } else {
            let normalized = crate::image_import::native::normalize(
                base, session, source.surface(), profile.as_deref(), orientation, check,
            );
            retire(normalized, source, session)?
        };
        if let Err(reason) = file
            .recheck()
            .and_then(|()| encoded.check())
            .and_then(|()| check())
        {
            return retire(Err(reason), output, session);
        }
        Ok(output)
    })();
    let result = if let Some(reason) = failure.into_inner() {
        match result {
            Ok(output) => retire(Err(reason), output, session),
            Err(_) => Err(reason),
        }
    } else {
        result
    };
    match (result, encoded.retire(&mut || session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; encoded WebP retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => retire(Err(reason), output, session),
    }
}

fn decoder_error(error: DecodingError) -> String {
    match error {
        DecodingError::InvalidParameter(reason) => reason,
        other => other.to_string(),
    }
}

fn stage_compressed_alpha<'a>(
    session: &'a SourceSession,
    encoded: &SessionBytes<'_>,
    info: &Info,
    alpha: &StaticAlpha,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<SessionBytes<'a>, String> {
    encoded.check_session(session)?;
    let size = compressed_alpha_scratch_bytes(info.width, info.height)
        .ok_or("WebP compressed alpha scratch size overflow")?;
    let pool = session.resident();
    let mut scratch = ScratchBytes::new(&pool, size, size).map_err(|e| e.to_string())?;
    let mut chunk = ScratchBytes::new(&pool, 65536, 65536).map_err(|e| e.to_string())?;
    let checked = || check().and_then(|()| encoded.check());
    let mut io_failure = None;
    let mut io_check = || checked().map_err(|reason| {
        io_failure = Some(reason);
        crate::large_document::Error::Cancelled
    });
    let input = encoded.bytes().cursor(alpha.range.start, alpha.range.end - alpha.range.start, &mut io_check)
        .map_err(|e| e.to_string())?;
    let length = 1 + u64::from(info.width) * u64::from(info.height);
    let mut spool = session.byte_temporary(length, ByteRole::SourceBulk)?;
    chunk[0] = alpha.header & !3;
    let mut used = 1;
    let decoded = decode_compressed_alpha_residual_rows(
        BufReader::with_capacity(65536, input), info.width, info.height, TABLE_BYTES, &mut scratch,
        |_, row| {
            checked().map_err(DecodingError::InvalidParameter)?;
            if used + row.len() > chunk.len() {
                spool.append(&chunk[..used], &mut || checked()).map_err(DecodingError::InvalidParameter)?;
                used = 0;
            }
            chunk[used..used + row.len()].copy_from_slice(row);
            used += row.len();
            Ok(())
        },
    );
    let result = match io_failure {
        Some(reason) => Err(reason),
        None => decoded.map_err(decoder_error).and_then(|()| spool.append(&chunk[..used], &mut || checked())),
    };
    if let Err(reason) = result {
        return Err(match spool.abort(&mut || session.cleanup_check()) {
            Ok(()) => reason,
            Err(cleanup) => format!("{reason}; WebP alpha spool retained: {cleanup}"),
        });
    }
    spool.finish(encoded.bytes().binding(), &mut || checked())
}

fn stage_pixels(
    base: &NativeDocument,
    session: &SourceSession,
    info: &Info,
    channels: usize,
    check: &dyn Fn() -> Result<(), String>,
    decode: impl FnOnce(&mut dyn FnMut(u32, &[u8], Option<&[u8]>) -> Result<(), String>) -> Result<(), String>,
) -> Result<StagedOutput, String> {
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        let mut writer = NativeRows::new(info.width, info.height, 8, store.clone())
            .map_err(|e| e.to_string())?;
        let size = info.width as usize * 8;
        let mut row = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
        decode(&mut |y, input, alpha| {
            guard().map_err(|e| e.to_string())?;
            let width = info.width as usize;
            if input.len() != width * channels || alpha.is_some_and(|samples| samples.len() != width) {
                return Err("WebP decoded row differs from admitted width".into());
            }
            for (x, (pixel, out)) in input.chunks_exact(channels).zip(row.chunks_exact_mut(8)).enumerate() {
                let a = alpha.map_or_else(|| pixel.get(3).copied().unwrap_or(255), |alpha| alpha[x]);
                for (word, value) in out.chunks_exact_mut(2).zip([pixel[0], pixel[1], pixel[2], a]) {
                    word.fill(value);
                }
            }
            writer.row(y, &row, &mut || guard()).map_err(|e| e.to_string())
        })?;
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })
}

fn stage_lossy_rows(
    base: &NativeDocument,
    session: &SourceSession,
    encoded: &SessionBytes<'_>,
    info: &Info,
    check: &dyn Fn() -> Result<(), String>,
    decode: impl FnOnce(&mut [u8], &mut dyn FnMut(u32, &[u8]) -> Result<(), DecodingError>) -> Result<(), DecodingError>,
) -> Result<StagedOutput, String> {
    let size = info.width as usize * 3;
    let mut scratch = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
    let alpha_spool = info.static_alpha.as_ref().filter(|alpha| alpha.compressed())
        .map(|alpha| stage_compressed_alpha(session, encoded, info, alpha, check))
        .transpose()?;
    let mut failure = None;
    let result = (|| {
        let mut alpha_check = || check().and_then(|()| encoded.check()).map_err(|reason| {
            failure = Some(reason);
            crate::large_document::Error::Cancelled
        });
        let mut alpha_scratch;
        let mut alpha = if let Some(alpha) = &info.static_alpha {
            let (bytes, range) = alpha_spool.as_ref().map_or_else(
                || (encoded.bytes(), alpha.range.clone()),
                |spool| (spool.bytes(), 0..spool.bytes().payload_length()),
            );
            let size = info.width as usize * 2;
            alpha_scratch = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
            let cursor = bytes.cursor(range.start, range.end - range.start, &mut alpha_check)
                .map_err(|e| e.to_string())?;
            Some(RawAlphaRows::new(BufReader::with_capacity(65536, cursor),
                info.width, info.height, &mut alpha_scratch).map_err(decoder_error)?)
        } else {
            None
        };
        stage_pixels(base, session, info, 3, check, |write| {
            decode(&mut scratch, &mut |y, row| {
                let alpha = alpha.as_mut().map(RawAlphaRows::read_row).transpose()?;
                write(y, row, alpha).map_err(DecodingError::InvalidParameter)
            }).map_err(decoder_error)
        })
    })();
    let result = match (result, failure) {
        (Ok(output), Some(reason)) => retire(Err(reason), output, session),
        (Err(_), Some(reason)) => Err(reason),
        (result, None) => result,
    };
    let cleanup = alpha_spool.map_or(Ok(()), |spool| spool.retire(&mut || session.cleanup_check()));
    match (result, cleanup) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!("{reason}; WebP alpha spool retained: {cleanup}")),
        (Ok(output), Err(cleanup)) => retire(Err(format!("WebP alpha spool retained: {cleanup}")), output, session),
    }
}

fn stage_animation(
    base: &NativeDocument,
    session: &SourceSession,
    encoded: &SessionBytes<'_>,
    info: &Info,
    decoder: &mut WebPDecoder<impl std::io::BufRead + Seek>,
    selected: usize,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let canvas_pixels = u64::from(info.width) * u64::from(info.height);
    let mut work = crate::transform::native::Work::new((128 * 1024 * 1024).max(4 * canvas_pixels))?;
    let mut previous: Option<StagedOutput> = None;
    let mut clear = None;
    let result = (|| {
        for index in 0..=selected {
            check()?;
            let frame = decoder.next_animation_frame_header().map_err(decoder_error)?;
            let [_, _, width, height] = frame.bounds;
            let frame_has_alpha = !frame.lossy || frame.alpha.is_some();
            let opaque = index == selected && !decoder.has_alpha();
            let replaces_canvas = frame.bounds == [0, 0, info.width, info.height]
                && (!frame_has_alpha || (!frame.blend && !opaque));
            let composition = if replaces_canvas { 0 } else { canvas_pixels };
            work.claim(u64::from(width) * u64::from(height) + composition)
                .map_err(|_| "WebP selected-frame decoding exceeds native work admission")?;
            let raw = stage_animation_frame(base, session, encoded, &frame, check)?;
            let candidate = if replaces_canvas {
                Ok(raw)
            } else {
                let result = composite_animation(
                    base, session, (info.width, info.height), &frame, raw.surface(),
                    previous.as_ref().map(StagedOutput::surface), clear, opaque, check,
                );
                retire(result, raw, session)
            };
            clear = frame.dispose.then_some(frame.bounds);
            previous = Some(if let Some(old) = previous.take() {
                retire(candidate, old, session)?
            } else {
                candidate?
            });
        }
        check()?;
        previous.take().ok_or_else(|| "WebP selected frame has no output".into())
    })();
    match previous {
        Some(old) => retire(result, old, session),
        None => result,
    }
}

fn stage_animation_frame(
    base: &NativeDocument,
    session: &SourceSession,
    encoded: &SessionBytes<'_>,
    frame: &AnimationFrameHeader,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let [_, _, width, height] = frame.bounds;
    let mut failure = None;
    let result = (|| {
        let mut checked = || check().map_err(|reason| {
            failure = Some(reason);
            crate::large_document::Error::Cancelled
        });
        let static_alpha = frame.alpha.as_ref().map(|range| -> Result<_, String> {
            let mut input = encoded.bytes().cursor(range.start, range.end - range.start, &mut checked)
                .map_err(|e| e.to_string())?;
            let mut header = [0];
            input.read_exact(&mut header).map_err(|e| e.to_string())?;
            Ok(StaticAlpha { range: range.clone(), header: header[0] })
        }).transpose()?;
        let info = Info {
            width, height, frames: 1, animated: false, lossy: frame.lossy,
            lossless: !frame.lossy || static_alpha.as_ref().is_some_and(StaticAlpha::compressed),
            alpha_chunk: static_alpha.is_some(), static_alpha, profile: None, exif: None,
        };
        let _charges = admit(&info, info.lossy, !info.lossy, base.budget())?;
        let range = &frame.color;
        let cursor = encoded.bytes().cursor(range.start, range.end - range.start, &mut checked)
            .map_err(|e| e.to_string())?;
        let input = BufReader::with_capacity(65536, cursor);
        if info.lossy {
            stage_lossy_rows(base, session, encoded, &info, check, |scratch, write| {
                Vp8Decoder::decode_rgb_rows(input, (width, height), PARTITION_BYTES, scratch,
                    WebPDecodeOptions::default().lossy_upsampling, write)
            })
        } else {
            let size = lossless_row_scratch_bytes(width, height).ok_or("WebP frame scratch size overflow")?;
            let mut scratch = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
            stage_pixels(base, session, &info, 4, check, |write| {
                decode_lossless_rgba_rows(input, (width, height), TABLE_BYTES, &mut scratch, |y, row| {
                    write(y, row, None).map_err(DecodingError::InvalidParameter)
                }).map_err(decoder_error)
            })
        }
    })();
    match (result, failure) {
        (Ok(output), Some(reason)) => retire(Err(reason), output, session),
        (Err(_), Some(reason)) => Err(reason),
        (result, None) => result,
    }
}

fn composite_animation(
    base: &NativeDocument,
    session: &SourceSession,
    (width, height): (u32, u32),
    frame: &AnimationFrameHeader,
    source: &Surface,
    previous: Option<&Surface>,
    clear: Option<[u32; 4]>,
    opaque: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        let mut writer = NativeTileWriter::new(width, height, 8, store.clone()).map_err(|e| e.to_string())?;
        let size = (TILE * TILE * 4) as usize;
        let mut tile = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
        let [fx, fy, _, _] = frame.bounds;
        let blend = frame.blend && (!frame.lossy || frame.alpha.is_some());
        for ty in 0..height.div_ceil(TILE) {
            for tx in 0..width.div_ceil(TILE) {
                check()?;
                let old = previous.map(|p| p.prepare_tile((tx, ty), &mut || guard()))
                    .transpose().map_err(|e| e.to_string())?;
                if let Some(bytes) = old.as_ref().and_then(|old| old.bytes()) {
                    tile.copy_from_slice(bytes);
                } else {
                    tile.fill(0);
                }
                drop(old);
                let (left, top) = (tx * TILE, ty * TILE);
                let (right, bottom) = ((left + TILE).min(width), (top + TILE).min(height));
                let offset = |x, y| (((y - top) * TILE + (x - left)) * 4) as usize;
                let clip = |[x, y, w, h]: [u32; 4]| (left.max(x)..right.min(x + w), top.max(y)..bottom.min(y + h));
                if let Some(clear) = clear {
                    let (xs, ys) = clip(clear);
                    if !xs.is_empty() {
                        for y in ys {
                            let start = offset(xs.start, y);
                            tile[start..start + (xs.end - xs.start) as usize * 4].fill(0);
                        }
                    }
                }
                let (xs, ys) = clip(frame.bounds);
                if !xs.is_empty() && !ys.is_empty() {
                    let region = crate::raster::backed::Region::new([
                        (xs.start - fx) as i32, (ys.start - fy) as i32,
                        (xs.end - fx) as i32, (ys.end - fy) as i32,
                    ]).map_err(|e| e.to_string())?;
                    let view = source.prepare_region(region, &mut || guard()).map_err(|e| e.to_string())?;
                    for y in ys {
                        check()?;
                        for x in xs.clone() {
                            let src = view.pixel16((x - fx) as i32, (y - fy) as i32)
                                .map_err(|e| e.to_string())?.map(crate::raster::project16);
                            let start = offset(x, y);
                            let target = &mut tile[start..start + 4];
                            let pixel = if blend {
                                blend_animation_pixel(src, (&*target).try_into().unwrap())
                            } else {
                                src
                            };
                            target.copy_from_slice(&pixel);
                        }
                    }
                }
                if opaque {
                    for y in top..bottom {
                        let start = offset(left, y);
                        for pixel in tile[start..start + (right - left) as usize * 4].chunks_exact_mut(4) {
                            pixel[3] = 255;
                        }
                    }
                }
                writer.tile(tx, ty, &tile, &mut || guard()).map_err(|e| e.to_string())?;
            }
        }
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })
}

fn retire(
    result: Result<StagedOutput, String>,
    input: StagedOutput,
    session: &SourceSession,
) -> Result<StagedOutput, String> {
    match (result, input.abort(session, &|| session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; WebP source retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; WebP candidate retirement refused: {cleanup}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;
    use crate::image_import::webp::tests::{chunk, compressed_alpha_fixture, fixture, lossless, riff};

    #[test]
    fn chooser_counts_held_webp_frames_without_spooling_and_checks_the_digest() {
        use crate::image_import::native::OpenFormat;

        let root = Fixture::new();
        let (runtime, _connection) = root.runtime_with_budgets(16 * 1024 * 1024, 32 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let path = root.path().join("Chooser.webp");
        let bytes = fixture(4, true);
        std::fs::write(&path, &bytes).unwrap();
        let disk = runtime.disk().used().unwrap();
        let check = || {
            assert_eq!(runtime.disk().used().unwrap(), disk);
            Ok(())
        };
        let digest: [u8; 32] = ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref().try_into().unwrap();
        let mut remaining = 0;
        let (choice, binding, _) = OpenFormat::Webp.inspect_layer(&session, &path, &mut remaining, &check).unwrap();
        assert_eq!(choice, Some(("frame", 4)));
        assert_eq!(binding, digest);
        let open = || crate::file_safety::open_native_read(&path).unwrap();
        assert_eq!(OpenFormat::Webp.choices(&session, &path, (open(), digest), &check).unwrap().0, 4);
        let mut wrong = digest;
        wrong[0] ^= 1;
        assert!(OpenFormat::Webp.choices(&session, &path, (open(), wrong), &check).is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }

    #[test]
    fn wide_animation_canvas_uses_tiles_and_matches_buffered_blending_and_disposal() {
        use crate::raster::backed::Region;
        use std::io::Cursor;

        let root = Fixture::new();
        let (runtime, _connection) = root.runtime_with_budgets(96 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 8, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let mut bytes = fixture(4, false);
        let mut reference = WebPDecoder::new(Cursor::new(&bytes)).unwrap();
        let mut expected = vec![0; reference.output_buffer_size().unwrap()];
        for _ in 0..4 {
            reference.read_frame(&mut expected).unwrap();
        }
        drop(reference);
        let (width, height) = (NATIVE_EDGE, 1050u32);
        assert!(u64::from(width) * u64::from(height) * 4 > MAX_ALLOCATION as u64);
        bytes[24..27].copy_from_slice(&(width - 1).to_le_bytes()[..3]);
        bytes[27..30].copy_from_slice(&(height - 1).to_le_bytes()[..3]);
        let path = root.path().join("Wide animation.webp");
        std::fs::write(&path, bytes).unwrap();
        let disk = runtime.disk().used().unwrap();
        let output = prepare(&base, &session, &path, Some(&serde_json::json!({"frame": 3})), &|| Ok(())).unwrap();
        let surface = output.surface();
        assert_eq!((surface.width(), surface.height()), (width, height));
        let view = surface.prepare_region(Region::new([0, 0, 270, 19]).unwrap(), &mut || Ok(())).unwrap();
        for (index, expected) in expected.chunks_exact(4).enumerate() {
            let pixel: [u8; 4] = expected.try_into().unwrap();
            assert_eq!(view.pixel16((index % 270) as i32, (index / 270) as i32).unwrap(), pixel.map(|v| u16::from(v) * 257));
        }
        drop(view);
        let last = Region::new([width as i32 - 1, height as i32 - 1, width as i32, height as i32]).unwrap();
        let far = surface.prepare_region(last, &mut || Ok(())).unwrap();
        assert_eq!(far.pixel16(width as i32 - 1, height as i32 - 1).unwrap(), [0; 4]);
        drop(far);
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }

    #[test]
    fn compressed_alpha_spool_retires_on_cancellation_and_after_native_import() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(96 * 1024 * 1024, 32 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let bytes = compressed_alpha_fixture();
        let info = super::super::inspect_native(&mut std::io::Cursor::new(&bytes), bytes.len() as u64, false, &|| Ok(())).unwrap();
        assert!(info.static_alpha.unwrap().compressed());
        let path = root.path().join("Compressed alpha.webp");
        std::fs::write(&path, &bytes).unwrap();
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let encoded_charge = crate::large_document::LargeSourceTemporary::encoded_bytes(bytes.len() as u64).unwrap();
        let cancel = || {
            if runtime.disk().used().unwrap() > disk + encoded_charge {
                Err("Compressed WebP alpha cancelled".into())
            } else {
                Ok(())
            }
        };
        let reason = prepare(&base, &session, &path, None, &cancel).err().unwrap();
        assert!(reason.contains("Compressed WebP alpha cancelled"), "{reason}");
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(runtime.resident().used(), resident);

        let output = prepare(&base, &session, &path, None, &|| Ok(())).unwrap();
        let surface = output.surface();
        assert_eq!((surface.width(), surface.height(), surface.depth()), (17, 53, 8));
        let expected = image::load_from_memory_with_format(&bytes, image::ImageFormat::WebP).unwrap().to_rgba8();
        for y in [0, 15, 16, 31, 32, 52] {
            let region = crate::raster::backed::Region::new([16, y, 17, y + 1]).unwrap();
            let actual = surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(16, y).unwrap();
            let samples = expected.get_pixel(16, y as u32).0;
            assert_eq!(samples[3], (16 * 3 + y * 7) as u8);
            assert_eq!(actual, samples.map(|value| u16::from(value) * 257));
        }
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(runtime.resident().used(), resident);
        drop((base, session, runtime, connection));
    }

    #[test]
    fn native_static_and_animated_lossless_webp_stage_frames_above_the_contiguous_limit() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(80 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let (width, height) = (NATIVE_EDGE, 1049);
        let pixel = [27, 61, 99, 173];
        assert!(width as usize * height as usize * 4 > MAX_ALLOCATION);
        let path = root.path().join("Large lossless.webp");
        let file = std::fs::File::create(&path).unwrap();
        image_webp::WebPEncoder::new(file).encode(
            &pixel.repeat((width * height) as usize), width, height, image_webp::ColorType::Rgba8,
        ).unwrap();
        let still = std::fs::read(&path).unwrap();
        let mut body = b"WEBP".to_vec();
        let mut vp8x = vec![0x12, 0, 0, 0];
        vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
        vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
        chunk(&mut body, b"VP8X", &vp8x);
        chunk(&mut body, b"ANIM", &[0; 6]);
        let mut frame = Vec::new();
        for value in [0, 0, width - 1, height - 1, 10] {
            frame.extend_from_slice(&value.to_le_bytes()[..3]);
        }
        frame.push(2);
        assert_eq!(&still[12..16], b"VP8L");
        frame.extend_from_slice(&still[12..]);
        chunk(&mut body, b"ANMF", &frame);
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        for bytes in [still, riff(body)] {
            std::fs::write(&path, bytes).unwrap();
            let output = prepare(&base, &session, &path, Some(&serde_json::json!({"frame":0})), &|| Ok(())).unwrap();
            let surface = output.surface();
            assert_eq!((surface.width(), surface.height(), surface.depth()), (width, height, 8));
            let (x, y) = (width as i32 - 1, height as i32 - 1);
            let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
            let sample = surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap();
            assert_eq!(sample, pixel.map(|v| u16::from(v) * 257));
            output.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(runtime.resident().used(), resident);
        }
        drop((base, session, runtime, connection));
    }

    #[test]
    fn streamed_lossy_admission_is_bounded_by_band_width_at_full_native_geometry() {
        let pool = ResidentBudget::new(40 * 1024 * 1024).unwrap();
        let mut info = Info {
            width: NATIVE_EDGE, height: NATIVE_EDGE, frames: 1,
            animated: false, lossy: true, lossless: false, alpha_chunk: false,
            static_alpha: None, profile: None, exif: None,
        };
        for has_alpha in [false, true] {
            info.alpha_chunk = has_alpha;
            info.static_alpha = has_alpha.then_some(StaticAlpha {
                range: 0..(1 + u64::from(NATIVE_EDGE).pow(2)), header: 0,
            });
            assert!(admit(&info, false, false, &pool).is_err());
            let charges = admit(&info, true, false, &pool).unwrap();
            assert_eq!(pool.used(), PARTITION_BYTES + (3 * 384 + 3 * 128) * (NATIVE_EDGE as usize).div_ceil(16));
            drop(charges);
            assert_eq!(pool.used(), 0);
        }
    }

    #[test]
    fn opaque_lossless_import_fits_a_single_full_pixel_buffer() {
        let root = Fixture::new();
        let limit = 45 * 1024 * 1024;
        let (runtime, connection) = root.runtime_with_budgets(limit, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let (width, height) = (NATIVE_EDGE, 113u32);
        assert!(FIXED_BYTES + TABLE_BYTES + (width * height) as usize * 7 > limit);
        let path = root.path().join("Opaque lossless.webp");
        std::fs::write(&path, lossless(width, height, 71, false)).unwrap();
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let output = prepare(&base, &session, &path, Some(&serde_json::json!({})), &|| Ok(())).unwrap();
        let surface = output.surface();
        assert_eq!((surface.width(), surface.height(), surface.depth()), (width, height, 8));
        let (x, y) = (width as i32 - 1, height as i32 - 1);
        let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
        let sample = surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap();
        let n = (width * height - 1) as u8;
        assert_eq!(sample, [n.wrapping_add(71), n.wrapping_mul(31), 71, 255].map(|v| u16::from(v) * 257));
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(runtime.resident().used(), resident);
        drop((base, session, runtime, connection));
    }

    #[test]
    fn snapshots_and_outputs_retire_on_budget_refusal_cancel_decode_failure_and_retry() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = root.path().join("source.webp");
        let good = fixture(4, true);
        std::fs::write(&path, &good).unwrap();
        let command = serde_json::json!({"frame":3});
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let holds: Vec<_> = (0..2)
            .map(|_| {
                runtime
                    .resident()
                    .reserve_bytes(48 * 1024 * 1024, MAX_ALLOCATION)
                    .unwrap()
            })
            .collect();
        assert!(prepare(&base, &session, &path, Some(&command), &|| Ok(())).is_err());
        drop(holds);
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let reason = prepare(&base, &session, &path, Some(&command), &|| {
            if runtime.disk().used().unwrap() > disk {
                Err("WebP cancelled".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(reason.contains("WebP cancelled"), "{reason}");
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);

        let seed = lossless(2, 2, 19, true);
        let mut body = b"WEBP".to_vec();
        chunk(&mut body, b"VP8L", &seed[20..25]); // Header only: admission succeeds, decoding fails.
        std::fs::write(&path, riff(body)).unwrap();
        assert!(prepare(&base, &session, &path, None, &|| Ok(())).is_err());
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);

        let mut oversized = seed.clone();
        let header = (8191u32 | (4095u32 << 14) | (1 << 28)).to_le_bytes();
        oversized[21..25].copy_from_slice(&header);
        std::fs::write(&path, oversized).unwrap();
        let reason = prepare(&base, &session, &path, None, &|| Ok(()))
            .err()
            .unwrap();
        assert!(reason.contains("contiguous-buffer"), "{reason}");
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);

        std::fs::write(&path, &good).unwrap();
        let output = prepare(&base, &session, &path, Some(&command), &|| Ok(())).unwrap();
        assert_eq!(
            (output.surface().width(), output.surface().height()),
            (19, 270)
        );
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop((base, session, connection, runtime));
    }
}
