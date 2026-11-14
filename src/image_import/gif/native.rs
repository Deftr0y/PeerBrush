//! Incremental GIF rows and disposal composition over private native sources.
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    image_import::source_file::{FileLimits, FileSource},
    large_document::ScratchBytes,
    raster::{backed::Region, CanvasLimits, Surface, NATIVE_EDGE},
    selection::NativeRows,
    workspace::source_runtime::SourceSession,
};
use ::gif::{DecodeOptions, DisposalMethod, MemoryLimit};
use std::{
    io::{self, Read},
    path::Path,
};

pub(in crate::image_import) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    encoded_bytes: crate::image_import::ENCODED_LIMIT as u64,
};

struct Checked<'a, R> {
    input: R,
    check: &'a dyn Fn() -> Result<(), String>,
}
impl<R: Read> Read for Checked<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        (self.check)().map_err(io::Error::other)?;
        self.input.read(bytes)
    }
}
#[derive(Clone, Copy)]
struct Frame {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
    interlaced: bool,
    disposal: DisposalMethod,
}
impl Frame {
    fn row(self, y: u32) -> u32 {
        if !self.interlaced {
            return y;
        }
        let mut offset = 0;
        for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
            if y >= start && (y - start) % step == 0 {
                return offset + (y - start) / step;
            }
            offset += self.height.saturating_sub(start).div_ceil(step);
        }
        unreachable!()
    }
}

fn retire(
    result: Result<Option<StagedOutput>, String>,
    input: StagedOutput,
    session: &SourceSession,
) -> Result<Option<StagedOutput>, String> {
    match (result, input.abort(session, &|| session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!("{reason}; GIF retirement refused: {cleanup}")),
        (Ok(output), Err(reason)) => {
            if let Some(output) = output {
                if let Err(cleanup) = output.abort(session, &|| session.cleanup_check()) {
                    return Err(format!(
                        "{reason}; GIF candidate retirement refused: {cleanup}"
                    ));
                }
            }
            Err(reason)
        }
    }
}

fn composite(
    base: &NativeDocument,
    session: &SourceSession,
    info: super::Info,
    frame: Frame,
    pixels: Option<&Surface>,
    previous: Option<&Surface>,
    raw_channels: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        let mut writer = NativeRows::new(info.width, info.height, 8, store.clone())
            .map_err(|e| e.to_string())?;
        let bytes = info.width as usize * 8;
        let mut row = ScratchBytes::new(base.budget(), bytes, bytes).map_err(|e| e.to_string())?;
        for y in 0..info.height {
            check()?;
            let old = previous
                .map(|p| {
                    p.prepare_region(
                        Region::new([0, y as i32, info.width as i32, y as i32 + 1]).unwrap(),
                        &mut || guard(),
                    )
                })
                .transpose()
                .map_err(|e| e.to_string())?;
            let inside_y = y >= frame.top && y - frame.top < frame.height;
            let sy = if inside_y {
                frame.row(y - frame.top)
            } else {
                0
            };
            let incoming = pixels
                .filter(|_| inside_y && frame.left < info.width)
                .map(|pixels| {
                    pixels.prepare_region(
                        Region::new([
                            0,
                            sy as i32,
                            frame.width.min(info.width - frame.left) as i32,
                            sy as i32 + 1,
                        ])
                        .unwrap(),
                        &mut || guard(),
                    )
                })
                .transpose()
                .map_err(|e| e.to_string())?;
            for (x, out) in row.chunks_exact_mut(8).enumerate() {
                let x = x as u32;
                let inside = inside_y && x >= frame.left && x - frame.left < frame.width;
                let mut pixel = old
                    .as_ref()
                    .map(|p| p.pixel16(x as i32, y as i32))
                    .transpose()
                    .map_err(|e| e.to_string())?
                    .unwrap_or([0; 4]);
                if inside {
                    if let Some(incoming) = &incoming {
                        let incoming = incoming
                            .pixel16((x - frame.left) as i32, sy as i32)
                            .map_err(|e| e.to_string())?;
                        // Raw first-frame decoding retains hidden palette RGB.
                        if incoming[3] != 0 || raw_channels {
                            pixel = incoming;
                        }
                    } else {
                        pixel = [0; 4];
                    }
                }
                for (out, word) in out.chunks_exact_mut(2).zip(pixel) {
                    out.copy_from_slice(&word.to_be_bytes());
                }
            }
            writer
                .row(y, &row, &mut || guard())
                .map_err(|e| e.to_string())?;
        }
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    command: &serde_json::Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    prepare_inner(base, session, path, Some(command), check)
}

pub(crate) fn prepare_patch(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    // Patches consume raw channels from the default frame, without import's
    // frame selection, animation composition or color-management policy.
    prepare_inner(base, session, path, None, check)
}

fn prepare_inner(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    command: Option<&serde_json::Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let input = FileSource::open(
        session,
        path,
        FILE_LIMITS,
        "gif",
        &mut || check(),
    )?;
    prepare_source(base, session, input, command, check)
}

pub(in crate::image_import) fn prepare_source(
    base: &NativeDocument,
    session: &SourceSession,
    mut input: FileSource<'_>,
    command: Option<&serde_json::Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    input.start_read()?;
    let info = super::inspect_native(&mut input, command.is_some(), check)?;
    let selected = command.map_or(Ok(Some(0)), |command| {
        command
            .get("frame")
            .map(|v| {
                v.as_u64()
                    .filter(|n| *n < info.frames as u64)
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
    input.admit(&mut || check())?;
    input.start_read()?;
    // No decoder-owned frame buffer: palette conversion retains only one row.
    // Covers bounded XMP growth, LZW dictionaries, buffered input and headers.
    let _decoder =
        crate::psd::memory36::MetadataCharge::reserve(base.budget(), 4 * 1024 * 1024, check)?;
    let mut options = DecodeOptions::new();
    options.set_color_output(::gif::ColorOutput::RGBA);
    options.set_memory_limit(MemoryLimit::Bytes((1024 * 1024).try_into().unwrap()));
    let mut previous: Option<StagedOutput> = None;
    let result = (|| {
        let mut decoder = options
            .read_info(Checked {
                input: &mut input,
                check,
            })
            .map_err(|e| e.to_string())?;
        if (u32::from(decoder.width()), u32::from(decoder.height())) != (info.width, info.height) {
            return Err("GIF dimensions changed after admission".into());
        }
        let canvas_pixels = u64::from(info.width) * u64::from(info.height);
        // Allow decoding and compositing two complete frames even at native size.
        let mut work = crate::transform::native::Work::new(128_000_000.max(4 * canvas_pixels))?;
        for index in 0..=selected {
            check()?;
            let f = decoder
                .next_frame_info()
                .map_err(|e| e.to_string())?
                .ok_or("GIF frame disappeared after admission")?;
            let frame = Frame {
                left: f.left.into(),
                top: f.top.into(),
                width: f.width.into(),
                height: f.height.into(),
                interlaced: f.interlaced,
                disposal: f.dispose,
            };
            CanvasLimits::Native.check(frame.width, frame.height)?;
            let disposal = (index != selected).then_some(frame.disposal);
            let discard = matches!(disposal, Some(DisposalMethod::Previous | DisposalMethod::Background));
            let clear = disposal == Some(DisposalMethod::Background)
                && previous.is_some() && frame.left < info.width && frame.top < info.height;
            if info.frames > 1 || frame.width > info.width || frame.height > info.height {
                let frame_pixels = u64::from(frame.width) * u64::from(frame.height);
                let composition = if !discard || clear { canvas_pixels } else { 0 };
                work.claim(frame_pixels + composition)
                    .map_err(|_| "GIF selected-frame decoding exceeds native work admission")?;
            }
            let size = frame.width as usize * 4;
            let mut bytes = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
            let mut decode_row = |bytes: &mut [u8]| -> Result<(), String> {
                check()?;
                // The converter leaves invalid palette indices unwritten.
                bytes.fill(0);
                if !decoder.fill_buffer(bytes).map_err(|e| e.to_string())? {
                    return Err("Truncated GIF frame pixels".into());
                }
                Ok(())
            };
            let raw = if discard {
                // Validate the pixels even when disposal removes the entire frame.
                for _ in 0..frame.height {
                    decode_row(&mut bytes)?;
                }
                if !clear {
                    continue;
                }
                None
            } else {
                Some(crate::retained::transaction::stage(base, session, check, |store, guard| {
                    let mut writer = NativeRows::new(frame.width, frame.height, 8, store.clone())
                        .map_err(|e| e.to_string())?;
                    let mut row = ScratchBytes::new(base.budget(), size * 2, size * 2)
                        .map_err(|e| e.to_string())?;
                    for y in 0..frame.height {
                        decode_row(&mut bytes)?;
                        for (word, byte) in row.chunks_exact_mut(2).zip(bytes.iter()) {
                            word.fill(*byte);
                        }
                        writer.row(y, &row, &mut || guard()).map_err(|e| e.to_string())?;
                    }
                    writer.finish(&mut || guard()).map_err(|e| e.to_string())
                })?)
            };
            drop(bytes);
            if info.frames == 1 && !frame.interlaced && frame.left == 0 && frame.top == 0
                && (frame.width, frame.height) == (info.width, info.height)
            {
                previous = raw;
                break;
            }
            let candidate = composite(
                base, session, info, frame,
                raw.as_ref().map(StagedOutput::surface),
                previous.as_ref().map(StagedOutput::surface),
                command.is_none() || info.frames == 1,
                check,
            ).map(Some);
            let candidate = match raw {
                Some(raw) => retire(candidate, raw, session)?,
                None => candidate?,
            };
            previous = if let Some(old) = previous.take() {
                retire(Ok(candidate), old, session)?
            } else {
                candidate
            };
        }
        drop(decoder);
        input.finish(&mut || check())?;
        check()?;
        previous
            .take()
            .ok_or_else(|| "GIF selected frame has no output".into())
    })();
    if let Some(previous) = previous {
        retire(result.map(Some), previous, session)?.ok_or_else(|| "GIF output missing".into())
    } else {
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::{gif_import_tests, tests::Fixture};

    #[test]
    fn native_static_gif_keeps_wide_and_tall_palette_samples() {
        let fixture = Fixture::new();
        let (runtime, connection) = fixture.runtime_with_budgets(32 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let path = fixture.path().join("Large static.GIF");
        let edge = NATIVE_EDGE as u16;
        for (width, height, interlaced) in [(edge, 3, false), (3, edge, true)] {
            let bytes = gif_import_tests::static_geometry(width, height, interlaced);
            std::fs::write(&path, &bytes).unwrap();
            let disk = runtime.disk().used().unwrap();
            for raw in [false, true] {
                let output = if raw {
                    prepare_patch(&base, &session, &path, &|| Ok(())).unwrap()
                } else {
                    prepare(&base, &session, &path, &serde_json::json!({}), &|| Ok(())).unwrap()
                };
                let surface = output.surface();
                let (w, h) = (u32::from(width), u32::from(height));
                assert_eq!((surface.width(), surface.height(), surface.depth()), (w, h, 8));
                let (x, y) = (w as i32 - 1, h as i32 - 1);
                let region = Region::new([x - 1, y, x + 1, y + 1]).unwrap();
                let pixels = surface.prepare_region(region, &mut || Ok(())).unwrap();
                assert_eq!(pixels.pixel16(x, y).unwrap(), [53, 67, 89, 255].map(|v| v * 257));
                assert_eq!(pixels.pixel16(x - 1, y).unwrap(), [17, 29, 43, 0].map(|v| v * 257));
                drop(pixels);
                output.abort(&session, &|| session.cleanup_check()).unwrap();
                assert_eq!(runtime.disk().used().unwrap(), disk);
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        drop((base, session, runtime, connection));
    }

    #[test]
    fn native_wide_animation_stages_only_pixels_needed_by_the_selected_frame() {
        use ::gif::{DisposalMethod, Encoder, Frame};
        use std::borrow::Cow;

        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(32 * 1024 * 1024, 8 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let path = fixture.path().join("Wide animation.gif");
        let edge = NATIVE_EDGE as u16;
        let mut bytes = Vec::new();
        {
            let mut encoder = Encoder::new(&mut bytes, edge, 3, &[0, 0, 0, 53, 67, 89, 101, 113, 127]).unwrap();
            let mut first = vec![0; edge as usize];
            first[0] = 1;
            first[edge as usize - 2] = 1;
            encoder.write_frame(&Frame {
                top: 1, width: edge, height: 1, dispose: DisposalMethod::Keep,
                transparent: Some(0), buffer: Cow::Owned(first), ..Frame::default()
            }).unwrap();
            // A distinct marker in each dense tile defeats store deduplication.
            for (width, dispose) in [(edge, DisposalMethod::Previous), (edge - 2, DisposalMethod::Background)] {
                let pixels = (0..u32::from(width))
                    .map(|x| if x % crate::raster::TILE == x / crate::raster::TILE { 1 } else { 2 })
                    .collect();
                encoder.write_frame(&Frame {
                    top: 1, width, height: 1, dispose,
                    buffer: Cow::Owned(pixels), ..Frame::default()
                }).unwrap();
            }
            encoder.write_frame(&Frame {
                left: edge - 2, top: 1, width: 2, height: 1, transparent: Some(0),
                buffer: Cow::Borrowed(&[0, 2]), ..Frame::default()
            }).unwrap();
        }
        std::fs::write(&path, &bytes).unwrap();
        let disk = runtime.disk().used().unwrap();
        let error = prepare(&base, &session, &path, &serde_json::json!({}), &|| Ok(())).err().unwrap();
        assert!(error.contains("explicit frame index"));
        let output = prepare(&base, &session, &path, &serde_json::json!({"frame": 3}), &|| Ok(())).unwrap();
        let surface = output.surface();
        assert_eq!((surface.width(), surface.height(), surface.depth()), (NATIVE_EDGE, 3, 8));
        let cleared = surface.prepare_region(Region::new([0, 1, 1, 2]).unwrap(), &mut || Ok(())).unwrap();
        assert_eq!(cleared.pixel16(0, 1).unwrap(), [0; 4]);
        drop(cleared);
        let right = i32::from(edge);
        let region = Region::new([right - 2, 0, right, 2]).unwrap();
        let pixels = surface.prepare_region(region, &mut || Ok(())).unwrap();
        assert_eq!(pixels.pixel16(right - 2, 0).unwrap(), [0; 4]);
        assert_eq!(pixels.pixel16(right - 2, 1).unwrap(), [53, 67, 89, 255].map(|v| v * 257));
        assert_eq!(pixels.pixel16(right - 1, 1).unwrap(), [101, 113, 127, 255].map(|v| v * 257));
        drop(pixels);
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn interlaced_source_rows_are_a_complete_permutation_for_short_and_tiled_frames() {
        for height in (1..18).chain([255, 256, 257, 513]) {
            let frame = Frame {
                left: 0,
                top: 0,
                width: 1,
                height,
                interlaced: true,
                disposal: DisposalMethod::Keep,
            };
            let order: Vec<_> = [(0, 8), (4, 8), (2, 4), (1, 2)]
                .into_iter()
                .flat_map(|(start, step)| (start..height).step_by(step))
                .collect();
            for (encoded, display) in order.into_iter().enumerate() {
                assert_eq!(frame.row(display), encoded as u32);
            }
        }
    }

    #[test]
    fn gif_cancellation_budget_refusal_and_retry_retire_all_intermediate_frames() {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(32 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = fixture.path().join("source.gif");
        std::fs::write(&path, gif_import_tests::fixture()).unwrap();
        let command = serde_json::json!({"frame":4});
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let hold = runtime
            .resident()
            .reserve_bytes(30 * 1024 * 1024 - resident, 32 * 1024 * 1024)
            .unwrap();
        assert!(prepare(&base, &session, &path, &command, &|| Ok(())).is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop(hold);
        assert_eq!(runtime.resident().used(), resident);
        let reason = prepare(&base, &session, &path, &command, &|| {
            if runtime.disk().used().unwrap() > disk {
                Err("GIF import interrupted".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(reason.contains("GIF import interrupted"), "{reason}");
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let mut malformed = gif_import_tests::fixture();
        assert_eq!(malformed.pop(), Some(0x3b));
        // Structural inspection succeeds, then the final frame's invalid LZW
        // size fails after earlier frames have produced retained native output.
        malformed.extend_from_slice(b"\x2c\0\0\0\0\x01\0\x01\0\0\xff\0\x3b");
        std::fs::write(&path, malformed).unwrap();
        assert!(prepare(
            &base,
            &session,
            &path,
            &serde_json::json!({"frame":5}),
            &|| Ok(())
        )
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        std::fs::write(&path, gif_import_tests::fixture()).unwrap();
        let output = prepare(&base, &session, &path, &command, &|| Ok(())).unwrap();
        assert_eq!(
            (
                output.surface().width(),
                output.surface().height(),
                output.surface().depth()
            ),
            (270, 9, 8)
        );
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop((base, session, runtime, connection));
    }

    #[test]
    fn raw_first_frame_keeps_transparent_palette_rgb_and_does_not_interpret_icc() {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(32 * 1024 * 1024, 64 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = fixture.path().join("raw.gif");
        for (profiled, bytes) in [
            (false, gif_import_tests::fixture()),
            (true, gif_import_tests::profiled_fixture()),
        ] {
            std::fs::write(&path, bytes).unwrap();
            if profiled {
                assert!(crate::image_import::Encoded::file(&path)
                    .err()
                    .unwrap()
                    .contains("ICC"));
            }
            let expected = image::open(&path).unwrap().into_rgba8();
            assert_eq!(expected.get_pixel(0, 0).0, [17, 29, 43, 0]);
            let disk = runtime.disk().used().unwrap();
            let output = prepare_patch(&base, &session, &path, &|| Ok(())).unwrap();
            assert_eq!(output.surface().depth(), 8);
            for y in 0..expected.height() {
                let row = output
                    .surface()
                    .prepare_region(
                        Region::new([0, y as i32, expected.width() as i32, y as i32 + 1]).unwrap(),
                        &mut || Ok(()),
                    )
                    .unwrap();
                for x in 0..expected.width() {
                    assert_eq!(
                        row.pixel16(x as i32, y as i32).unwrap(),
                        expected.get_pixel(x, y).0.map(|v| u16::from(v) * 257)
                    );
                }
            }
            output.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop((base, session, runtime, connection));
    }
}
