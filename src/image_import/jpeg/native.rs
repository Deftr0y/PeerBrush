//! Audited zune-jpeg decoding over authenticated seekable source bytes.
use super::Inspection;
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    image_import::source_file::{FileLimits, FileSource},
    large_document::{Reservation, ResidentBudget, ScratchBytes},
    raster::{CanvasLimits, NATIVE_EDGE},
    selection::NativeRows,
    workspace::source_runtime::SourceSession,
};
use image::metadata::Orientation;
use std::{
    cell::RefCell,
    io::{BufReader, Read, Seek, SeekFrom},
    path::Path,
};
use zune_core::{colorspace::ColorSpace, options::DecoderOptions};
use zune_jpeg::errors::DecodeErrors;

const MAX_ALLOCATION: usize = 64 * 1024 * 1024;
// zune-jpeg 0.5.15: bounded APP metadata/Vec growth, peek buffer, Huffman
// tables and component MCU/upsampling rows (including reallocation peaks).
const DECODER_BYTES: usize = 64 * 1024 * 1024;
pub(in crate::image_import) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    encoded_bytes: 3 * 1024 * 1024 * 1024,
};

fn coefficient_admission(
    info: &Inspection,
    pool: &ResidentBudget,
) -> Result<[Option<Reservation>; 4], String> {
    let mut charges = std::array::from_fn(|_| None);
    if !info.progressive && info.first_scan_components == info.components {
        return Ok(charges);
    }
    let (h, v) = if info.components == 1 {
        (1, 1)
    } else {
        (
            info.sampling
                .iter()
                .map(|s| usize::from(s[0]))
                .max()
                .unwrap(),
            info.sampling
                .iter()
                .map(|s| usize::from(s[1]))
                .max()
                .unwrap(),
        )
    };
    let mcus = (info.width as usize).div_ceil(h * 8) * (info.height as usize).div_ceil(v * 8);
    for (charge, sample) in charges
        .iter_mut()
        .zip(&info.sampling)
        .take(info.components as usize)
    {
        let samples = if info.components == 1 {
            1
        } else {
            usize::from(sample[0]) * usize::from(sample[1])
        };
        let bytes = mcus
            .checked_mul(samples)
            .and_then(|n| n.checked_mul(64 * 2))
            .ok_or("JPEG coefficient allocation overflow")?;
        *charge = Some(
            pool.reserve_bytes(bytes, MAX_ALLOCATION)
                .map_err(|_| "JPEG coefficient plane exceeds native memory admission")?,
        );
    }
    Ok(charges)
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    managed: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    check()?;
    if !super::matches(path) {
        return Err("Native JPEG source extension differs".into());
    }
    let file = FileSource::open(
        session,
        path,
        FILE_LIMITS,
        path.extension()
            .and_then(|v| v.to_str())
            .ok_or("Invalid JPEG path")?,
        &mut || check(),
    )?;
    prepare_source(base, session, file, managed, check)
}

pub(in crate::image_import) fn prepare_source(
    base: &NativeDocument,
    session: &SourceSession,
    mut file: FileSource<'_>,
    managed: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    // Also admits the 64 KiB buffered cursor before constructing it.
    let decoder_charge = base
        .budget()
        .reserve_bytes(DECODER_BYTES, DECODER_BYTES)
        .map_err(|e| e.to_string())?;
    let encoded = file.snapshot(&mut || check())?;
    let failure = RefCell::new(None);
    let result = (|| {
        let mut io_check = || {
            if failure.borrow().is_some() {
                return Err(crate::large_document::Error::Cancelled);
            }
            if let Err(reason) = check().and_then(|()| encoded.check()) {
                *failure.borrow_mut() = Some(reason);
                return Err(crate::large_document::Error::Cancelled);
            }
            Ok(())
        };
        let cursor = encoded
            .bytes()
            .cursor(0, encoded.bytes().payload_length(), &mut io_check)
            .map_err(|e| e.to_string())?;
        let mut input = BufReader::with_capacity(64 * 1024, cursor);
        let info = Inspection::read(
            &mut input, encoded.bytes().payload_length(), CanvasLimits::Native, managed, check,
        )?;
        let channels = if info.components == 1 { 1 } else { 3 };
        let length = info.width as usize * info.height as usize * channels;
        let coefficients = coefficient_admission(&info, base.budget())?;
        let profile = if managed && info.count != 0 {
            let mut profile = ScratchBytes::new(
                base.budget(),
                info.profile_bytes,
                crate::color_profile::MAX_PROFILE_BYTES,
            )
            .map_err(|e| e.to_string())?;
            let mut at = 0;
            for part in &info.parts[..info.count] {
                check()?;
                input
                    .seek(SeekFrom::Start(part.offset))
                    .map_err(|e| e.to_string())?;
                input
                    .read_exact(&mut profile[at..at + part.length])
                    .map_err(|e| e.to_string())?;
                at += part.length;
            }
            crate::color_profile::supported(&profile)?;
            Some(profile)
        } else {
            None
        };
        input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        let options = DecoderOptions::default()
            .set_strict_mode(false)
            .set_max_width(NATIVE_EDGE as usize)
            .set_max_height(NATIVE_EDGE as usize)
            .jpeg_set_max_scans(100);
        let mut decoder = zune_jpeg::JpegDecoder::new_with_options(input, options);
        decoder.decode_headers().map_err(|e| e.to_string())?;
        if decoder.dimensions() != Some((info.width as usize, info.height as usize))
            || decoder
                .info()
                .is_none_or(|header| header.components != info.components)
        {
            return Err("JPEG decoder dimensions or channels differ from admission".into());
        }
        let color = match decoder
            .input_colorspace()
            .ok_or("JPEG color space missing")?
        {
            ColorSpace::Luma => ColorSpace::Luma,
            ColorSpace::RGB | ColorSpace::YCbCr => ColorSpace::RGB,
            ColorSpace::CMYK | ColorSpace::YCCK if !managed => ColorSpace::RGB,
            _ => return Err("JPEG color layout requires explicit RGB conversion".into()),
        };
        decoder.set_options(options.jpeg_set_out_colorspace(color));
        let orientation = if managed {
            decoder
                .exif()
                .and_then(|v| Orientation::from_exif_chunk(v))
                .unwrap_or(Orientation::NoTransforms)
        } else {
            Orientation::NoTransforms
        };
        if color.num_components() != channels || decoder.output_buffer_size() != Some(length) {
            return Err("JPEG output shape differs from admission".into());
        }
        let stride = info.width as usize * channels;
        let source = if length > MAX_ALLOCATION {
            let mut scratch = ScratchBytes::new(base.budget(), stride, stride)
                .map_err(|e| e.to_string())?;
            let result = stage_pixels(base, session, &info, channels, check, |write| {
                decoder.decode_rows_into(&mut scratch, |y, row| {
                    write(y, row).map_err(DecodeErrors::Format)
                }).map_err(|error| match error {
                    DecodeErrors::Format(reason) => reason,
                    other => other.to_string(),
                })
            });
            drop((decoder, coefficients, decoder_charge));
            result?
        } else {
            let mut pixels = ScratchBytes::new(base.budget(), length, MAX_ALLOCATION)
                .map_err(|_| "JPEG output exceeds native contiguous-buffer admission")?;
            decoder.decode_into(&mut pixels).map_err(|e| e.to_string())?;
            drop((decoder, coefficients, decoder_charge));
            if let Some(reason) = failure.borrow_mut().take() {
                return Err(reason);
            }
            stage_pixels(base, session, &info, channels, check, |write| {
                for (y, row) in pixels.chunks_exact(stride).enumerate() {
                    write(y, row)?;
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
                base,
                session,
                source.surface(),
                profile.as_ref().map(|p| &**p),
                orientation,
                check,
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
            "{reason}; encoded JPEG retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => retire(Err(reason), output, session),
    }
}

fn stage_pixels(
    base: &NativeDocument,
    session: &SourceSession,
    info: &Inspection,
    channels: usize,
    check: &dyn Fn() -> Result<(), String>,
    decode: impl FnOnce(&mut dyn FnMut(usize, &[u8]) -> Result<(), String>) -> Result<(), String>,
) -> Result<StagedOutput, String> {
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        let mut writer = NativeRows::new(info.width, info.height, 8, store.clone())
            .map_err(|e| e.to_string())?;
        let size = info.width as usize * 8;
        let mut row = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
        decode(&mut |y, input| {
            guard().map_err(|e| e.to_string())?;
            for (source, target) in input.chunks_exact(channels).zip(row.chunks_exact_mut(8)) {
                let values = [
                    source[0],
                    source[if channels == 1 { 0 } else { 1 }],
                    source[if channels == 1 { 0 } else { 2 }],
                    255,
                ];
                for (value, word) in values.into_iter().zip(target.chunks_exact_mut(2)) {
                    word.fill(value);
                }
            }
            writer.row(y as u32, &row, &mut || guard()).map_err(|e| e.to_string())
        })?;
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
            "{reason}; JPEG source retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; JPEG candidate retirement refused: {cleanup}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::{jpeg_tests, tests::Fixture};

    #[test]
    fn jpeg_row_output_matches_the_original_buffer_and_stops_on_sink_error() {
        use zune_core::bytestream::ZCursor;
        for bytes in [
            jpeg_tests::fixture(false),
            jpeg_tests::fixture(true),
            jpeg_tests::scanned(true, 3),
            jpeg_tests::scanned(false, 3),
        ] {
            let new_decoder = || zune_jpeg::JpegDecoder::new(ZCursor::new(bytes.as_slice()));
            let mut full = new_decoder();
            let expected = full.decode().unwrap();
            let (_, height) = full.dimensions().unwrap();
            let stride = expected.len() / height;
            let mut scratch = vec![0; stride];
            let mut rows = 0;
            new_decoder().decode_rows_into(&mut scratch, |y, samples| {
                assert_eq!(y, rows);
                assert_eq!(samples, &expected[y * stride..(y + 1) * stride]);
                rows += 1;
                Ok(())
            }).unwrap();
            assert_eq!(rows, height);
            let mut rows = 0;
            let error = new_decoder().decode_rows_into(&mut scratch, |_, _| {
                rows += 1;
                Err(DecodeErrors::FormatStatic("test sink cancelled"))
            }).unwrap_err();
            assert!(matches!(error, DecodeErrors::FormatStatic("test sink cancelled")));
            assert_eq!(rows, 1);
        }
    }

    #[test]
    fn native_interleaved_jpeg_over_the_contiguous_limit_stages_and_retires_rows() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 192 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let (width, height) = (NATIVE_EDGE, 1401);
        assert!(width as usize * height as usize * 3 > MAX_ALLOCATION);
        let path = root.path().join("Streaming.jpg");
        std::fs::write(&path, jpeg_tests::interleaved_geometry(width as u16, height as u16)).unwrap();
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let output = prepare(&base, &session, &path, true, &|| Ok(())).unwrap();
        let surface = output.surface();
        assert_eq!((surface.width(), surface.height(), surface.depth()), (width, height, 8));
        let (x, y) = (width as i32 - 1, height as i32 - 1);
        let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
        let sample = surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap();
        assert_eq!(sample, [128 * 257, 128 * 257, 128 * 257, 65535]);
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(runtime.resident().used(), resident);
        drop((base, session, runtime, connection));
    }

    #[test]
    fn native_jpeg_large_edges_keep_baseline_and_multiple_scan_samples() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(160 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let path = root.path().join("Large source.JPEG");
        for (bytes, width, height, managed, expected) in [
            (jpeg_tests::fixture_geometry(3, NATIVE_EDGE, true), 3, NATIVE_EDGE, false, None),
            (jpeg_tests::scanned_geometry(true, 3, NATIVE_EDGE as u16, 9), NATIVE_EDGE, 9, true, Some([128, 128, 128, 255])),
            (jpeg_tests::scanned_geometry(false, 3, NATIVE_EDGE as u16, 9), NATIVE_EDGE, 9, true, Some([128, 128, 128, 255])),
        ] {
            std::fs::write(&path, &bytes).unwrap();
            let disk = runtime.disk().used().unwrap();
            let resident = runtime.resident().used();
            let output = prepare(&base, &session, &path, managed, &|| Ok(())).unwrap();
            let surface = output.surface();
            assert_eq!((surface.width(), surface.height(), surface.depth()), (width, height, 8));
            let (x, y) = (width as i32 - 1, height as i32 - 1);
            let expected = expected.unwrap_or_else(|| {
                image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg)
                    .unwrap().to_rgba8().get_pixel(x as u32, y as u32).0
            });
            let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
            let sample = surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap();
            assert_eq!(sample, expected.map(|value| u16::from(value) * 257));
            output.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(runtime.resident().used(), resident);
        }
        drop((base, session, runtime, connection));
    }

    #[test]
    fn coefficient_planes_are_admitted_before_progressive_or_separate_scan_decode() {
        let pool = ResidentBudget::new(128 * 1024 * 1024).unwrap();
        for progressive in [false, true] {
            let bytes = jpeg_tests::scanned(progressive, 3);
            let info = super::super::inspect(&bytes, &|| Ok(())).unwrap();
            let charges = coefficient_admission(&info, &pool).unwrap();
            assert_eq!(pool.used(), 6 * 64 * 2 * 3);
            drop(charges);
            assert_eq!(pool.used(), 0);
            let mut too_large = info;
            too_large.width = 8192;
            too_large.height = 8192;
            assert!(coefficient_admission(&too_large, &pool).is_err());
            assert_eq!(pool.used(), 0);
        }
    }

    #[test]
    fn jpeg_snapshot_and_native_outputs_retire_on_budget_refusal_cancel_and_retry() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(192 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = root.path().join("source.jpeg");
        std::fs::write(
            &path,
            jpeg_tests::profiled(&jpeg_tests::fixture(false), true),
        )
        .unwrap();
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let holds: Vec<_> = (0..3)
            .map(|_| {
                runtime
                    .resident()
                    .reserve_bytes(48 * 1024 * 1024, MAX_ALLOCATION)
                    .unwrap()
            })
            .collect();
        assert!(prepare(&base, &session, &path, true, &|| Ok(())).is_err());
        drop(holds);
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let reason = prepare(&base, &session, &path, true, &|| {
            if runtime.disk().used().unwrap() > disk {
                Err("JPEG cancelled".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(reason.contains("JPEG cancelled"), "{reason}");
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let mut malformed = jpeg_tests::fixture(false);
        let quant = malformed
            .windows(2)
            .position(|v| v == [0xff, 0xdb])
            .unwrap();
        malformed[quant + 4] = 4; // Quantization table 4 is outside the JPEG table set.
        std::fs::write(&path, malformed).unwrap();
        assert!(prepare(&base, &session, &path, true, &|| Ok(())).is_err());
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let mut oversized = jpeg_tests::fixture(false);
        let frame = oversized
            .windows(2)
            .position(|v| v == [0xff, 0xc0])
            .unwrap();
        oversized[frame + 5..frame + 7].copy_from_slice(&(NATIVE_EDGE as u16).to_be_bytes());
        oversized[frame + 7..frame + 9].copy_from_slice(&(NATIVE_EDGE as u16).to_be_bytes());
        std::fs::write(&path, oversized).unwrap();
        let reason = prepare(&base, &session, &path, true, &|| Ok(()))
            .err()
            .unwrap();
        assert!(reason.contains("contiguous-buffer"), "{reason}");
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let mut progressive = jpeg_tests::scanned(true, 1);
        let frame = progressive.windows(2).position(|v| v == [0xff, 0xc2]).unwrap();
        progressive[frame + 5..frame + 7].copy_from_slice(&3000u16.to_be_bytes());
        progressive[frame + 7..frame + 9].copy_from_slice(&(NATIVE_EDGE as u16).to_be_bytes());
        std::fs::write(&path, progressive).unwrap();
        let reason = prepare(&base, &session, &path, true, &|| Ok(())).err().unwrap();
        assert!(reason.contains("coefficient plane"), "{reason}");
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        std::fs::write(
            &path,
            jpeg_tests::profiled(&jpeg_tests::fixture(false), true),
        )
        .unwrap();
        let output = prepare(&base, &session, &path, true, &|| Ok(())).unwrap();
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
