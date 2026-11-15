//! Stream QOI into native tile rows without an encoded or decoded frame buffer.
use super::source_file::{FileLimits, FileSource};
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    large_document::ScratchBytes,
    raster::{CanvasLimits, NATIVE_EDGE},
    selection::NativeRows,
    workspace::source_runtime::SourceSession,
};
use std::{fs::File, io::Read, path::Path};

pub(super) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    encoded_bytes: super::ENCODED_LIMIT as u64,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Header {
    width: u32,
    height: u32,
    channels: u8,
    colorspace: u8,
}
impl Header {
    fn read(input: &mut impl Read) -> Result<Self, String> {
        let mut bytes = [0; 14];
        input
            .read_exact(&mut bytes)
            .map_err(|_| "Truncated QOI header")?;
        if &bytes[..4] != b"qoif" || ![3, 4].contains(&bytes[12]) || bytes[13] > 1 {
            return Err("Invalid QOI signature, channels or color space".into());
        }
        let header = Self {
            width: u32::from_be_bytes(bytes[4..8].try_into().unwrap()),
            height: u32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            channels: bytes[12],
            colorspace: bytes[13],
        };
        CanvasLimits::Native.check(header.width, header.height)?;
        Ok(header)
    }
}

pub(super) fn inspect(path: &Path) -> Result<super::Info, String> {
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let mut file = File::open(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.len() > FILE_LIMITS.encoded_bytes {
        return Err("Encoded QOI image must be a file at most 128 MiB".into());
    }
    let header = Header::read(&mut file)?;
    Ok(super::Info {
        format: "Qoi".into(),
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
        "qoi",
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
    input.admit(&mut || check())?;
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        input.start_read()?;
        if Header::read(&mut input)? != header {
            return Err("QOI header changed after admission".into());
        }
        let mut writer = NativeRows::new(header.width, header.height, 8, store.clone())
            .map_err(|e| e.to_string())?;
        let size = header.width as usize * 4;
        let mut row = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
        let mut checked = || guard();
        decode(
            &mut input.decoder_stream(),
            header,
            &mut row,
            &mut || check(),
            |y, row| {
                if row.iter().all(|&byte| byte == 0) {
                    return Ok(());
                }
                writer.zero_until(y, &mut checked).map_err(|e| e.to_string())?;
                writer.row8(y, row, &mut checked).map_err(|e| e.to_string())
            },
        )?;
        writer.zero_until(header.height, &mut checked).map_err(|e| e.to_string())?;
        input.finish(&mut || check())?;
        check()?;
        writer.finish(&mut checked).map_err(|e| e.to_string())
    })
}

fn decode(
    input: &mut impl Read,
    header: Header,
    row: &mut [u8],
    check: &mut dyn FnMut() -> Result<(), String>,
    mut emit: impl FnMut(u32, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    if row.len() != header.width as usize * 4 {
        return Err("Invalid QOI row admission".into());
    }
    let byte = |input: &mut _| -> Result<u8, String> {
        let mut b = [0];
        Read::read_exact(input, &mut b).map_err(|_| "Truncated QOI pixels")?;
        Ok(b[0])
    };
    let mut index = [[0u8; 4]; 64];
    let mut pixel = [0u8, 0, 0, 255];
    let mut run = 0u8;
    for y in 0..header.height {
        check()?;
        for output in row.chunks_exact_mut(4) {
            if run > 0 {
                run -= 1;
            } else {
                let tag = byte(input)?;
                let changed = match tag {
                    0xfe => {
                        input
                            .read_exact(&mut pixel[..3])
                            .map_err(|_| "Truncated QOI RGB pixel")?;
                        true
                    }
                    0xff => {
                        if header.channels != 4 {
                            return Err("QOI RGB stream contains an RGBA pixel".into());
                        }
                        input
                            .read_exact(&mut pixel)
                            .map_err(|_| "Truncated QOI RGBA pixel")?;
                        true
                    }
                    _ => match tag >> 6 {
                        0 => {
                            pixel = index[tag as usize];
                            if header.channels == 3 {
                                pixel[3] = 255;
                            }
                            false
                        }
                        1 => {
                            pixel[0] = pixel[0].wrapping_add((tag >> 4 & 3).wrapping_sub(2));
                            pixel[1] = pixel[1].wrapping_add((tag >> 2 & 3).wrapping_sub(2));
                            pixel[2] = pixel[2].wrapping_add((tag & 3).wrapping_sub(2));
                            true
                        }
                        2 => {
                            let next = byte(input)?;
                            let green = (tag & 63).wrapping_sub(32);
                            pixel[0] = pixel[0]
                                .wrapping_add(green)
                                .wrapping_add((next >> 4).wrapping_sub(8));
                            pixel[1] = pixel[1].wrapping_add(green);
                            pixel[2] = pixel[2]
                                .wrapping_add(green)
                                .wrapping_add((next & 15).wrapping_sub(8));
                            true
                        }
                        _ => {
                            run = tag & 63;
                            false
                        }
                    },
                };
                if changed {
                    let hash = (u32::from(pixel[0]) * 3
                        + u32::from(pixel[1]) * 5
                        + u32::from(pixel[2]) * 7
                        + u32::from(pixel[3]) * 11)
                        % 64;
                    index[hash as usize] = pixel;
                }
            }
            output.copy_from_slice(&pixel);
        }
        emit(y, row)?;
    }
    if run != 0 {
        return Err("QOI run exceeds the declared image".into());
    }
    let mut end = [0; 8];
    input
        .read_exact(&mut end)
        .map_err(|_| "Truncated QOI end marker")?;
    if end != [0, 0, 0, 0, 0, 0, 0, 1] {
        return Err("Invalid QOI end marker".into());
    }
    if input.read(&mut [0]).map_err(|e| e.to_string())? != 0 {
        return Err("Trailing QOI image data".into());
    }
    check()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use image::ImageEncoder;
    use std::io::Cursor;

    pub(crate) fn fixture(width: u32, height: u32, channels: usize) -> Vec<u8> {
        let pixels: Vec<_> = (0..width * height)
            .flat_map(|i| {
                let p = match i % 257 {
                    0..=65 => [0, 0, 0, 255],
                    66..=129 => [73, 119, 191, 137],
                    130..=160 => [0, 0, 0, 255],
                    _ => [
                        (i * 17) as u8,
                        (i * 31) as u8,
                        (i * 7) as u8,
                        if i % 5 == 0 { 0 } else { 193 },
                    ],
                };
                p[..channels].to_vec()
            })
            .collect();
        let mut bytes = Vec::new();
        image::codecs::qoi::QoiEncoder::new(&mut bytes)
            .write_image(
                &pixels,
                width,
                height,
                if channels == 3 {
                    image::ExtendedColorType::Rgb8
                } else {
                    image::ExtendedColorType::Rgba8
                },
            )
            .unwrap();
        bytes
    }
    fn pixels(bytes: &[u8]) -> Result<Vec<u8>, String> {
        let mut input = Cursor::new(bytes);
        let header = Header::read(&mut input)?;
        let mut row = vec![0; header.width as usize * 4];
        let mut result = Vec::new();
        decode(&mut input, header, &mut row, &mut || Ok(()), |_, row| {
            result.extend_from_slice(row);
            Ok(())
        })?;
        Ok(result)
    }
    #[test]
    fn streaming_qoi_matches_installed_decoder_for_runs_indexes_and_rgba_channels() {
        for channels in [3, 4] {
            for (w, h) in [(1, 1), (31, 19), (270, 9), (16000, 3), (3, 16000)] {
                for colorspace in [0, 1] {
                    let mut bytes = fixture(w, h, channels);
                    bytes[13] = colorspace;
                    let expected =
                        image::load_from_memory_with_format(&bytes, image::ImageFormat::Qoi)
                            .unwrap()
                            .to_rgba8();
                    assert_eq!(pixels(&bytes).unwrap(), expected.into_raw());
                }
            }
        }
    }
    #[test]
    fn qoi_rejects_bad_headers_truncation_overlong_runs_and_trailing_data() {
        let valid = fixture(31, 19, 4);
        for at in [0, 3, 13, 14, valid.len() / 2, valid.len() - 1] {
            assert!(pixels(&valid[..at]).is_err());
        }
        for (at, value) in [(0, b'X'), (12, 2), (13, 2), (valid.len() - 1, 0)] {
            let mut bad = valid.clone();
            bad[at] = value;
            assert!(pixels(&bad).is_err());
        }
        let mut trailing = valid.clone();
        trailing.push(0);
        assert!(pixels(&trailing).is_err());
        let mut bad = fixture(1, 1, 4);
        bad.splice(14..bad.len() - 8, [0xfd]);
        assert!(pixels(&bad).unwrap_err().contains("run exceeds"));
        for width in [0u32, NATIVE_EDGE + 1, u32::MAX] {
            let mut bad = valid.clone();
            bad[4..8].copy_from_slice(&width.to_be_bytes());
            assert!(Header::read(&mut Cursor::new(bad)).is_err());
        }
        let mut square = valid[..14].to_vec();
        square[4..8].copy_from_slice(&NATIVE_EDGE.to_be_bytes());
        square[8..12].copy_from_slice(&NATIVE_EDGE.to_be_bytes());
        let header = Header::read(&mut Cursor::new(square)).unwrap();
        assert_eq!((header.width, header.height), (NATIVE_EDGE, NATIVE_EDGE));
        let mut rgb = fixture(1, 1, 3);
        rgb.splice(14..rgb.len() - 8, [0xff, 1, 2, 3, 4]);
        assert!(pixels(&rgb).unwrap_err().contains("RGBA pixel"));
    }
    #[test]
    fn staged_qoi_is_native_and_retires_on_decode_failure_and_cancellation() {
        use crate::engine::native::worker::tests::Fixture;
        let fixture_root = Fixture::new();
        let (runtime, connection) =
            fixture_root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = fixture_root.path().join("source.qoi");
        let bytes = fixture(270, 9, 4);
        std::fs::write(&path, &bytes).unwrap();
        let expected = pixels(&bytes).unwrap();
        let baseline = runtime.disk().used().unwrap();
        let peak = std::cell::Cell::new(baseline);
        let output = prepare(&base, &session, &path, &|| {
            peak.set(peak.get().max(runtime.disk().used().unwrap()));
            Ok(())
        })
        .unwrap();
        assert_eq!(output.surface().depth(), 8);
        for y in 0..9 {
            let row = output
                .surface()
                .prepare_region(
                    crate::raster::backed::Region::new([0, y, 270, y + 1]).unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            for x in 0..270 {
                let at = (y as usize * 270 + x as usize) * 4;
                assert_eq!(
                    row.pixel16(x, y).unwrap(),
                    std::array::from_fn(|c| u16::from(expected[at + c]) * 257)
                );
            }
        }
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        for threshold in [baseline + 1, peak.get()] {
            let reason = prepare(&base, &session, &path, &|| {
                if runtime.disk().used().unwrap() >= threshold {
                    Err("QOI interrupted".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert!(reason.contains("QOI interrupted"), "{reason}");
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
        let mut malformed = bytes;
        malformed.pop();
        std::fs::write(&path, malformed).unwrap();
        assert!(prepare(&base, &session, &path, &|| Ok(())).is_err());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        drop((base, session, runtime, connection));
    }
}
