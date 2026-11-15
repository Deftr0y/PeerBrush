//! Native TIFF coding units over an authenticated, session-owned byte snapshot.
use super::Directory;
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    image_import::source_file::{FileLimits, FileSource},
    large_document::ScratchBytes,
    raster::{CanvasLimits, NATIVE_EDGE},
    selection::NativeRows,
    workspace::source_runtime::SourceSession,
};
use ::tiff::{
    decoder::{BufferLayoutPreference, ChunkType, Decoder, TiffCodingUnit},
    tags::{SampleFormat, Tag},
    ColorType,
};
use image::metadata::Orientation;
use std::{
    io::{self, BufReader, Read, Seek, SeekFrom},
    path::Path,
};

const CHUNK_BYTES: usize = 64 * 1024 * 1024;
const DECODER_BYTES: usize = 32 * 1024 * 1024;
pub(in crate::image_import) const FILE_LIMITS: FileLimits = FileLimits {
    edge: NATIVE_EDGE,
    pixels: NATIVE_EDGE as u64 * NATIVE_EDGE as u64,
    // A 16000-square uncompressed RGBA16 page needs about 2 GB of samples.
    encoded_bytes: 3 * 1024 * 1024 * 1024,
};

/// An explicit page view over immutable original bytes; all other offsets stay
/// container-relative. The view owns no file or document publication authority.
struct Page<R> {
    input: R,
    position: u64,
    pointer_at: u64,
    pointer: [u8; 8],
    length: usize,
}
impl<R: Read + Seek> Page<R> {
    fn new(mut input: R, directory: &Directory, page: usize) -> Result<Self, String> {
        let offset = *directory
            .offsets()
            .get(page)
            .ok_or("TIFF page index is outside the available items")?;
        let h = directory.header;
        let length = h.pointer();
        let bytes = if h.little {
            offset.to_le_bytes()
        } else {
            offset.to_be_bytes()
        };
        let mut pointer = [0; 8];
        pointer[..length].copy_from_slice(if h.little {
            &bytes[..length]
        } else {
            &bytes[8 - length..]
        });
        input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
        Ok(Self {
            input,
            position: 0,
            pointer_at: if h.big { 8 } else { 4 },
            pointer,
            length,
        })
    }
}
impl<R: Read> Read for Page<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = self.input.read(out)?;
        let end = self
            .position
            .checked_add(count as u64)
            .ok_or_else(|| io::Error::other("TIFF cursor overflow"))?;
        let first = self.position.max(self.pointer_at);
        let last = end.min(self.pointer_at + self.length as u64);
        if first < last {
            out[(first - self.position) as usize..(last - self.position) as usize].copy_from_slice(
                &self.pointer
                    [(first - self.pointer_at) as usize..(last - self.pointer_at) as usize],
            );
        }
        self.position = end;
        Ok(count)
    }
}
impl<R: Seek> Seek for Page<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.position = self.input.seek(to)?;
        Ok(self.position)
    }
}

#[derive(Clone, Copy)]
struct Color {
    depth: u16,
    samples: usize,
    gray: bool,
    cmyk: bool,
    bilevel: bool,
}
impl Color {
    fn new(color: ColorType, raw: bool) -> Result<Self, String> {
        let (bits,samples,gray,cmyk) = match color {
            ColorType::Gray(bits @ (1|8|16)) => (bits,1,true,false),
            ColorType::GrayA(bits @ (8|16)) => (bits,2,true,false),
            ColorType::RGB(bits @ (8|16)) => (bits,3,false,false),
            ColorType::RGBA(bits @ (8|16)) => (bits,4,false,false),
            ColorType::CMYK(bits @ (8|16)) if raw => (bits,4,false,true),
            _ => return Err("TIFF import requires supported integer RGB/grayscale channels; convert other encodings explicitly".into()),
        };
        Ok(Self {
            depth: u16::from(bits.max(8)),
            samples,
            gray,
            cmyk,
            bilevel: bits == 1,
        })
    }
    fn validate(
        self,
        layout: &BufferLayoutPreference,
        width: u32,
        height: u32,
    ) -> Result<(), String> {
        if layout.sample_format != SampleFormat::Uint
            || (layout.planes != 1 && layout.planes != self.samples)
        {
            return Err("TIFF sample format or plane layout differs from admission".into());
        }
        let samples = if layout.planes == 1 { self.samples } else { 1 };
        let bits = if self.bilevel {
            1
        } else {
            usize::from(self.depth)
        };
        let row_bytes = (width as usize * samples * bits).div_ceil(8);
        let row_stride = layout.row_stride.ok_or("TIFF row stride missing")?.get();
        let plane_stride = layout
            .plane_stride
            .ok_or("TIFF plane stride missing")?
            .get();
        let plane = row_stride
            .checked_mul(height as usize)
            .ok_or("TIFF plane size overflow")?;
        let length = plane_stride
            .checked_mul(layout.planes - 1)
            .and_then(|n| n.checked_add(plane))
            .ok_or("TIFF buffer size overflow")?;
        if row_stride < row_bytes || plane_stride < plane || layout.complete_len < length {
            return Err("TIFF coding unit buffer is incomplete".into());
        }
        if self.cmyk && layout.planes != 1 {
            return Err("Planar CMYK TIFF patches require explicit RGB conversion".into());
        }
        Ok(())
    }
    fn row(self, bytes: &[u8], layout: &BufferLayoutPreference, y: usize, out: &mut [u8]) {
        let row_stride = layout.row_stride.unwrap().get();
        let plane_stride = layout.plane_stride.unwrap().get();
        let planar = layout.planes > 1;
        for (x, pixel) in out.chunks_exact_mut(8).enumerate() {
            let sample = |c: usize| -> u16 {
                let base = (if planar { c * plane_stride } else { 0 }) + y * row_stride;
                if self.bilevel {
                    return if bytes[base + x / 8] & (128 >> (x % 8)) == 0 {
                        0
                    } else {
                        65535
                    };
                }
                let index = base
                    + (if planar { x } else { x * self.samples + c }) * usize::from(self.depth / 8);
                if self.depth == 8 {
                    u16::from(bytes[index]) * 257
                } else {
                    u16::from_ne_bytes([bytes[index], bytes[index + 1]])
                }
            };
            let rgba = if self.cmyk {
                // Match image's raw CMYK patch projection, including f32 rounding.
                let scale = if self.depth == 8 { 257 } else { 1 };
                let maximum = if self.depth == 8 { 255.0 } else { 65535.0 };
                let k = 1.0 - f32::from(sample(3) / scale) / maximum;
                let rgb = [0, 1, 2]
                    .map(|c| ((maximum - f32::from(sample(c) / scale)) * k) as u16 * scale);
                [rgb[0], rgb[1], rgb[2], 65535]
            } else if self.gray {
                let v = sample(0);
                [v, v, v, if self.samples == 2 { sample(1) } else { 65535 }]
            } else {
                [
                    sample(0),
                    sample(1),
                    sample(2),
                    if self.samples == 4 { sample(3) } else { 65535 },
                ]
            };
            for (word, value) in pixel.chunks_exact_mut(2).zip(rgba) {
                word.copy_from_slice(&value.to_be_bytes());
            }
        }
    }
}

fn rows<R: Read + Seek>(
    decoder: &mut Decoder<R>,
    color: Color,
    width: u32,
    height: u32,
    base: &NativeDocument,
    session: &SourceSession,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let (chunk_width, chunk_height) = decoder.chunk_dimensions();
    CanvasLimits::Native.check(chunk_width, chunk_height)?;
    let columns = width.div_ceil(chunk_width);
    let lines = height.div_ceil(chunk_height);
    let units = columns
        .checked_mul(lines)
        .ok_or("TIFF coding unit count overflow")?;
    let count = match decoder.get_chunk_type() {
        ChunkType::Strip => decoder.strip_count(),
        ChunkType::Tile => decoder.tile_count(),
    }
    .map_err(|e| e.to_string())?;
    if count != units
        && count
            != units
                .checked_mul(color.samples as u32)
                .ok_or("TIFF plane count overflow")?
    {
        return Err("TIFF coding unit count differs from its image".into());
    }
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        let mut writer = NativeRows::new(width, height, color.depth, store.clone())
            .map_err(|e| e.to_string())?;
        let mut row = ScratchBytes::new(base.budget(), width as usize * 8, width as usize * 8)
            .map_err(|e| e.to_string())?;
        // shortcut: the installed TIFF API decodes complete coding units. Admit
        // at most 64 MiB per unit/band; use incremental codec APIs before lifting it.
        let band_bytes = width as usize * height.min(chunk_height) as usize * 8;
        let mut band = if columns > 1 {
            Some(
                ScratchBytes::new(base.budget(), band_bytes, CHUNK_BYTES)
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        for by in 0..lines {
            let h = (height - by * chunk_height).min(chunk_height);
            for bx in 0..columns {
                check()?;
                let index = by * columns + bx;
                let unit = TiffCodingUnit(index);
                let layout = decoder
                    .image_coding_unit_layout(unit)
                    .map_err(|e| e.to_string())?;
                let w = (width - bx * chunk_width).min(chunk_width);
                color.validate(&layout, w, h)?;
                let mut decoded =
                    ScratchBytes::new(base.budget(), layout.complete_len, CHUNK_BYTES)
                        .map_err(|e| e.to_string())?;
                decoder
                    .read_coding_unit_bytes(unit, &mut decoded)
                    .map_err(|e| e.to_string())?;
                for y in 0..h {
                    check()?;
                    if let Some(band) = &mut band {
                        let start =
                            (y as usize * width as usize + bx as usize * chunk_width as usize) * 8;
                        color.row(
                            &decoded,
                            &layout,
                            y as usize,
                            &mut band[start..start + w as usize * 8],
                        );
                    } else {
                        color.row(&decoded, &layout, y as usize, &mut row);
                        writer
                            .row(by * chunk_height + y, &row, &mut || guard())
                            .map_err(|e| e.to_string())?;
                    }
                }
            }
            if let Some(band) = &band {
                for y in 0..h {
                    let start = y as usize * width as usize * 8;
                    writer
                        .row(
                            by * chunk_height + y,
                            &band[start..start + width as usize * 8],
                            &mut || guard(),
                        )
                        .map_err(|e| e.to_string())?;
                }
            }
        }
        writer.finish(&mut || guard()).map_err(|e| e.to_string())
    })
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    command: Option<&serde_json::Value>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    if command.is_some_and(|c| c.get("frame").is_some() || c.get("variant").is_some()) {
        return Err("This image accepts page, not frame or variant".into());
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .filter(|_| super::matches(path))
        .ok_or("TIFF extension missing")?;
    let file = FileSource::open(
        session,
        path,
        FILE_LIMITS,
        extension,
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
    // Includes the first-directory hash map, bounded IFD values, conversion
    // copies, buffered input, and the installed lossless codec working state.
    let _decoder = base
        .budget()
        .reserve_bytes(DECODER_BYTES, DECODER_BYTES)
        .map_err(|e| e.to_string())?;
    let encoded = file.snapshot(&mut || check())?;
    let result = (|| {
        let mut io_check = || {
            check()
                .and_then(|()| encoded.check())
                .map_err(|_| crate::large_document::Error::Cancelled)
        };
        let cursor = encoded
            .bytes()
            .cursor(0, encoded.bytes().payload_length(), &mut io_check)
            .map_err(|e| e.to_string())?;
        let mut input = BufReader::with_capacity(64 * 1024, cursor);
        let directory = Directory::read(&mut input, encoded.bytes().payload_length(), check)?;
        let page = if let Some(c) = command {
            match c.get("page") {
                Some(v) => v
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|&n| n < directory.offsets().len())
                    .ok_or("Image selection index is outside the available items")?,
                None if directory.offsets().len() == 1 => 0,
                None => {
                    return Err("Choose an explicit page index before importing this image".into())
                }
            }
        } else {
            0
        };
        let (width, height) = directory.native_dimensions(&mut input, page, check)?;
        let profile = if command.is_some() {
            directory
                .profile_range(&mut input, page, check)?
                .map(|(offset, length)| -> Result<_, String> {
                    let mut profile = ScratchBytes::new(
                        base.budget(),
                        length,
                        crate::color_profile::MAX_PROFILE_BYTES,
                    )
                    .map_err(|e| e.to_string())?;
                    input
                        .seek(SeekFrom::Start(offset))
                        .map_err(|e| e.to_string())?;
                    input.read_exact(&mut profile).map_err(|e| e.to_string())?;
                    Ok(profile)
                })
                .transpose()?
        } else {
            None
        };
        let mut decoder =
            Decoder::new(Page::new(input, &directory, page)?).map_err(|e| e.to_string())?;
        let mut limits = ::tiff::decoder::Limits::default();
        limits.decoding_buffer_size = CHUNK_BYTES;
        limits.intermediate_buffer_size = 16 * 1024 * 1024;
        decoder = decoder.with_limits(limits);
        let color = Color::new(
            decoder.colortype().map_err(|e| e.to_string())?,
            command.is_none(),
        )?;
        let samples = decoder
            .find_tag_unsigned::<u16>(Tag::SamplesPerPixel)
            .map_err(|e| e.to_string())?
            .unwrap_or(1);
        if usize::from(samples) != color.samples {
            return Err(
                "TIFF extra sample planes require explicit supported channel conversion".into(),
            );
        }
        if decoder.dimensions().map_err(|e| e.to_string())? != (width, height) {
            return Err("TIFF decoder dimensions differ from admission".into());
        }
        let compression = decoder
            .find_tag_unsigned::<u16>(Tag::Compression)
            .map_err(|e| e.to_string())?
            .unwrap_or(1);
        if !matches!(compression, 1 | 4 | 5 | 8 | 32946 | 32773) {
            return Err("Native TIFF compression requires separate decoder allocation admission; export a lossless TIFF or PNG".into());
        }
        let orientation = if command.is_some() {
            decoder
                .find_tag(Tag::Orientation)
                .map_err(|e| e.to_string())?
                .and_then(|v| Orientation::from_exif(v.into_u16().ok()?.min(255) as u8))
                .unwrap_or(Orientation::NoTransforms)
        } else {
            Orientation::NoTransforms
        };
        let source = rows(&mut decoder, color, width, height, base, session, check)?;
        drop(decoder);
        let normalized = crate::image_import::native::normalize(
            base,
            session,
            source.surface(),
            profile.as_ref().map(|p| &**p),
            orientation,
            check,
        );
        let output = retire(normalized, source, session)?;
        if let Err(reason) = file
            .recheck()
            .and_then(|()| encoded.check())
            .and_then(|()| check())
        {
            return retire(Err(reason), output, session);
        }
        Ok(output)
    })();
    match (result, encoded.retire(&mut || session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; encoded TIFF retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => retire(Err(reason), output, session),
    }
}
fn retire(
    result: Result<StagedOutput, String>,
    input: StagedOutput,
    session: &SourceSession,
) -> Result<StagedOutput, String> {
    match (result, input.abort(session, &|| session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; TIFF source retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; TIFF candidate retirement refused: {cleanup}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::native::worker::{tests::Fixture, tiff_tests};
    use serde_json::json;

    #[test]
    fn native_tall_tiff_keeps_raw_and_oriented_samples_at_both_depths() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base = NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(())).unwrap();
        let path = root.path().join("Tall TIFF.tif");
        let command = json!({"page":0});
        for depth in [8, 16] {
            let bytes = tiff_tests::fixture_geometry(depth, false, false, true, false, (3, NATIVE_EDGE), 6);
            std::fs::write(&path, &bytes).unwrap();
            let info = super::super::inspect(&path).unwrap();
            assert_eq!((info.width, info.height, info.bit_depth), (3, NATIVE_EDGE, depth));
            let expected = [0, 1, 2, 3].map(|c| {
                let value = ((2 * 137 + (NATIVE_EDGE - 1) * 271 + c * 13007 + 12347) % 65536) as u16;
                if depth == 8 { u16::from(crate::raster::project16(value)) * 257 } else { value }
            });
            for (options, size, [x, y]) in [
                (None, (3, NATIVE_EDGE), [2, NATIVE_EDGE as i32 - 1]),
                (Some(&command), (NATIVE_EDGE, 3), [0, 2]),
            ] {
                let output = prepare(&base, &session, &path, options, &|| Ok(())).unwrap();
                let surface = output.surface();
                assert_eq!((surface.width(), surface.height(), surface.depth()), (size.0, size.1, depth));
                let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
                assert_eq!(surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap(), expected);
                output.abort(&session, &|| session.cleanup_check()).unwrap();
            }
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(runtime.disk().used().unwrap(), 0);
        }
        drop((base, session, runtime, connection));
    }

    #[test]
    fn selected_tiff_dimensions_are_bounded_before_coding_unit_setup() {
        let mut bytes = tiff_tests::fixture(16, false, false, true, false);
        let ifd = u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize;
        let width = ifd + 2;
        let height = width + 12;
        assert_eq!(&bytes[width..width + 2], &256u16.to_le_bytes());
        assert_eq!(&bytes[height..height + 2], &257u16.to_le_bytes());
        bytes[height + 8..height + 12].copy_from_slice(&NATIVE_EDGE.to_le_bytes());
        for edge in [NATIVE_EDGE, NATIVE_EDGE + 1, u32::MAX] {
            bytes[width + 8..width + 12].copy_from_slice(&edge.to_le_bytes());
            let mut input = io::Cursor::new(&bytes);
            let directory = Directory::read(&mut input, bytes.len() as u64, &|| Ok(())).unwrap();
            let size = directory.native_dimensions(&mut input, 0, &|| Ok(()));
            if edge == NATIVE_EDGE {
                assert_eq!(size.unwrap(), (NATIVE_EDGE, NATIVE_EDGE));
            } else {
                assert!(size.is_err());
            }
        }
    }

    #[test]
    fn admitted_tiff_sources_retire_after_invalid_input_staging_cancellation_and_retry() {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = root.path().join("source.tiff");
        let bytes = tiff_tests::fixture(16, true, true, false, true);
        std::fs::write(&path, &bytes).unwrap();
        let command = json!({});
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let hold = runtime
            .resident()
            .reserve_bytes(240 * 1024 * 1024 - resident, 256 * 1024 * 1024)
            .unwrap();
        assert!(prepare(&base, &session, &path, Some(&command), &|| Ok(())).is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop(hold);
        let encoded =
            crate::large_document::LargeSourceTemporary::encoded_bytes(bytes.len() as u64).unwrap();
        assert!(prepare(&base, &session, &path, Some(&command), &|| {
            if runtime.disk().used().unwrap() > disk + encoded {
                Err("TIFF staging interrupted".into())
            } else {
                Ok(())
            }
        })
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(runtime.resident().used(), resident);
        let mut invalid = bytes.clone();
        invalid.pop();
        std::fs::write(&path, invalid).unwrap();
        assert!(prepare(&base, &session, &path, Some(&command), &|| Ok(())).is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        std::fs::write(&path, bytes).unwrap();
        let output = prepare(&base, &session, &path, Some(&command), &|| Ok(())).unwrap();
        assert_eq!(
            (
                output.surface().width(),
                output.surface().height(),
                output.surface().depth()
            ),
            (19, 270, 16)
        );
        output.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(runtime.resident().used(), resident);
        drop((base, session, runtime, connection));
    }
    #[test]
    fn page_views_patch_only_the_selected_pointer_through_short_reads_and_seeks() {
        let bytes = tiff_tests::compressed(::tiff::encoder::Compression::Uncompressed);
        let directory =
            Directory::read(&mut io::Cursor::new(&bytes), bytes.len() as u64, &|| Ok(())).unwrap();
        let mut page = Page::new(io::Cursor::new(&bytes), &directory, 1).unwrap();
        let mut actual = Vec::new();
        let mut part = [0; 3];
        loop {
            let count = page.read(&mut part).unwrap();
            if count == 0 {
                break;
            }
            actual.extend_from_slice(&part[..count]);
        }
        let mut expected = bytes.clone();
        expected[4..8].copy_from_slice(&(directory.offsets()[1] as u32).to_le_bytes());
        assert_eq!(actual, expected);
        page.seek(SeekFrom::Start(3)).unwrap();
        let mut crossing = [0; 8];
        page.read_exact(&mut crossing).unwrap();
        assert_eq!(&crossing, &expected[3..11]);
    }
}
