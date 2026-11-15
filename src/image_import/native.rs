//! Normalize admitted image sources before document insertion. No live mutation.
use super::{
    png_admission::StagedPng,
    prepared_png::{apng, ColorPolicy},
    source_file::FileSource,
};
use crate::{
    color_profile::prepared::{ChargedWords, UncachedTransforms},
    engine::{
        native::{Cancellation, NativeDocument, StagedOutput},
        scene::Source36,
    },
    large_document::{ScratchBytes, Store},
    psd::memory36::Admitted,
    raster::{backed::Region, Surface, TILE},
    selection::{NativeRows as Rows, NativeTileWriter as Writer},
    workspace::source_runtime::SourceSession,
};
use image::metadata::Orientation;
use std::{fs::File, io::{BufRead, BufReader}, path::Path};

#[derive(Clone, Copy)]
pub(crate) enum OpenFormat {
    Png,
    Jpeg,
    Jxl,
    Bmp,
    Gif,
    Webp,
    Tiff,
    Ico,
    Pnm,
    Qoi,
    Tga,
    Svg,
    Svgz,
}
impl OpenFormat {
    pub(crate) fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        [
            ("png", Self::Png),
            ("jpg", Self::Jpeg),
            ("jpeg", Self::Jpeg),
            ("jxl", Self::Jxl),
            ("bmp", Self::Bmp),
            ("gif", Self::Gif),
            ("webp", Self::Webp),
            ("tif", Self::Tiff),
            ("tiff", Self::Tiff),
            ("ico", Self::Ico),
            ("qoi", Self::Qoi),
            ("tga", Self::Tga),
            ("svg", Self::Svg),
            ("svgz", Self::Svgz),
        ]
        .into_iter()
        .find_map(|(suffix, format)| extension.eq_ignore_ascii_case(suffix).then_some(format))
        .or_else(|| super::pnm::matches(path).then_some(Self::Pnm))
    }

    pub(crate) fn recognizes(self, prefix: &[u8]) -> bool {
        use image::ImageFormat;
        let actual = image::guess_format(prefix).ok();
        match self {
            Self::Png => actual == Some(ImageFormat::Png),
            Self::Jpeg => actual == Some(ImageFormat::Jpeg),
            Self::Jxl => crate::jpeg_xl::signature(prefix),
            Self::Bmp => actual == Some(ImageFormat::Bmp),
            Self::Gif => actual == Some(ImageFormat::Gif),
            Self::Webp => actual == Some(ImageFormat::WebP),
            Self::Tiff => {
                actual == Some(ImageFormat::Tiff)
                    || prefix.starts_with(b"II+\0")
                    || prefix.starts_with(b"MM\0+")
            }
            Self::Ico => actual == Some(ImageFormat::Ico),
            Self::Pnm => actual == Some(ImageFormat::Pnm),
            Self::Qoi => actual == Some(ImageFormat::Qoi),
            Self::Tga | Self::Svg => actual.is_none(),
            Self::Svgz => prefix.starts_with(&[0x1f, 0x8b]),
        }
    }

    pub(crate) fn encoded_limit(self) -> u64 {
        match self {
            Self::Png => apng::NATIVE_OPEN_LIMITS.encoded_bytes,
            Self::Bmp => super::bmp::FILE_LIMITS.encoded_bytes,
            Self::Jpeg => super::jpeg::FILE_LIMITS.encoded_bytes,
            Self::Pnm => super::pnm::FILE_LIMITS.encoded_bytes,
            Self::Tiff => super::tiff::FILE_LIMITS.encoded_bytes,
            Self::Tga => super::tga::FILE_LIMITS.encoded_bytes,
            Self::Webp => super::webp::FILE_LIMITS.encoded_bytes,
            Self::Jxl => super::jxl::FILE_LIMITS.encoded_bytes,
            Self::Svg => crate::svg::MARKUP_LIMIT as u64,
            Self::Svgz => 1024 * 1024,
            _ => super::ENCODED_LIMIT as u64,
        }
    }

    pub(crate) fn choice_key(self) -> Option<&'static str> {
        match self {
            Self::Png | Self::Gif | Self::Webp => Some("frame"),
            Self::Tiff => Some("page"),
            Self::Ico => Some("variant"),
            _ => None,
        }
    }

    fn open_source<'a>(
        self,
        session: &'a SourceSession,
        path: &Path,
        held: (File, [u8; 32]),
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(FileSource<'a>, super::source_file::FileLimits), String> {
        let limits = self.file_limits();
        let extension = path
            .extension()
            .and_then(|ext| ext.to_str())
            .ok_or("Missing image extension")?;
        let input = FileSource::open_held(session, path, limits, extension, held, &mut || check())?;
        Ok((input, limits))
    }

    fn file_limits(self) -> super::source_file::FileLimits {
        match self {
            Self::Bmp => super::bmp::FILE_LIMITS,
            Self::Gif => super::gif::FILE_LIMITS,
            Self::Jpeg => super::jpeg::FILE_LIMITS,
            Self::Jxl => super::jxl::FILE_LIMITS,
            Self::Qoi => super::qoi::FILE_LIMITS,
            Self::Pnm => super::pnm::FILE_LIMITS,
            Self::Tiff => super::tiff::FILE_LIMITS,
            Self::Tga => super::tga::FILE_LIMITS,
            Self::Webp => super::webp::FILE_LIMITS,
            _ => super::source_file::FileLimits {
                encoded_bytes: self.encoded_limit(),
                ..apng::FALLBACK_OPEN_LIMITS
            },
        }
    }

    pub(crate) fn inspect_layer(
        self,
        session: &SourceSession,
        path: &Path,
        encoded_remaining: &mut u64,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(Option<(&'static str, usize)>, [u8; 32], Vec<[u16; 2]>), String> {
        if matches!(self, Self::Png) {
            return Err("PNG layers require their dedicated native admission".into());
        }
        let mut limits = self.file_limits();
        // These held, streamed sources use their own per-file byte envelopes.
        let shared_batch = !matches!(self, Self::Bmp | Self::Jpeg | Self::Jxl | Self::Pnm | Self::Tga | Self::Tiff | Self::Webp);
        if shared_batch {
            if *encoded_remaining == 0 {
                return Err("Encoded image batch exceeds 128 MiB".into());
            }
            limits.encoded_bytes = limits.encoded_bytes.min(*encoded_remaining);
        }
        let extension = path.extension().and_then(|ext| ext.to_str()).ok_or("Missing image extension")?;
        let mut input = FileSource::open(session, path, limits, extension, &mut || check())?;
        let (choice, sizes) = if let Some(key) = self.choice_key() {
            let (count, sizes) = self.inspect_choices(session, &mut input, Some(encoded_remaining), check)?;
            ((count > 1).then_some((key, count)), sizes)
        } else {
            input.start_read()?;
            let header = input.fill_buf().map_err(|err| err.to_string())?;
            if !self.recognizes(&header[..header.len().min(16)]) {
                return Err("Image signature differs from the selected format".into());
            }
            if matches!(self, Self::Tga) {
                super::tga::check_header(&mut input)?;
            }
            input.admit(&mut || check())?;
            (None, Vec::new())
        };
        if shared_batch {
            *encoded_remaining -= input.length();
        }
        Ok((choice, input.source_binding()?.source_sha256(), sizes))
    }

    /// Return chooser metadata bound to the held source digest.
    pub(crate) fn choices(
        self,
        session: &SourceSession,
        path: &Path,
        held: (File, [u8; 32]),
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(usize, Vec<[u16; 2]>), String> {
        let (mut input, _) = self.open_source(session, path, held, check)?;
        self.inspect_choices(session, &mut input, None, check)
    }

    fn inspect_choices(
        self,
        session: &SourceSession,
        input: &mut FileSource<'_>,
        encoded_remaining: Option<&mut u64>,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(usize, Vec<[u16; 2]>), String> {
        match self {
            Self::Gif | Self::Ico => {
                input.start_read()?;
                let choices = if matches!(self, Self::Gif) {
                    (super::gif::inspect_native(input, true, check)?.frames, Vec::new())
                } else {
                    let sizes = super::ico::variant_sizes(input)?;
                    (sizes.len(), sizes)
                };
                input.admit(&mut || check())?;
                Ok(choices)
            }
            Self::Webp => {
                let info = super::webp::inspect_held(input, check)?;
                if let Some(remaining) = encoded_remaining.filter(|_| info.native_file_limit() == super::ENCODED_LIMIT as u64) {
                    *remaining = remaining.checked_sub(input.length()).ok_or("Encoded image batch exceeds 128 MiB")?;
                }
                Ok((info.frames, Vec::new()))
            }
            Self::Tiff => {
                let _reader = session
                    .resident()
                    .reserve_bytes(65536, 65536)
                    .map_err(|e| e.to_string())?;
                let encoded = input.snapshot(&mut || check())?;
                let result = (|| {
                    let mut guard = || {
                        check()
                            .and_then(|()| encoded.check())
                            .map_err(|_| crate::large_document::Error::Cancelled)
                    };
                    let length = encoded.bytes().payload_length();
                    let cursor = encoded
                        .bytes()
                        .cursor(0, length, &mut guard)
                        .map_err(|e| e.to_string())?;
                    let mut reader = BufReader::with_capacity(65536, cursor);
                    Ok(super::tiff::Directory::read(&mut reader, length, check)?.offsets().len())
                })();
                match (result, encoded.retire(&mut || session.cleanup_check())) {
                    (Ok(count), Ok(())) => Ok((count, Vec::new())),
                    (Err(reason), Ok(())) => Err(reason),
                    (Ok(_), Err(cleanup)) => {
                        Err(format!("Image inspection staging retained: {cleanup}"))
                    }
                    (Err(reason), Err(cleanup)) => Err(format!(
                        "{reason}; image inspection staging retained: {cleanup}"
                    )),
                }
            }
            _ => Err("Image format has no multi-item inspection".into()),
        }
    }

    pub(crate) fn prepare(
        self,
        base: &NativeDocument,
        session: &SourceSession,
        path: &Path,
        held: (File, [u8; 32]),
        options: &serde_json::Value,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(StagedOutput, Option<Admitted<Source36>>), String> {
        if matches!(self, Self::Png) {
            return apng::prepare_open(base, session, path, held, options, check)
                .map(|output| (output, None));
        }
        let check = &|| {
            check()?;
            session.check()
        };
        let (input, limits) = self.open_source(session, path, held, check)?;
        self.prepare_source(base, session, input, limits, options, &Cancellation::default(), check)
    }

    pub(crate) fn prepare_pinned_layer(
        self,
        base: &NativeDocument,
        session: &SourceSession,
        path: &Path,
        expected: [u8; 32],
        options: &serde_json::Value,
        cancellation: &Cancellation,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(StagedOutput, Option<Admitted<Source36>>), String> {
        if matches!(self, Self::Png) {
            return Err("PNG layers require their dedicated native admission".into());
        }
        let limits = self.file_limits();
        let extension = path.extension().and_then(|ext| ext.to_str()).ok_or("Missing image extension")?;
        let input = FileSource::open_pinned(session, path, limits, extension, expected, &mut || check())?;
        self.prepare_source(base, session, input, limits, options, cancellation, check)
    }

    fn prepare_source(
        self,
        base: &NativeDocument,
        session: &SourceSession,
        input: FileSource<'_>,
        limits: super::source_file::FileLimits,
        options: &serde_json::Value,
        cancellation: &Cancellation,
        check: &dyn Fn() -> Result<(), String>,
    ) -> Result<(StagedOutput, Option<Admitted<Source36>>), String> {
        let raster = match self {
            Self::Jpeg => super::jpeg::prepare_source(base, session, input, true, check),
            Self::Jxl => super::jxl::prepare_source(base, session, input, check),
            Self::Bmp => super::bmp::prepare_source(base, session, input, check),
            Self::Gif => super::gif::prepare_source(base, session, input, Some(options), check),
            Self::Webp => super::webp::prepare_source(base, session, input, Some(options), check),
            Self::Tiff => super::tiff::prepare_source(base, session, input, Some(options), check),
            Self::Ico => {
                super::ico::prepare_source(base, session, input, limits, Some(options), check)
            }
            Self::Pnm => super::pnm::prepare_source(base, session, input, check),
            Self::Qoi => super::qoi::prepare_source(base, session, input, check),
            Self::Tga => super::tga::prepare_source(base, session, input, check),
            Self::Svg | Self::Svgz => {
                return super::svg::prepare_source(
                    base,
                    session,
                    input,
                    matches!(self, Self::Svgz),
                    cancellation,
                    check,
                )
                .map(|(output, source)| (output, Some(source)));
            }
            Self::Png => unreachable!("PNG uses its selected frame decoder"),
        }?;
        Ok((raster, None))
    }
}

/// Non-PNG file producers shared by import, placement and patch commands.
pub(crate) fn stream_file(
    base: &NativeDocument,
    session: &SourceSession,
    path: &std::path::Path,
    check: &dyn Fn() -> Result<(), String>,
) -> Option<Result<StagedOutput, String>> {
    let extension = path.extension()?.to_str()?;
    if super::pnm::matches(path) {
        Some(super::pnm::prepare(base, session, path, check))
    } else if super::jpeg::matches(path) {
        Some(super::jpeg::prepare(base, session, path, false, check))
    } else if extension.eq_ignore_ascii_case("jxl") {
        Some(super::jxl::prepare_patch(base, session, path, check))
    } else if super::tiff::matches(path) {
        Some(super::tiff::prepare(base, session, path, None, check))
    } else if extension.eq_ignore_ascii_case("qoi") {
        Some(super::qoi::prepare(base, session, path, check))
    } else if extension.eq_ignore_ascii_case("webp") {
        Some(super::webp::prepare(base, session, path, None, check))
    } else if extension.eq_ignore_ascii_case("tga") {
        Some(super::tga::prepare(base, session, path, check))
    } else if extension.eq_ignore_ascii_case("bmp") {
        Some(super::bmp::prepare(base, session, path, check))
    } else if extension.eq_ignore_ascii_case("ico") {
        Some(super::ico::prepare(base, session, path, None, check))
    } else {
        None
    }
}

/// Orientation is a private generation; retire its input before publication.
pub(super) fn orient_staged(
    base: &NativeDocument,
    session: &SourceSession,
    source: StagedOutput,
    orientation: Orientation,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    if orientation == Orientation::NoTransforms {
        return Ok(source);
    }
    let output = crate::retained::transaction::stage(base, session, check, |store, guard| {
        orient(source.surface(), orientation, store, &mut || guard()).map_err(|e| e.to_string())
    });
    match (output, source.abort(session, &|| session.cleanup_check())) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(reason), Ok(())) => Err(reason),
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; image source retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; oriented image retirement refused: {cleanup}"
            )),
        },
    }
}

pub(crate) fn prepare(
    base: &NativeDocument,
    session: &SourceSession,
    source: &StagedPng,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    check()?;
    if source.complete().color_policy() != ColorPolicy::ImportSource
        || !source.surface().uses_budget(base.budget())
        || !source.surface().uses_disk_budget(base.disk_budget())
    {
        return Err("Native import requires color-validated source admission".into());
    }
    let profile = source.complete().profile();
    let orientation = source.complete().header().orientation;
    normalize(base, session, source.surface(), profile, orientation, check)
}

pub(super) fn normalize(
    base: &NativeDocument,
    session: &SourceSession,
    source: &Surface,
    profile: Option<&[u8]>,
    orientation: Orientation,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    if orientation == Orientation::NoTransforms {
        return crate::retained::transaction::stage(base, session, check, |store, guard| {
            convert(source, profile, store, &mut || guard())
        });
    }
    let oriented = crate::retained::transaction::stage(base, session, check, |store, guard| {
        orient(source, orientation, store, &mut || guard()).map_err(|e| e.to_string())
    })?;
    let output = crate::retained::transaction::stage(base, session, check, |store, guard| {
        convert(oriented.surface(), profile, store, &mut || guard())
    });
    match (output, oriented.abort(session, &|| session.cleanup_check())) {
        (Ok(output), Ok(())) => Ok(output),
        (Err(reason), Ok(())) => Err(reason),
        (Err(reason), Err(cleanup)) => Err(format!(
            "{reason}; oriented source retirement refused: {cleanup}"
        )),
        (Ok(output), Err(reason)) => match output.abort(session, &|| session.cleanup_check()) {
            Ok(()) => Err(reason),
            Err(cleanup) => Err(format!(
                "{reason}; converted source retirement refused: {cleanup}"
            )),
        },
    }
}

pub(super) fn prepare_raw(
    base: &NativeDocument,
    session: &SourceSession,
    source: &StagedPng,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<StagedOutput, String> {
    if source.complete().color_policy() != ColorPolicy::RawSelection
        || !source.surface().uses_budget(base.budget())
        || !source.surface().uses_disk_budget(base.disk_budget())
    {
        return Err("Native raw import requires admitted source ownership".into());
    }
    crate::retained::transaction::stage(base, session, check, |store, guard| {
        convert(source.surface(), None, store, &mut || guard())
    })
}

pub(crate) fn convert(
    surface: &Surface,
    profile: Option<&[u8]>,
    store: &Store,
    check: &mut dyn FnMut() -> crate::large_document::Result<()>,
) -> Result<Surface, String> {
    let pool = store.resident_budget();
    let conversion = profile
        .map(|profile| {
            UncachedTransforms::new(profile, &pool, &mut || check().map_err(|e| e.to_string()))
        })
        .transpose()?;
    let width = surface.width() as usize;
    let length = width * usize::from(surface.depth() / 8) * 4;
    let mut input = ScratchBytes::new(&pool, length, length).map_err(|e| e.to_string())?;
    let mut output = if conversion.is_some() && surface.depth() == 8 {
        Some(ScratchBytes::new(&pool, length, length).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let mut words = if conversion.is_some() && surface.depth() == 16 {
        Some((
            ChargedWords::new(width * 4, &pool)?,
            ChargedWords::new(width * 4, &pool)?,
        ))
    } else {
        None
    };
    let mut row = ScratchBytes::new(&pool, width * 8, width * 8).map_err(|e| e.to_string())?;
    let mut writer = Rows::new(
        surface.width(),
        surface.height(),
        surface.depth(),
        store.clone(),
    )
    .map_err(|e| e.to_string())?;
    for y in 0..surface.height() {
        check().map_err(|e| e.to_string())?;
        let region = Region::new([0, y as i32, surface.width() as i32, y as i32 + 1])
            .map_err(|e| e.to_string())?;
        let prepared = surface
            .prepare_region(region, check)
            .map_err(|e| e.to_string())?;
        prepared
            .copy_row(y as i32, &mut input)
            .map_err(|e| e.to_string())?;
        drop(prepared);
        // Imports share oriented full-row CMS grouping across storage backends.
        if surface.depth() == 8 {
            let pixels = if let Some(conversion) = &conversion {
                let output = output.as_mut().unwrap();
                conversion.convert8(&input, output)?;
                &**output
            } else {
                &*input
            };
            for (&sample, bytes) in pixels.iter().zip(row.chunks_exact_mut(2)) {
                bytes.copy_from_slice(&(u16::from(sample) * 257).to_be_bytes());
            }
        } else if let Some(conversion) = &conversion {
            let (native, converted) = words.as_mut().unwrap();
            for (word, bytes) in native.iter_mut().zip(input.chunks_exact(2)) {
                *word = u16::from_be_bytes([bytes[0], bytes[1]]);
            }
            conversion.convert16(native, converted)?;
            for (&word, bytes) in converted.iter().zip(row.chunks_exact_mut(2)) {
                bytes.copy_from_slice(&word.to_be_bytes());
            }
        } else {
            row.copy_from_slice(&input);
        }
        writer.row(y, &row, check).map_err(|e| e.to_string())?;
    }
    writer.finish(check).map_err(|e| e.to_string())
}

pub(super) fn orient(
    source: &Surface,
    orientation: Orientation,
    store: &Store,
    check: &mut dyn FnMut() -> crate::large_document::Result<()>,
) -> crate::large_document::Result<Surface> {
    let (w, h) = (source.width(), source.height());
    let rotated = matches!(
        orientation,
        Orientation::Rotate90
            | Orientation::Rotate270
            | Orientation::Rotate90FlipH
            | Orientation::Rotate270FlipH
    );
    let (width, height) = if rotated { (h, w) } else { (w, h) };
    let original = |x: u32, y: u32| match orientation {
        Orientation::NoTransforms => (x, y),
        Orientation::Rotate90 => (y, h - 1 - x),
        Orientation::Rotate180 => (w - 1 - x, h - 1 - y),
        Orientation::Rotate270 => (w - 1 - y, x),
        Orientation::FlipHorizontal => (w - 1 - x, y),
        Orientation::FlipVertical => (x, h - 1 - y),
        Orientation::Rotate90FlipH => (y, x),
        Orientation::Rotate270FlipH => (w - 1 - y, h - 1 - x),
    };
    let bpp = usize::from(source.depth() / 8) * 4;
    let mut writer = Writer::new(width, height, source.depth(), store.clone())?;
    let mut bytes = ScratchBytes::new(
        &store.resident_budget(),
        (TILE * TILE) as usize * bpp,
        (TILE * TILE) as usize * bpp,
    )?;
    for ty in 0..height.div_ceil(TILE) {
        for tx in 0..width.div_ceil(TILE) {
            check()?;
            bytes.fill(0);
            let (left, top) = (tx * TILE, ty * TILE);
            let (right, bottom) = ((left + TILE).min(width), (top + TILE).min(height));
            let a = original(left, top);
            let b = original(right - 1, bottom - 1);
            let region = Region::new([
                a.0.min(b.0) as i32,
                a.1.min(b.1) as i32,
                a.0.max(b.0) as i32 + 1,
                a.1.max(b.1) as i32 + 1,
            ])?;
            let prepared = source.prepare_region(region, check)?;
            for y in top..bottom {
                check()?;
                for x in left..right {
                    let (sx, sy) = original(x, y);
                    let pixel = prepared.pixel16(sx as i32, sy as i32)?;
                    let at = ((y - top) * TILE + x - left) as usize * bpp;
                    if source.depth() == 16 {
                        for (i, word) in pixel.into_iter().enumerate() {
                            bytes[at + i * 2..at + i * 2 + 2].copy_from_slice(&word.to_be_bytes());
                        }
                    } else {
                        bytes[at..at + 4].copy_from_slice(&pixel.map(crate::raster::project16));
                    }
                }
            }
            drop(prepared);
            writer.tile(tx, ty, &bytes, check)?;
        }
    }
    writer.finish(check)
}

#[cfg(test)]
#[path = "native_tests.rs"]
mod tests;
