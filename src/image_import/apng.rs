//! Selected PNG animation frames over the shared admitted pass-row decoder.
use super::*;
use crate::{
    engine::native::{NativeDocument, StagedOutput},
    image_import::{native, png_admission, source_file::FileSource},
    raster::{backed::Region, Surface, TILE},
    selection::NativeTileWriter,
    workspace::source_runtime::SourceSession,
};
use image::Pixel;
use std::path::Path;

fn blend(target: &mut image::Rgba<u8>, incoming: image::Rgba<u8>, op: png::BlendOp) {
    if op == png::BlendOp::Source {
        *target = incoming;
    } else {
        target.blend(&incoming);
    }
}

fn blend16(target: [u16; 4], incoming: [u16; 4], op: png::BlendOp) -> [u16; 4] {
    if op == png::BlendOp::Source {
        incoming
    } else if incoming[3] == 0 {
        target
    } else {
        crate::raster::blend16(target, incoming, 1.0, "normal")
    }
}

/// Resident imports share pass expansion and disposal with native staging. Keeping
/// the post-disposal canvas avoids restoring pixels from before Background disposal.
pub(crate) fn resident(
    bytes: &[u8],
    selected: usize,
    control: &crate::loading::Control,
) -> Result<image::DynamicImage, String> {
    let check = || control.check();
    let (facts, frames) = container_stream_animation(
        &mut Cursor::new(bytes),
        bytes.len() as u64,
        8192,
        8192 * 8192,
        ColorPolicy::ImportSource,
        true,
        &mut || check(),
    )?;
    if frames.is_none_or(|n| selected >= n as usize) {
        return Err("Invalid PNG animation frame selection".into());
    }
    // Byte 24 is the sample depth in the validated IHDR.
    if bytes[24] == 16 {
        resident_canvas(bytes, selected, facts, &check, |target: &mut image::Rgba<u16>, pixel, op| {
            let incoming = std::array::from_fn(|i| u16::from_be_bytes([pixel[2 * i], pixel[2 * i + 1]]));
            target.0 = blend16(target.0, incoming, op);
        }).map(image::DynamicImage::ImageRgba16)
    } else {
        resident_canvas(bytes, selected, facts, &check, |target, pixel, op| {
            blend(target, image::Rgba([pixel[0], pixel[2], pixel[4], pixel[6]]), op);
        }).map(image::DynamicImage::ImageRgba8)
    }
}

fn resident_canvas<P: Pixel>(
    bytes: &[u8],
    selected: usize,
    (width, height, declared_profile): (u32, u32, bool),
    check: &dyn Fn() -> Result<(), String>,
    composite: impl Fn(&mut P, &[u8], png::BlendOp),
) -> Result<image::ImageBuffer<P, Vec<P::Subpixel>>, String>
where
    P::Subpixel: Default,
{
    let sample_bytes = std::mem::size_of::<P::Subpixel>();
    crate::image_import::check_budget(width, height, (sample_bytes * 8) as u16, crate::image_import::BUDGET)?;
    let pool = ResidentBudget::new(crate::image_import::BUDGET).map_err(|e| e.to_string())?;
    let _decoder = pool
        .reserve_bytes(DECODER_BYTES, DECODER_BYTES)
        .map_err(|e| e.to_string())?;
    let size = width as usize * height as usize * usize::from(P::CHANNEL_COUNT);
    let canvas_bytes = size * sample_bytes;
    let _canvas = pool.reserve_bytes(canvas_bytes, canvas_bytes).map_err(|e| e.to_string())?;
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(size)
        .map_err(|_| "PNG animation canvas allocation failed")?;
    pixels.resize(size, P::Subpixel::default());
    let mut canvas = image::ImageBuffer::<P, _>::from_raw(width, height, pixels)
        .ok_or("PNG animation canvas shape differs")?;
    let binding = SourceBinding::file(sha(bytes), pool.clone());
    let mut callback = || check();
    let cancellation = RefCell::new(&mut callback as &mut dyn FnMut() -> Result<(), String>);
    let mut decoder = png::Decoder::new_with_limits(
        Checked {
            input: Cursor::new(bytes),
            check: &cancellation,
        },
        png::Limits {
            bytes: DECODER_LIMIT,
        },
    );
    decoder.ignore_checksums(false);
    decoder.set_ignore_text_chunk(true);
    decoder.set_transformations(png::Transformations::EXPAND);
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    if declared_profile && reader.info().icc_profile.is_none() {
        return Err("Declared PNG ICC profile could not be decoded within admission".into());
    }
    let mut work = crate::transform::native::Work::new(128_000_000)?;
    if reader.info().frame_control.is_none() {
        work.claim(u64::from(width) * u64::from(height))?;
        decode_frame(
            &mut reader,
            width,
            height,
            binding.clone(),
            ColorPolicy::ImportSource,
            &pool,
            &mut || check(),
            |_, _, _| Ok(()),
        )?;
        reader.next_frame_info().map_err(|e| e.to_string())?;
    }
    for index in 0..=selected {
        if index != 0 {
            reader.next_frame_info().map_err(|e| e.to_string())?;
        }
        let frame = reader
            .info()
            .frame_control
            .ok_or("PNG animation frame control missing")?;
        work.claim(
            u64::from(frame.width) * u64::from(frame.height) + u64::from(width) * u64::from(height),
        )?;
        let draw = index == selected || frame.dispose_op == png::DisposeOp::None;
        let clear = index != selected && frame.dispose_op == png::DisposeOp::Background;
        let complete = decode_frame(
            &mut reader,
            frame.width,
            frame.height,
            binding.clone(),
            ColorPolicy::ImportSource,
            &pool,
            &mut || check(),
            |_, event, data| {
                if draw || clear {
                    for (sample, pixel) in data.chunks_exact(8).enumerate() {
                        let x = frame.x_offset + event.first_x + sample as u32 * event.step_x;
                        let target = canvas.get_pixel_mut(x, frame.y_offset + event.y);
                        if clear {
                            target.channels_mut().fill(P::Subpixel::default());
                        } else {
                            composite(target, pixel, frame.blend_op);
                        }
                    }
                }
                Ok(())
            },
        )?;
        if index == selected {
            reader.finish().map_err(|e| e.to_string())?;
            validate_metadata(reader.info(), &complete)?;
        }
    }
    check()?;
    Ok(canvas)
}

#[derive(Default)]
pub(super) struct Inspection {
    shape: Option<(u32, u32)>,
    frames: Option<u32>,
    controls: u32,
    sequence: u64,
    idat: bool,
    data: bool,
}
impl Inspection {
    pub(super) fn frames(&self) -> Option<u32> {
        self.frames
    }
    pub(super) fn chunk(&mut self, kind: &[u8; 4], data: &[u8], size: u64) -> Result<(), String> {
        let word = |at| u32::from_be_bytes(data[at..at + 4].try_into().unwrap());
        match kind {
            b"iCCP" | b"sRGB" | b"gAMA" | b"cHRM" | b"cICP" | b"eXIf" if self.idat => {
                return Err("PNG color/orientation metadata appears after image data".into());
            }
            b"IHDR" if size == 13 => self.shape = Some((word(0), word(4))),
            b"acTL" => {
                if size != 8
                    || self.frames.is_some()
                    || self.idat
                    || word(0) == 0
                    || word(0) as usize > crate::image_import::MAX_ITEMS
                {
                    return Err("Invalid PNG animation header or frame count".into());
                }
                self.frames = Some(word(0));
            }
            b"fcTL" => {
                if size != 26 || self.frames.is_none() || (self.controls != 0 && !self.data) {
                    return Err("Invalid PNG animation frame control".into());
                }
                self.sequence(word(0))?;
                let (width, height) = self.shape.ok_or("PNG animation lacks dimensions")?;
                if word(4) == 0
                    || word(8) == 0
                    || word(4).checked_add(word(12)).is_none_or(|v| v > width)
                    || word(8).checked_add(word(16)).is_none_or(|v| v > height)
                    || data[24] > 2
                    || data[25] > 1
                    || (!self.idat
                        && (word(4), word(8), word(12), word(16)) != (width, height, 0, 0))
                {
                    return Err(
                        "PNG animation frame exceeds its canvas or has invalid operations".into(),
                    );
                }
                self.controls += 1;
                if self.controls > self.frames.unwrap() {
                    return Err("PNG animation has excess frames".into());
                }
                self.data = false;
            }
            b"IDAT" => {
                if self.controls > 1 {
                    return Err("PNG default image appears after animation frames".into());
                }
                self.idat = true;
                if self.controls == 1 {
                    self.data = true;
                }
            }
            b"fdAT" => {
                if size < 4 || !self.idat || self.controls == 0 {
                    return Err("PNG animation data lacks its frame control".into());
                }
                self.sequence(word(0))?;
                self.data = true;
            }
            b"IEND" if self.frames.is_some() => {
                if !self.idat || !self.data || Some(self.controls) != self.frames {
                    return Err("PNG animation frame count or data is incomplete".into());
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn sequence(&mut self, value: u32) -> Result<(), String> {
        if u64::from(value) != self.sequence {
            return Err("PNG animation sequence differs".into());
        }
        self.sequence += 1;
        Ok(())
    }
}

fn retire(
    result: Result<StagedOutput, String>,
    input: StagedOutput,
    session: &SourceSession,
) -> Result<StagedOutput, String> {
    match (result, input.abort(session, &|| session.cleanup_check())) {
        (result, Ok(())) => result,
        (Err(reason), Err(cleanup)) => Err(format!("{reason}; APNG retirement refused: {cleanup}")),
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; APNG candidate retirement refused: {cleanup}"
            )),
        },
    }
}

fn composite(
    base: &NativeDocument,
    session: &SourceSession,
    shape: (u32, u32),
    frame: png::FrameControl,
    source: Option<&Surface>,
    previous: Option<&Surface>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        let (width, height) = shape;
        let depth = source.or(previous)
            .ok_or("PNG composition requires a canvas or frame")?
            .depth();
        let mut writer = NativeTileWriter::new(width, height, depth, store.clone())
            .map_err(|e| e.to_string())?;
        let stride = usize::from(depth / 8) * 4;
        let size = (TILE * TILE) as usize * stride;
        let mut bytes = ScratchBytes::new(base.budget(), size, size).map_err(|e| e.to_string())?;
        let [fx, fy, fw, fh] = [frame.x_offset, frame.y_offset, frame.width, frame.height].map(|v| v as i32);
        for ty in 0..height.div_ceil(TILE) {
            for tx in 0..width.div_ceil(TILE) {
                check()?;
                let old = previous.map(|p| p.prepare_tile((tx, ty), &mut || guard()))
                    .transpose().map_err(|e| e.to_string())?;
                if let Some(pixels) = old.as_ref().and_then(|tile| tile.bytes()) {
                    bytes.copy_from_slice(pixels);
                } else {
                    bytes.fill(0);
                }
                drop(old);

                let left = (tx * TILE) as i32;
                let top = (ty * TILE) as i32;
                let x0 = left.max(fx);
                let y0 = top.max(fy);
                let x1 = ((tx + 1) * TILE).min(width) as i32;
                let y1 = ((ty + 1) * TILE).min(height) as i32;
                let x1 = x1.min(fx + fw);
                let y1 = y1.min(fy + fh);
                if x0 < x1 && y0 < y1 {
                    let incoming = source.map(|source| {
                        let part = Region::new([x0 - fx, y0 - fy, x1 - fx, y1 - fy])?;
                        source.prepare_region(part, &mut || guard())
                    }).transpose().map_err(|e| e.to_string())?;
                    for y in y0..y1 {
                        check()?;
                        for x in x0..x1 {
                            let at = ((y - top) as usize * TILE as usize + (x - left) as usize) * stride;
                            let out = &mut bytes[at..at + stride];
                            if let Some(incoming) = &incoming {
                                let src = incoming.pixel16(x - fx, y - fy).map_err(|e| e.to_string())?;
                                if depth == 16 {
                                    let dst = std::array::from_fn(|i| u16::from_be_bytes([out[2 * i], out[2 * i + 1]]));
                                    let pixel = blend16(dst, src, frame.blend_op);
                                    for (out, value) in out.chunks_exact_mut(2).zip(pixel) {
                                        out.copy_from_slice(&value.to_be_bytes());
                                    }
                                } else {
                                    let mut target = image::Rgba((&*out).try_into().unwrap());
                                    blend(&mut target, image::Rgba(src.map(|v| (v / 257) as u8)), frame.blend_op);
                                    out.copy_from_slice(&target.0);
                                }
                            } else {
                                out.fill(0);
                            }
                        }
                    }
                }
                writer.tile(tx, ty, &bytes, &mut || guard()).map_err(|e| e.to_string())?;
            }
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
    let input = FileSource::open(session, path, NATIVE_OPEN_LIMITS, "png", &mut || check())?;
    prepare_file(base, session, input, command, check)
}

const INLINE_ANIMATION_LIMITS: FileLimits = FileLimits {
    edge: 8192,
    pixels: 8192 * 8192,
    encoded_bytes: 128 * 1024 * 1024,
};

pub(in crate::image_import) const FALLBACK_OPEN_LIMITS: FileLimits = FileLimits {
    edge: 8192,
    pixels: 32 * 1024 * 1024,
    encoded_bytes: 128 * 1024 * 1024,
};

pub(crate) const NATIVE_OPEN_LIMITS: FileLimits = FileLimits {
    edge: crate::raster::NATIVE_EDGE,
    pixels: crate::raster::NATIVE_EDGE as u64 * crate::raster::NATIVE_EDGE as u64,
    // A dense RGBA16 image can have about 2 GB of incompressible samples.
    encoded_bytes: 3 * 1024 * 1024 * 1024,
};

fn check_animation(width: u32, height: u32, length: u64, limits: FileLimits) -> Result<(), String> {
    if width > limits.edge || height > limits.edge
        || u64::from(width) * u64::from(height) > limits.pixels
        || length > limits.encoded_bytes
    {
        return Err("PNG animation exceeds its native size limits".into());
    }
    Ok(())
}

pub(in crate::image_import) fn inspect_path(path: &Path) -> Result<crate::image_import::Info, String> {
    use std::io::{BufReader, Read, SeekFrom};
    if !path.is_absolute() {
        return Err("Use an absolute local image path".into());
    }
    let file = crate::file_safety::open_native_read(path).map_err(|e| format!("Cannot read image: {e}"))?;
    let length = file.metadata().map_err(|e| e.to_string())?.len();
    if length > NATIVE_OPEN_LIMITS.encoded_bytes {
        return Err("Encoded PNG image exceeds 3 GiB".into());
    }
    let mut input = BufReader::with_capacity(65536, file);
    let mut header = [0; 33];
    input.read_exact(&mut header).map_err(|e| e.to_string())?;
    let mut decoder = png::Decoder::new(Cursor::new(header));
    let info = decoder.read_header_info().map_err(|e| e.to_string())?;
    input.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let ((width, height, _), frames) = container_stream_animation(
        &mut input, length, NATIVE_OPEN_LIMITS.edge, NATIVE_OPEN_LIMITS.pixels,
        ColorPolicy::ImportSource, true, &mut || Ok(()),
    )?;
    if (width, height) != (info.width, info.height) {
        return Err("PNG header changed during inspection".into());
    }
    Ok(crate::image_import::Info {
        format: "Png".into(), width, height,
        bit_depth: if info.bit_depth == png::BitDepth::Sixteen { 16 } else { 8 },
        choice: frames.map(|_| "frame"), count: frames.unwrap_or(1) as usize,
    })
}

pub(crate) fn open_frame_count(
    file: &mut std::fs::File,
    version: crate::file_safety::Version,
    pool: &crate::large_document::ResidentBudget,
    check: &mut dyn FnMut() -> Result<(), String>,
) -> Result<Option<u32>, String> {
    use ring::digest::{Context, SHA256};
    use std::io::{BufReader, Seek, SeekFrom};
    struct Hashing<'a>(&'a mut std::fs::File, &'a mut Context);
    impl std::io::Read for Hashing<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let read = self.0.read(output)?;
            self.1.update(&output[..read]);
            Ok(read)
        }
    }
    if version.bytes > NATIVE_OPEN_LIMITS.encoded_bytes
        || file.metadata().map_err(|e| e.to_string())?.len() != version.bytes
    {
        return Err("PNG source bytes changed or exceed admission".into());
    }
    let _buffer = pool
        .reserve_bytes(65536, 65536)
        .map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut hash = Context::new(&SHA256);
    let (_, frames) = container_stream_animation(
        &mut BufReader::with_capacity(65536, Hashing(file, &mut hash)),
        version.bytes,
        NATIVE_OPEN_LIMITS.edge,
        NATIVE_OPEN_LIMITS.pixels,
        ColorPolicy::ImportSource,
        true,
        check,
    )?;
    if crate::file_safety::finish_fingerprint(hash) != version.sha256() {
        return Err("PNG inspection differs from its admitted open source".into());
    }
    Ok(frames)
}

pub(crate) fn prepare_open(
    base: &NativeDocument,
    session: &SourceSession,
    path: &Path,
    held: (std::fs::File, [u8; 32]),
    command: &serde_json::Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let input = FileSource::open_held(session, path, NATIVE_OPEN_LIMITS, "png", held, &mut || check())?;
    prepare_file(base, session, input, command, check)
}

pub(crate) struct LayerInspection {
    pub frames: Option<u32>,
    pub digest: [u8; 32],
}

pub(crate) fn inspect_layer(
    session: &SourceSession,
    path: &Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<LayerInspection, String> {
    let mut input = FileSource::open(session, path, NATIVE_OPEN_LIMITS, "png", &mut || check())?;
    let (_, frames) = inspect_file(&mut input, check)?;
    Ok(LayerInspection { frames, digest: input.source_binding()?.source_sha256() })
}

fn inspect_file(
    input: &mut FileSource<'_>,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<((u32, u32, bool), Option<u32>), String> {
    input.start_read()?;
    let length = input.length();
    let (facts, frames) = container_stream_animation(
        input,
        length,
        NATIVE_OPEN_LIMITS.edge,
        NATIVE_OPEN_LIMITS.pixels,
        ColorPolicy::ImportSource,
        true,
        &mut || check(),
    )?;
    input.admit(&mut || check())?;
    Ok((facts, frames))
}

fn prepare_file(
    base: &NativeDocument,
    session: &SourceSession,
    mut input: FileSource<'_>,
    command: &serde_json::Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let (facts, frames) = inspect_file(&mut input, check)?;
    let selected = selection(command, frames)?;
    let binding = input.source_binding()?;
    if crate::image_import::selected_digest(command)?.is_some_and(|expected| expected != binding.source_sha256()) {
        return Err("PNG changed after import inspection; choose the source again".into());
    }
    input.start_read()?;
    let output = prepare_input(
        base,
        session,
        input.decoder_stream(),
        binding,
        facts,
        frames,
        selected,
        true,
        check,
    )?;
    if let Err(reason) = input.finish(&mut || check()).and_then(|()| check()) {
        return retire(Err(reason), output, session);
    }
    Ok(output)
}

pub(crate) fn prepare_encoded(
    base: &NativeDocument,
    session: &SourceSession,
    encoded: &EncodedPng,
    command: &serde_json::Value,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let binding = encoded.binding(base.budget())?;
    let (facts, frames) = container_stream_animation(
        &mut Cursor::new(encoded.bytes()),
        encoded.bytes().len() as u64,
        Limits::NATIVE_COMMAND.edge,
        Limits::NATIVE_COMMAND.pixels,
        ColorPolicy::ImportSource,
        true,
        &mut || check(),
    )?;
    if frames.is_some() {
        check_animation(facts.0, facts.1, encoded.bytes().len() as u64, INLINE_ANIMATION_LIMITS)?;
    }
    prepare_input(
        base,
        session,
        Cursor::new(encoded.bytes()),
        binding,
        facts,
        frames,
        selection(command, frames)?,
        frames.is_none(),
        check,
    )
}

pub(crate) fn prepare_clipboard(
    base: &NativeDocument,
    session: &SourceSession,
    bytes: &[u8],
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let facts = inspect_clipboard(bytes, &mut || check())?;
    let binding = SourceBinding {
        source: sha(bytes),
        transport: None,
        pool: base.budget().clone(),
    };
    check()?;
    prepare_input(
        base, session, Cursor::new(bytes), binding, facts, None, 0, true, check,
    )
}

fn selection(command: &serde_json::Value, frames: Option<u32>) -> Result<u64, String> {
    let selected = command
        .get("frame")
        .map(|v| {
            v.as_u64()
                .filter(|n| *n < u64::from(frames.unwrap_or(1)))
                .ok_or("Image selection index is outside the available items")
        })
        .transpose()?;
    if frames.is_some() && selected.is_none() {
        return Err("Choose an explicit frame index before importing this PNG animation".into());
    }
    let selected = selected.unwrap_or(0);
    Ok(selected)
}

fn prepare_input(
    base: &NativeDocument,
    session: &SourceSession,
    mut input: impl BufRead + Seek,
    binding: SourceBinding,
    (width, height, declared_profile): (u32, u32, bool),
    frames: Option<u32>,
    selected: u64,
    native_tiles: bool,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    let _decoder = base
        .budget()
        .reserve_bytes(DECODER_BYTES, DECODER_BYTES)
        .map_err(|e| e.to_string())?;
    let mut previous: Option<StagedOutput> = None;
    let result = (|| {
        let mut callback = || {
            check()?;
            session.check_pinned_work()
        };
        let cancellation = RefCell::new(&mut callback as &mut dyn FnMut() -> Result<(), String>);
        let mut decoder = png::Decoder::new_with_limits(
            Checked {
                input: &mut input,
                check: &cancellation,
            },
            png::Limits {
                bytes: DECODER_LIMIT,
            },
        );
        decoder.ignore_checksums(false);
        decoder.set_ignore_text_chunk(true);
        decoder.set_transformations(png::Transformations::EXPAND);
        let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
        if (reader.info().width, reader.info().height) != (width, height)
            || reader.info().animation_control.map(|a| a.num_frames) != frames
            || (declared_profile && reader.info().icc_profile.is_none())
        {
            return Err("PNG animation header or declared profile differs from admission".into());
        }
        let canvas_pixels = u64::from(width) * u64::from(height);
        let mut work = crate::transform::native::Work::new(if native_tiles && frames.is_some() {
            // Admit a separate default image and two full frame compositions.
            128_000_000.max(5 * canvas_pixels)
        } else if native_tiles {
            2 * NATIVE_OPEN_LIMITS.pixels
        } else {
            // Animation retains its existing transport/work envelope.
            crate::image_import::check_budget(
                width,
                height,
                if reader.info().bit_depth == png::BitDepth::Sixteen { 16 } else { 8 },
                crate::image_import::BUDGET,
            )?;
            128_000_000
        })?;
        if frames.is_some() && reader.info().frame_control.is_none() {
            // Decode and validate the non-animation default image without retaining it.
            work.claim(canvas_pixels)?;
            decode_frame(
                &mut reader,
                width,
                height,
                binding.clone(),
                ColorPolicy::ImportSource,
                base.budget(),
                &mut || check(),
                |_, _, _| Ok(()),
            )?;
            reader.next_frame_info().map_err(|e| e.to_string())?;
        }
        for index in 0..=selected {
            check()?;
            if index != 0 {
                reader.next_frame_info().map_err(|e| e.to_string())?;
            }
            let frame = reader.info().frame_control;
            let (fw, fh) = frame.map_or((width, height), |f| (f.width, f.height));
            let discarded = frame.filter(|f| index != selected && f.dispose_op != png::DisposeOp::None);
            let clear = discarded.is_some_and(|f| f.dispose_op == png::DisposeOp::Background);
            let clear_partial = clear && previous.is_some() && discarded.is_some_and(|f| {
                (f.x_offset, f.y_offset, fw, fh) != (0, 0, width, height)
            });
            let composition = if discarded.is_none() || clear_partial { canvas_pixels } else { 0 };
            work.claim(u64::from(fw) * u64::from(fh) + composition)
                .map_err(|_| "PNG selected-frame decoding exceeds native work admission")?;
            if let Some(frame) = discarded {
                // Validate the original compressed rows even when their pixels will be discarded.
                decode_frame(
                    &mut reader, fw, fh, binding.clone(), ColorPolicy::ImportSource, base.budget(),
                    &mut || session.check().and_then(|()| check()),
                    |_, _, _| Ok(()),
                )?;
                if let Some(old) = previous.take_if(|_| clear) {
                    if clear_partial {
                        let candidate = composite(
                            base, session, (width, height), frame, None, Some(old.surface()), check,
                        );
                        previous = Some(retire(candidate, old, session)?);
                    } else {
                        old.abort(session, &|| session.cleanup_check())?;
                    }
                }
                continue;
            }
            let raw = png_admission::stage_with(
                session,
                binding.clone(),
                ColorPolicy::ImportSource,
                &mut || check(),
                |guard, row| {
                    decode_frame(
                        &mut reader,
                        fw,
                        fh,
                        binding.clone(),
                        ColorPolicy::ImportSource,
                        base.budget(),
                        guard,
                        row,
                    )
                },
            )?;
            let completed = if index == selected {
                (|| {
                    reader.finish().map_err(|e| e.to_string())?;
                    validate_metadata(reader.info(), raw.complete())
                })()
            } else {
                Ok(())
            };
            if frames.is_none() && frame.is_none() && completed.is_ok() && raw.is_ready_for_import() {
                return raw.into_unchanged_output(base, session, check);
            }
            let candidate = (|| {
                completed?;
                let composed = if let Some(frame) = frame {
                    composite(
                        base,
                        session,
                        (width, height),
                        frame,
                        Some(raw.surface()),
                        previous.as_ref().map(StagedOutput::surface),
                        check,
                    )?
                } else {
                    return native::prepare(base, session, &raw, check);
                };
                if index == selected && !raw.is_ready_for_import() {
                    let result = native::normalize(
                        base,
                        session,
                        composed.surface(),
                        raw.complete().profile(),
                        raw.complete().header().orientation,
                        check,
                    );
                    retire(result, composed, session)
                } else {
                    Ok(composed)
                }
            })();
            let cleanup = raw.abort(session);
            let candidate = match (candidate, cleanup) {
                (result, Ok(())) => result?,
                (Err(reason), Err(cleanup)) => {
                    return Err(format!(
                        "{reason}; APNG frame retirement refused: {cleanup}"
                    ))
                }
                (Ok(candidate), Err(reason)) => {
                    return retire(Err(reason), candidate, session);
                }
            };
            previous = Some(if let Some(old) = previous.take() {
                retire(Ok(candidate), old, session)?
            } else {
                candidate
            });
        }
        drop(reader);
        check()?;
        previous
            .take()
            .ok_or_else(|| "PNG selected frame has no output".into())
    })();
    if let Some(previous) = previous {
        retire(result, previous, session)
    } else {
        result
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::engine::native::worker::tests::Fixture;
    use std::io::Write;

    #[test]
    fn inline_animation_keeps_its_transport_envelope() {
        let limits = INLINE_ANIMATION_LIMITS;
        assert!(check_animation(8192, 4097, limits.encoded_bytes, limits).is_ok());
        assert!(check_animation(8193, 1, 1024, limits).is_err());
        assert!(check_animation(1, 1, limits.encoded_bytes + 1, limits).is_err());
    }

    #[test]
    fn wide_held_animation_uses_native_tiles_and_keeps_exact_words() {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(96 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let check = || Ok(());
        let base = NativeDocument::new_blank(4, 4, 8, &runtime.resident(), &runtime.disk(), &check).unwrap();
        let width = crate::raster::NATIVE_EDGE;
        let under = [12347u16, 23459, 34571, 65535];
        let over = [50331u16, 41011, 11473, 32768];
        let mut words = vec![0u16; width as usize * 4];
        // More distinct old tiles than the store can pin in one full-width row.
        for tile in 0..10 {
            let x = tile * TILE + tile;
            words[x as usize * 4..x as usize * 4 + 4].copy_from_slice(&under);
        }
        words[(width as usize - 2) * 4..(width as usize - 1) * 4].copy_from_slice(&under);
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, width, 1);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Sixteen);
            encoder.set_animated(4, 0).unwrap();
            let mut writer = encoder.write_header().unwrap();
            let first: Vec<u8> = words.into_iter().flat_map(u16::to_be_bytes).collect();
            writer.write_image_data(&first).unwrap();
            // Each of these discarded frames has more unique tiles than the store allows.
            for (frame_width, disposal) in [(width, png::DisposeOp::Previous), (width - 2, png::DisposeOp::Background)] {
                writer.set_frame_dimension(frame_width, 1).unwrap();
                writer.set_dispose_op(disposal).unwrap();
                let pixels: Vec<u8> = (0..frame_width).flat_map(|x| {
                    let words = if x % TILE == x / TILE { under } else { over };
                    words.into_iter().flat_map(u16::to_be_bytes)
                }).collect();
                writer.write_image_data(&pixels).unwrap();
            }
            writer.set_frame_dimension(2, 1).unwrap();
            writer.set_frame_position(width - 2, 0).unwrap();
            writer.set_blend_op(png::BlendOp::Over).unwrap();
            writer.write_image_data(&over.map(u16::to_be_bytes).concat().repeat(2)).unwrap();
            writer.finish().unwrap();
        }
        let path = fixture.path().join("Wide animation.png");
        std::fs::write(&path, bytes).unwrap();
        let info = inspect_path(&path).unwrap();
        assert_eq!((info.width, info.height, info.bit_depth, info.choice, info.count), (width, 1, 16, Some("frame"), 4));
        let inspection = inspect_layer(&session, &path, &check).unwrap();
        assert_eq!(inspection.frames, Some(4));
        let mut file = crate::file_safety::open_native_read(&path).unwrap();
        let version = crate::file_safety::native_source_version(&path, &mut file, &mut [0; 65536], &check).unwrap();
        assert_eq!(version.sha256(), inspection.digest);
        assert_eq!(open_frame_count(&mut file, version, base.budget(), &mut || Ok(())).unwrap(), Some(4));
        let disk = runtime.disk().used().unwrap();
        let options = serde_json::json!({"frame": 3});
        for output in [
            prepare_open(&base, &session, &path, (file, version.sha256()), &options, &check).unwrap(),
            prepare(&base, &session, &path, &options, &check).unwrap(),
        ] {
            let surface = output.surface();
            assert_eq!((surface.width(), surface.height(), surface.depth()), (width, 1, 16));
            let cleared = surface.prepare_region(Region::new([0, 0, 1, 1]).unwrap(), &mut || Ok(())).unwrap();
            assert_eq!(cleared.pixel16(0, 0).unwrap(), [0; 4]);
            drop(cleared);
            let right = width as i32;
            let region = Region::new([right - 2, 0, right, 1]).unwrap();
            let view = surface.prepare_region(region, &mut || Ok(())).unwrap();
            assert_eq!(view.pixel16(right - 2, 0).unwrap(), [31339, 32235, 23022, 65535]);
            assert_eq!(view.pixel16(right - 1, 0).unwrap(), over);
            drop(view);
            output.abort(&session, &|| session.cleanup_check()).unwrap();
        }
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }

    pub(crate) fn fixture(interlaced: bool, hidden: bool, depth: u8, color: u8) -> Vec<u8> {
        use super::super::tests::chunk;
        let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut header = Vec::new();
        header.extend_from_slice(&270u32.to_be_bytes());
        header.extend_from_slice(&9u32.to_be_bytes());
        header.extend_from_slice(&[depth, color, 0, 0, u8::from(interlaced)]);
        chunk(&mut bytes, b"IHDR", &header);
        if color == 3 {
            chunk(
                &mut bytes,
                b"PLTE",
                &[17, 29, 43, 91, 113, 139, 227, 181, 149, 59, 73, 83],
            );
            chunk(&mut bytes, b"tRNS", &[0, 31, 127, 255]);
        }
        chunk(&mut bytes, b"acTL", &[0, 0, 0, 5, 0, 0, 0, 0]);
        let mut sequence = 0u32;
        let frames = [
            (270u32, 9u32, 0u32, 0u32, 0u8, 0u8),
            (263, 7, 2, 1, 1, 1),
            (260, 5, 3, 2, 2, 0),
            (9, 4, 0, 0, 2, 1),
            (2, 3, 268, 6, 0, 1),
        ];
        for index in 0..5 + usize::from(hidden) {
            let default = hidden && index == 0;
            let frame_index = index.saturating_sub(usize::from(hidden));
            let (width, height, x, y, dispose, blend) = if default {
                frames[0]
            } else {
                frames[frame_index]
            };
            if !default {
                let mut control = Vec::new();
                for value in [sequence, width, height, x, y] {
                    control.extend_from_slice(&value.to_be_bytes());
                }
                sequence += 1;
                control.extend_from_slice(&[0, 1, 0, 10, dispose, blend]);
                chunk(&mut bytes, b"fcTL", &control);
            }
            let passes: &[(u32, u32, usize, usize)] = if interlaced {
                &[
                    (0, 0, 8, 8),
                    (4, 0, 8, 8),
                    (0, 4, 4, 8),
                    (2, 0, 4, 4),
                    (0, 2, 2, 4),
                    (1, 0, 2, 2),
                    (0, 1, 1, 2),
                ]
            } else {
                &[(0, 0, 1, 1)]
            };
            let mut scan = Vec::new();
            for &(sx, sy, dx, dy) in passes {
                if sx >= width {
                    continue;
                }
                for y in (sy..height).step_by(dy) {
                    scan.push(0);
                    for x in (sx..width).step_by(dx) {
                        let n = (x + y * 3 + index as u32) as u8;
                        let rgb = [n.wrapping_mul(17), n.wrapping_add(91), n.wrapping_mul(31)];
                        let alpha = [0, 31, 127, 255][n as usize % 4];
                        let values: Vec<u8> = match color {
                            0 => vec![rgb[0]],
                            2 => rgb.to_vec(),
                            3 => vec![n % 4],
                            4 => vec![rgb[0], alpha],
                            6 => vec![rgb[0], rgb[1], rgb[2], alpha],
                            _ => unreachable!(),
                        };
                        for value in values {
                            scan.push(value);
                            if depth == 16 {
                                scan.push(value.wrapping_add(37));
                            }
                        }
                    }
                }
            }
            let mut compressed =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            compressed.write_all(&scan).unwrap();
            let compressed = compressed.finish().unwrap();
            if index == 0 {
                chunk(&mut bytes, b"IDAT", &compressed);
            } else {
                let mut data = sequence.to_be_bytes().to_vec();
                sequence += 1;
                data.extend_from_slice(&compressed);
                chunk(&mut bytes, b"fdAT", &data);
            }
        }
        chunk(&mut bytes, b"IEND", &[]);
        bytes
    }

    pub(crate) fn profiled() -> Vec<u8> {
        let source = fixture(true, true, 8, 6);
        let metadata = super::super::tests::fixture(
            png::ColorType::Rgba,
            png::BitDepth::Eight,
            Some(moxcms::ColorProfile::new_display_p3().encode().unwrap()),
            6,
        );
        let mut bytes = source[..33].to_vec();
        let mut at = 33;
        while at < metadata.len() {
            let size = u32::from_be_bytes(metadata[at..at + 4].try_into().unwrap()) as usize + 12;
            if matches!(&metadata[at + 4..at + 8], b"iCCP" | b"eXIf") {
                bytes.extend_from_slice(&metadata[at..at + size]);
            }
            at += size;
        }
        bytes.extend_from_slice(&source[33..]);
        bytes
    }

    pub(crate) fn single() -> Vec<u8> {
        use super::super::tests::chunk;
        let source = fixture(false, false, 8, 6);
        let mut bytes = source[..8].to_vec();
        let mut controls = 0;
        let mut at = 8;
        while at < source.len() {
            let size = u32::from_be_bytes(source[at..at + 4].try_into().unwrap()) as usize + 12;
            let kind = &source[at + 4..at + 8];
            if kind == b"fcTL" {
                controls += 1;
                if controls == 2 {
                    break;
                }
            }
            if kind == b"acTL" {
                chunk(&mut bytes, b"acTL", &[0, 0, 0, 1, 0, 0, 0, 0]);
            } else {
                bytes.extend_from_slice(&source[at..at + size]);
            }
            at += size;
        }
        chunk(&mut bytes, b"IEND", &[]);
        bytes
    }

    #[test]
    fn previous_disposal_does_not_restore_pixels_cleared_by_background() {
        for interlaced in [false, true] {
            for hidden in [false, true] {
                let image = resident(
                    &fixture(interlaced, hidden, 8, 6),
                    3,
                    &crate::loading::Control::default(),
                )
                .unwrap();
                assert_eq!(image.as_rgba8().unwrap().get_pixel(10, 3).0, [0; 4]);
            }
        }
    }

    #[test]
    fn cancelled_and_refused_animation_staging_retires_all_owned_frames() {
        let fixture_root = Fixture::new();
        let (runtime, connection) =
            fixture_root.runtime_with_budgets(96 * 1024 * 1024, 128 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let base =
            NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let path = fixture_root.path().join("animation.png");
        std::fs::write(&path, fixture(true, true, 8, 6)).unwrap();
        let command = serde_json::json!({"frame":4});
        let disk = runtime.disk().used().unwrap();
        let resident = runtime.resident().used();
        let hold = runtime
            .resident()
            .reserve_bytes(40 * 1024 * 1024 - resident, 64 * 1024 * 1024)
            .unwrap();
        let hold2 = runtime
            .resident()
            .reserve_bytes(40 * 1024 * 1024, 64 * 1024 * 1024)
            .unwrap();
        assert!(prepare(&base, &session, &path, &command, &|| Ok(())).is_err());
        drop((hold, hold2));
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let reason = prepare(&base, &session, &path, &command, &|| {
            if runtime.disk().used().unwrap() > disk {
                Err("APNG import interrupted".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(reason.contains("APNG import interrupted"), "{reason}");
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let original = fixture(false, false, 8, 6);
        let mut at = 8;
        let mut frame_data = Vec::new();
        while at < original.len() {
            let size = u32::from_be_bytes(original[at..at + 4].try_into().unwrap()) as usize;
            if &original[at + 4..at + 8] == b"fdAT" {
                frame_data.push((at, size));
            }
            at += size + 12;
        }
        // Corrupt a discarded Previous frame and the selected frame independently.
        for (at, size) in [frame_data[1], *frame_data.last().unwrap()] {
            let mut data = original[at + 8..at + 8 + size].to_vec();
            data[4] = 0; // Keep the sequence and rebuild the chunk CRC.
            let mut bad = original[..at].to_vec();
            super::super::tests::chunk(&mut bad, b"fdAT", &data);
            bad.extend_from_slice(&original[at + size + 12..]);
            std::fs::write(&path, bad).unwrap();
            assert!(prepare(&base, &session, &path, &command, &|| Ok(())).is_err());
            assert_eq!(runtime.resident().used(), resident);
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        std::fs::write(&path, fixture(true, true, 8, 6)).unwrap();
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
        assert_eq!(runtime.resident().used(), resident);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        drop((session, base, connection, runtime));
    }

    #[test]
    fn animation_admission_rejects_wrong_sequences_counts_bounds_crc_and_precision() {
        use super::super::tests::chunk;
        let valid = fixture(false, false, 8, 6);
        let inspect = |bytes: &[u8]| {
            container_stream_animation(
                &mut Cursor::new(bytes),
                bytes.len() as u64,
                8192,
                8192 * 8192,
                ColorPolicy::ImportSource,
                true,
                &mut || Ok(()),
            )
        };
        assert!(inspect(&valid).is_ok());
        let single = single();
        assert_eq!(inspect(&single).unwrap().1, Some(1));
        assert!(selection(&serde_json::json!({}), Some(1)).is_err());
        assert_eq!(
            selection(&serde_json::json!({"frame":0}), Some(1)).unwrap(),
            0
        );
        for kind in [b"acTL", b"fcTL", b"fdAT"] {
            let mut at = 8;
            while &valid[at + 4..at + 8] != kind {
                at += u32::from_be_bytes(valid[at..at + 4].try_into().unwrap()) as usize + 12;
            }
            let length = u32::from_be_bytes(valid[at..at + 4].try_into().unwrap()) as usize;
            let mut data = valid[at + 8..at + 8 + length].to_vec();
            data[0] = 255;
            let mut changed = valid[..at].to_vec();
            chunk(&mut changed, kind, &data);
            changed.extend_from_slice(&valid[at + length + 12..]);
            assert!(inspect(&changed).is_err());
        }
        let mut corrupt = valid.clone();
        corrupt[32] ^= 1;
        assert!(inspect(&corrupt).is_err());
        assert!(inspect(&fixture(false, false, 16, 6)).is_ok());
        for end in [0, 8, 32, valid.len() - 1] {
            assert!(inspect(&valid[..end]).is_err());
        }
    }
}
