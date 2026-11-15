//! Bounded, precision-preserving raster import shared by document commands and clipboard files.
use crate::raster::{check_size, Raster};
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{Cursor, Read},
    path::Path,
};

pub(crate) mod bmp;
pub(crate) mod gif;
pub(crate) mod ico;
pub(crate) mod jpeg;
pub(crate) mod jxl;
pub(crate) mod native;
pub(crate) mod placement;
#[path = "image_import/png_admission.rs"]
pub(crate) mod png_admission;
mod png_rows;
#[path = "image_import/png_surface.rs"]
pub(crate) mod png_surface;
pub(crate) mod pnm;
#[path = "image_import/prepared_png.rs"]
pub(crate) mod prepared_png;
pub(crate) mod qoi;
mod source_file;
pub(crate) mod svg;
pub(crate) mod tga;
pub(crate) mod tiff;
pub(crate) mod webp;

pub const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp", "jxl", "ico", "pbm", "pgm", "ppm",
    "pam", "pnm", "tga", "qoi", "svg", "svgz",
];
pub const BUDGET: usize = 256 * 1024 * 1024;
const ENCODED_LIMIT: usize = 128 * 1024 * 1024;
const MAX_ITEMS: usize = 1024;

pub fn limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(BUDGET as u64);
    limits
}

#[derive(Clone, Debug, Serialize)]
pub struct Info {
    pub format: String,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u16,
    /// Zero-based frame, page or icon variant. None denotes a single static image.
    pub choice: Option<&'static str>,
    pub count: usize,
}

#[derive(Clone, Copy, PartialEq)]
enum Codec {
    Image(ImageFormat),
    Jxl,
}

pub struct Encoded {
    bytes: Vec<u8>,
    format: Codec,
    pub info: Info,
    offsets: Vec<usize>,
    source: Option<crate::source::Source>,
}
pub struct Prepared {
    pub pixels: Raster,
    pub source: Option<crate::source::Source>,
}

pub(crate) fn selected_digest(command: &Value) -> Result<Option<[u8; 32]>, String> {
    let Some(value) = command.get("source_sha256") else { return Ok(None) };
    let bytes = value.as_array().filter(|bytes| bytes.len() == 32)
        .ok_or("source_sha256 must contain 32 bytes")?;
    let mut digest = [0; 32];
    for (target, value) in digest.iter_mut().zip(bytes) {
        *target = value.as_u64().and_then(|n| u8::try_from(n).ok())
            .ok_or("source_sha256 must contain 32 bytes")?;
    }
    Ok(Some(digest))
}

fn bytes_at(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], String> {
    bytes
        .get(at..at.checked_add(len).ok_or("Invalid image offset")?)
        .ok_or_else(|| "Truncated image structure".into())
}
fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}


fn png_frames(bytes: &[u8]) -> Result<(usize, bool), String> {
    let mut at = 8;
    let mut frames = 1;
    let mut animated = false;
    let mut managed = false;
    let mut nonstandard = false;
    while at < bytes.len() {
        let head = bytes_at(bytes, at, 8)?;
        let len = be32(&head[..4]) as usize;
        let data = bytes_at(bytes, at + 8, len)?;
        bytes_at(bytes, at + 8 + len, 4)?;
        match &head[4..] {
            b"acTL" => {
                if animated || len != 8 { return Err("Invalid PNG animation header".into()); }
                animated = true;
                frames = be32(&data[..4]) as usize;
                if frames == 0 || frames > MAX_ITEMS { return Err("PNG animation frame limit exceeded".into()); }
            }
            b"cICP" if data != [1,13,0,1] => return Err("HDR/non-sRGB PNG color encoding is not supported; convert explicitly before importing".into()),
            b"iCCP" | b"sRGB" => managed=true,
            b"gAMA" => {if len!=4 {return Err("Invalid PNG gamma chunk".into());} nonstandard|=be32(data)!=45455;}
            b"cHRM" => {
                if len!=32 {return Err("Invalid PNG chromaticity chunk".into());}
                nonstandard|=data.chunks_exact(4).map(be32).ne([31270,32900,64000,33000,30000,60000,15000,6000]);
            }
            b"IEND" => {
                if nonstandard && !managed {return Err("Non-sRGB PNG gamma/chromaticities need an explicit color conversion before importing".into());}
                return Ok((if animated { frames } else { 1 }, animated));
            }
            _ => {}
        }
        at += len + 12;
    }
    Err("PNG has no complete end chunk".into())
}

fn gif_frames(bytes: &[u8]) -> Result<usize, String> {
    Ok(gif::inspect(&mut Cursor::new(bytes), &|| Ok(()))?.frames)
}

fn webp_frames(bytes: &[u8]) -> Result<usize, String> {
    Ok(webp::inspect(&mut Cursor::new(bytes), bytes.len() as u64, true, &|| Ok(()))?.frames)
}

// Follow the IFD chain without decoding pages. Selecting a page changes only the
// first-IFD pointer in an owned byte buffer; offsets and the source file stay intact.
fn tiff_offsets(bytes: &[u8]) -> Result<Vec<usize>, String> {
    tiff::Directory::read(&mut Cursor::new(bytes), bytes.len() as u64, &|| Ok(()))?
        .offsets()
        .iter()
        .map(|&offset| usize::try_from(offset).map_err(|_| "TIFF offset exceeds address space".into()))
        .collect()
}

impl Encoded {
    pub(crate) fn icon_sizes(&self) -> Result<Vec<[u16; 2]>, String> {
        if self.info.choice != Some("variant") {
            return Ok(Vec::new());
        }
        ico::variant_sizes(&mut Cursor::new(&self.bytes))
    }
    fn jxl(bytes: Vec<u8>) -> Result<Self, String> {
        let info = crate::jpeg_xl::inspect(&bytes, &|| Ok(()))?;
        Ok(Self {
            bytes,
            format: Codec::Jxl,
            source: None,
            offsets: Vec::new(),
            info: Info {
                format: "JPEG XL".into(),
                width: info.width,
                height: info.height,
                bit_depth: info.bit_depth,
                choice: None,
                count: 1,
            },
        })
    }
    fn decode_jxl(&self, command: &Value, budget: usize) -> Result<Raster, String> {
        if ["frame", "page", "variant"]
            .iter()
            .any(|key| command.get(*key).is_some())
        {
            return Err("Static JPEG XL does not accept frame/page/variant choices".into());
        }
        check_budget(
            self.info.width,
            self.info.height,
            self.info.bit_depth,
            budget,
        )?;
        let decoded = crate::jpeg_xl::decode(&self.bytes, &|| Ok(()))?;
        if (
            decoded.info.width,
            decoded.info.height,
            decoded.info.bit_depth,
        ) != (self.info.width, self.info.height, self.info.bit_depth)
        {
            return Err("JPEG XL decoded metadata differs from its inspected source".into());
        }
        let info = decoded.info;
        match working_jxl_pixels(decoded, &|| Ok(()))? {
            crate::jpeg_xl::Pixels::Eight(pixels) => {
                Raster::from_rgba(info.width, info.height, &pixels)
            }
            crate::jpeg_xl::Pixels::Sixteen(pixels) => {
                Raster::from_rgba16(info.width, info.height, &pixels)
            }
        }
    }
    pub fn encoded_len(&self) -> usize {
        self.bytes.len()
    }
    pub fn file(path: &Path) -> Result<Self, String> {
        if !path.is_absolute() {
            return Err("Use an absolute local image path".into());
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !EXTENSIONS.contains(&ext.as_str()) {
            return Err("Unsupported image format. Import a supported raster image or self-contained static SVG; open PSD projects separately. PDF/AI/EPS require explicit supported export from the source application.".into());
        }
        let file = std::fs::File::open(path).map_err(|e| format!("Cannot read image: {e}"))?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        let encoded_limit = match ext.as_str() {
            "svg" => crate::svg::MARKUP_LIMIT,
            "svgz" => 1024 * 1024,
            _ => ENCODED_LIMIT,
        };
        if !metadata.is_file() || metadata.len() > encoded_limit as u64 {
            return Err(format!(
                "Encoded {ext} image must be a file at most {encoded_limit} bytes"
            ));
        }
        let mut bytes = Vec::new();
        file.take(encoded_limit as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > encoded_limit {
            return Err("Encoded image exceeds its raster/SVG import limit".into());
        }
        if ["svg", "svgz"].contains(&ext.as_str()) {
            let mut markup = Vec::new();
            if ext == "svgz" {
                flate2::read::GzDecoder::new(bytes.as_slice())
                    .take(crate::svg::MARKUP_LIMIT as u64 + 1)
                    .read_to_end(&mut markup)
                    .map_err(|e| format!("Invalid compressed SVG: {e}"))?;
            } else {
                markup = bytes.clone();
            }
            if markup.len() > crate::svg::MARKUP_LIMIT {
                return Err("SVG source exceeds 512 KiB".into());
            }
            let svg = String::from_utf8(markup).map_err(|_| "SVG import requires UTF-8 XML")?;
            let (width, height) = crate::svg::dimensions(&svg)?;
            let source = crate::source::Source {
                width,
                height,
                matrix: [1., 0., 0., 1., 0., 0.],
                content: crate::source::Content::Svg { svg },
            };
            return Ok(Self {
                bytes,
                format: Codec::Image(ImageFormat::Png),
                info: Info {
                    format: "SVG".into(),
                    width,
                    height,
                    bit_depth: 8,
                    choice: None,
                    count: 1,
                },
                offsets: Vec::new(),
                source: Some(source),
            });
        }
        if ext == "jxl" {
            return Self::jxl(bytes);
        }
        let guessed = image::guess_format(&bytes).ok();
        let expected = ImageFormat::from_extension(&ext).ok_or("Unsupported image extension")?;
        let format = guessed
            .or((expected == ImageFormat::Tga).then_some(expected))
            .ok_or("Cannot identify image contents")?;
        if format != expected {
            return Err("Image contents do not match its extension".into());
        }
        Self::new(bytes, format)
    }
    pub fn png(bytes: Vec<u8>) -> Result<Self, String> {
        if image::guess_format(&bytes).ok() != Some(ImageFormat::Png) {
            return Err("Expected PNG image bytes".into());
        }
        Self::new(bytes, ImageFormat::Png)
    }
    fn new(bytes: Vec<u8>, format: ImageFormat) -> Result<Self, String> {
        if bytes.len() > ENCODED_LIMIT {
            return Err("Encoded image exceeds 128 MiB".into());
        }
        if format == ImageFormat::Jpeg {
            jpeg::inspect(&bytes, &|| Ok(()))?;
        }
        if format == ImageFormat::Bmp {
            bmp::validate_dib(bytes.get(14..).ok_or("Truncated BMP header")?)?;
        }
        let offsets = if format == ImageFormat::Tiff {
            tiff_offsets(&bytes)?
        } else {
            Vec::new()
        };
        let png = if format == ImageFormat::Png {
            Some(png_frames(&bytes)?)
        } else {
            None
        };
        let count = match format {
            ImageFormat::Png => png.unwrap().0,
            ImageFormat::Gif => gif_frames(&bytes)?,
            ImageFormat::WebP => webp_frames(&bytes)?,
            ImageFormat::Tiff => offsets.len(),
            ImageFormat::Ico => {
                let header = bytes_at(&bytes, 0, 6)?;
                if header[..4] != [0, 0, 1, 0] {
                    return Err("Import icon images, not cursor files".into());
                }
                let n = u16::from_le_bytes(header[4..].try_into().unwrap()) as usize;
                if n == 0 || n > MAX_ITEMS {
                    return Err("Invalid icon variant count".into());
                }
                bytes_at(&bytes, 6, n * 16)?;
                n
            }
            _ => 1,
        };
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
        reader.limits(limits());
        let decoder = reader
            .into_decoder()
            .map_err(|e| format!("Cannot read image header: {e}"))?;
        let (width, height) = decoder.dimensions();
        check_size(width, height)?;
        validate_color(&decoder)?;
        let bit_depth = if decoder.color_type().bits_per_pixel()
            / decoder.color_type().channel_count() as u16
            > 8
        {
            16
        } else {
            8
        };
        let choice = if count > 1 || png.is_some_and(|p| p.1) {
            Some(match format {
                ImageFormat::Tiff => "page",
                ImageFormat::Ico => "variant",
                _ => "frame",
            })
        } else {
            None
        };
        drop(decoder);
        Ok(Self {
            bytes,
            format: Codec::Image(format),
            source: None,
            info: Info {
                format: format!("{format:?}"),
                width,
                height,
                bit_depth,
                choice,
                count,
            },
            offsets,
        })
    }
    pub fn decode(&self, command: &Value, budget: usize) -> Result<Raster, String> {
        self.decode_with_control(command, budget, &crate::loading::Control::default())
    }
    pub(crate) fn decode_with_control(
        &self,
        command: &Value,
        budget: usize,
        control: &crate::loading::Control,
    ) -> Result<Raster, String> {
        control.check()?;
        let result = self.decode_inner(command, budget, control);
        control.check()?;
        result
    }
    fn decode_inner(
        &self,
        command: &Value,
        budget: usize,
        control: &crate::loading::Control,
    ) -> Result<Raster, String> {
        let Codec::Image(format) = self.format else {
            return self.decode_jxl(command, budget);
        };
        if let Some(source) = &self.source {
            if ["frame", "page", "variant"]
                .iter()
                .any(|key| command.get(*key).is_some())
            {
                return Err("Static SVG import does not accept frame/page/variant choices".into());
            }
            check_budget(source.width, source.height, 8, budget)?;
            return source.render(source.width, source.height, 8);
        }
        let mut index = None;
        for key in ["frame", "page", "variant"] {
            if let Some(value) = command.get(key) {
                let expected = self.info.choice.unwrap_or(match format {
                    ImageFormat::Tiff => "page",
                    ImageFormat::Ico => "variant",
                    _ => "frame",
                });
                if key != expected {
                    return Err(format!("This image accepts {expected}, not {key}"));
                }
                index = Some(
                    value
                        .as_u64()
                        .and_then(|n| usize::try_from(n).ok())
                        .filter(|&n| n < self.info.count)
                        .ok_or("Image selection index is outside the available items")?,
                );
            }
        }
        if self.info.choice.is_some() && index.is_none() {
            return Err(format!(
                "Choose an explicit {} index (0–{}) before importing this image",
                self.info.choice.unwrap(),
                self.info.count - 1
            ));
        }
        let index = index.unwrap_or(0);
        if format == ImageFormat::Ico {
            let entry = bytes_at(&self.bytes, 6 + index * 16, 16)?;
            let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let at = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
            let content = bytes_at(&self.bytes, at, len)?;
            if content.starts_with(b"\x89PNG\r\n\x1a\n") {
                let image = Self::png(content.to_vec())?;
                if image.info.choice.is_some() {
                    return Err("An icon variant cannot contain an animation".into());
                }
                let pixels = image.decode_with_control(&serde_json::json!({}), budget, control)?;
                let width = if entry[0] == 0 { 256 } else { entry[0] as u32 };
                let height = if entry[1] == 0 { 256 } else { entry[1] as u32 };
                if (pixels.width, pixels.height) != (width, height) {
                    return Err("Icon directory dimensions do not match its image".into());
                }
                return Ok(pixels);
            }
            bmp::validate_dib(content)?;
        }
        let mut patched = None;
        if format == ImageFormat::Tiff && index != 0 {
            let mut bytes = self.bytes.clone();
            let little = &bytes[..2] == b"II";
            let magic = if little {
                u16::from_le_bytes(bytes[2..4].try_into().unwrap())
            } else {
                u16::from_be_bytes(bytes[2..4].try_into().unwrap())
            };
            let at = if magic == 43 { 8 } else { 4 };
            let pointer = if magic == 43 { 8 } else { 4 };
            let value = self.offsets[index] as u64;
            let encoded = if little {
                value.to_le_bytes()
            } else {
                value.to_be_bytes()
            };
            bytes[at..at + pointer].copy_from_slice(if little {
                &encoded[..pointer]
            } else {
                &encoded[8 - pointer..]
            });
            patched = Some(bytes);
        } else if format == ImageFormat::Ico {
            let mut bytes = self.bytes.clone();
            let entry = bytes[6 + index * 16..22 + index * 16].to_vec();
            bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
            bytes[6..22].copy_from_slice(&entry);
            patched = Some(bytes);
        }
        let bytes = patched.as_deref().unwrap_or(&self.bytes);
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
        reader.limits(limits());
        let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
        validate_color(&decoder)?;
        let (width, height) = decoder.dimensions();
        let depth = if decoder.color_type().bits_per_pixel()
            / decoder.color_type().channel_count() as u16
            > 8
        {
            16
        } else {
            8
        };
        check_budget(width, height, depth, budget)?;
        let profile = if format == ImageFormat::Jpeg {
            jpeg::profile(&self.bytes, &|| control.check())?.map(std::borrow::Cow::Owned)
        } else if format == ImageFormat::Tiff {
            tiff::profile(&self.bytes, index, &|| control.check())?.map(std::borrow::Cow::Borrowed)
        } else if format == ImageFormat::WebP {
            webp::profile(&self.bytes, &|| control.check())?.map(std::borrow::Cow::Borrowed)
        } else {
            decoder.icc_profile().map_err(|e| e.to_string())?.map(std::borrow::Cow::Owned)
        };
        if let Some(profile) = &profile {
            crate::color_profile::supported(profile)?;
        }
        let orientation = decoder.orientation().map_err(|e| e.to_string())?;
        let animated = self.info.choice == Some("frame");
        if format == ImageFormat::Png && !animated && png_rows::supported(bytes, orientation) {
            drop(decoder);
            // Layout eligibility is decided before decoding; failures never fall back.
            return png_rows::decode(bytes, width, height, depth, profile.as_deref(), control);
        }
        let mut image = if animated {
            if width as u64 * height as u64 * (index as u64 + 1) > 256 * 1024 * 1024 {
                return Err("Animation decoding work exceeds the import budget; export the selected frame separately".into());
            }
            drop(decoder);
            match format {
                ImageFormat::Gif => {
                    let mut d = image::codecs::gif::GifDecoder::new(Cursor::new(bytes))
                        .map_err(|e| e.to_string())?;
                    d.set_limits(limits()).map_err(|e| e.to_string())?;
                    DynamicImage::ImageRgba8(frame(d.into_frames(), index)?)
                }
                ImageFormat::WebP => {
                    let mut d = image::codecs::webp::WebPDecoder::new(Cursor::new(bytes))
                        .map_err(|e| e.to_string())?;
                    d.set_limits(limits()).map_err(|e| e.to_string())?;
                    DynamicImage::ImageRgba8(frame(d.into_frames(), index)?)
                }
                ImageFormat::Png => prepared_png::apng::resident(bytes, index, control)?,
                _ => return Err("Unsupported animation container".into()),
            }
        } else {
            DynamicImage::from_decoder(decoder).map_err(|e| format!("Cannot decode image: {e}"))?
        };
        image.apply_orientation(orientation);
        let (w, h) = (image.width(), image.height());
        if depth == 16 {
            let mut words = image.into_rgba16().into_raw();
            if let Some(profile) = profile.filter(|p| !crate::color_profile::is_srgb_profile(p)) {
                convert16_rows(&profile, &mut words, w, &|| Ok(()))?;
            }
            Raster::from_rgba16(w, h, &words)
        } else {
            let mut pixels = image.into_rgba8().into_raw();
            if let Some(profile) = profile.filter(|p| !crate::color_profile::is_srgb_profile(p)) {
                convert8_rows(&profile, &mut pixels, w, &|| Ok(()))?;
            }
            Raster::from_rgba(w, h, &pixels)
        }
    }
    pub fn prepare(&self, command: &Value, budget: usize) -> Result<Prepared, String> {
        self.prepare_with_control(command, budget, &crate::loading::Control::default())
    }
    pub(crate) fn prepare_with_control(
        &self,
        command: &Value,
        budget: usize,
        control: &crate::loading::Control,
    ) -> Result<Prepared, String> {
        Ok(Prepared {
            pixels: self.decode_with_control(command, budget, control)?,
            source: self.source.clone(),
        })
    }
}

fn frame(mut frames: image::Frames<'_>, index: usize) -> Result<image::RgbaImage, String> {
    for i in 0..=index {
        let frame = frames
            .next()
            .ok_or("Animation ended before the selected frame")?
            .map_err(|e| e.to_string())?;
        if i == index {
            return Ok(frame.into_buffer());
        }
    }
    unreachable!()
}
fn validate_color(decoder: &dyn ImageDecoder) -> Result<(), String> {
    use image::ExtendedColorType as C;
    if matches!(
        decoder.original_color_type(),
        C::Rgb32F | C::Rgba32F | C::Cmyk8 | C::Cmyk16
    ) {
        return Err("Floating-point/CMYK image import requires an explicit RGB conversion at supported precision".into());
    }
    let color = decoder.color_type();
    if color.bits_per_pixel() / color.channel_count() as u16 > 16 {
        return Err("Image channel precision is not supported".into());
    }
    Ok(())
}
pub(crate) fn check_budget(
    width: u32,
    height: u32,
    depth: u16,
    budget: usize,
) -> Result<(), String> {
    check_budget_with_limits(width, height, depth, budget, crate::raster::CanvasLimits::Resident)
}
pub(crate) fn check_budget_with_limits(
    width: u32,
    height: u32,
    depth: u16,
    budget: usize,
    limits: crate::raster::CanvasLimits,
) -> Result<(), String> {
    limits.check(width, height)?;
    // RGBA conversion and tiled storage coexist while constructing the result.
    let bytes = width as u64 * height as u64 * if depth == 16 { 16 } else { 8 };
    if bytes > budget as u64 {
        return Err(
            "Decoded images exceed the 256 MiB import budget; import smaller or fewer images"
                .into(),
        );
    }
    Ok(())
}
/// Native Windows DIB transport already supplies a decoder instead of a file.
pub(crate) fn decoder(
    mut decoder: impl ImageDecoder,
    budget: usize,
    geometry: crate::raster::CanvasLimits,
) -> Result<DynamicImage, String> {
    let mut admission = limits();
    admission.max_image_width = Some(geometry.edge());
    admission.max_image_height = Some(geometry.edge());
    admission.max_alloc = Some(budget.min(BUDGET) as u64);
    decoder.set_limits(admission).map_err(|e| e.to_string())?;
    validate_color(&decoder)?;
    let (w, h) = decoder.dimensions();
    let depth = if decoder.color_type().bits_per_pixel()
        / decoder.color_type().channel_count() as u16
        > 8
    {
        16
    } else {
        8
    };
    check_budget_with_limits(w, h, depth, budget, geometry)?;
    let profile = decoder.icc_profile().map_err(|e| e.to_string())?;
    if let Some(profile) = &profile {
        crate::color_profile::supported(profile)?;
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    let w = image.width();
    if depth == 16 {
        let mut words = image.into_rgba16();
        if let Some(profile) = profile.filter(|p| !crate::color_profile::is_srgb_profile(p)) {
            convert16_rows(&profile, &mut words, w, &|| Ok(()))?;
        }
        Ok(DynamicImage::ImageRgba16(words))
    } else {
        let mut bytes = image.into_rgba8();
        if let Some(profile) = profile.filter(|p| !crate::color_profile::is_srgb_profile(p)) {
            convert8_rows(&profile, &mut bytes, w, &|| Ok(()))?;
        }
        Ok(DynamicImage::ImageRgba8(bytes))
    }
}
// Match the native worker's oriented-row grouping, including SIMD tails.
// Reuse one executor and keep the conversion scratch bounded to one row.
fn convert8_rows(
    profile: &[u8],
    pixels: &mut [u8],
    width: u32,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let conversion = crate::color_profile::transforms(profile)?;
    for row in pixels.chunks_exact_mut(width as usize * 4) {
        check()?;
        conversion.convert8(row)?;
    }
    Ok(())
}
fn convert16_rows(
    profile: &[u8],
    pixels: &mut [u16],
    width: u32,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let conversion = crate::color_profile::transforms(profile)?;
    for row in pixels.chunks_exact_mut(width as usize * 4) {
        check()?;
        conversion.convert16(row)?;
    }
    Ok(())
}

pub(crate) fn working_jxl_pixels(
    mut decoded: crate::jpeg_xl::Decoded,
    check: &dyn Fn() -> Result<(), String>,
) -> Result<crate::jpeg_xl::Pixels, String> {
    use crate::jpeg_xl::Pixels;
    check()?;
    // Use the profile of the decoded samples, including the decoder's XYB
    // output selection. Working sRGB needs no second transform.
    let profile = &decoded.icc_profile;
    if !crate::color_profile::is_srgb_profile(profile) {
        let width = decoded.info.width;
        match &mut decoded.pixels {
            Pixels::Eight(pixels) => convert8_rows(profile, pixels, width, check)?,
            Pixels::Sixteen(pixels) => convert16_rows(profile, pixels, width, check)?,
        }
    }
    check()?;
    Ok(decoded.pixels)
}

pub fn inspect(path: &Path) -> Result<Info, String> {
    match native::OpenFormat::from_path(path) {
        Some(native::OpenFormat::Png) => prepared_png::apng::inspect_path(path),
        Some(native::OpenFormat::Bmp) => bmp::inspect(path),
        Some(native::OpenFormat::Gif) => gif::inspect_file(path),
        Some(native::OpenFormat::Jpeg) => jpeg::inspect_file(path),
        Some(native::OpenFormat::Jxl) => jxl::inspect(path),
        Some(native::OpenFormat::Qoi) => qoi::inspect(path),
        Some(native::OpenFormat::Pnm) => pnm::inspect(path),
        Some(native::OpenFormat::Tga) => tga::inspect(path),
        Some(native::OpenFormat::Tiff) => tiff::inspect(path),
        Some(native::OpenFormat::Webp) => webp::inspect_file(path),
        _ => Ok(Encoded::file(path)?.info),
    }
}
pub fn decode_file(path: &Path, command: &Value, budget: usize) -> Result<Raster, String> {
    Encoded::file(path)?.decode(command, budget)
}
pub(crate) fn jpeg_xl_patch_image(path: &Path, target: u16) -> Result<DynamicImage, String> {
    let encoded = Encoded::file(path)?;
    if encoded.format != Codec::Jxl {
        return Err("Image patch source is not JPEG XL".into());
    }
    let info = crate::jpeg_xl::Info {
        width: encoded.info.width,
        height: encoded.info.height,
        bit_depth: encoded.info.bit_depth,
    };
    crate::engine::image_patch_precision(info.bit_depth, target)?;
    check_budget(info.width, info.height, target, 128 * 1024 * 1024)?;
    let decoded = crate::jpeg_xl::decode_patch(&encoded.bytes, info)?;
    let (width, height) = (decoded.info.width, decoded.info.height);
    match decoded.pixels {
        crate::jpeg_xl::Pixels::Eight(pixels) => image::ImageBuffer::from_raw(width, height, pixels).map(DynamicImage::ImageRgba8),
        crate::jpeg_xl::Pixels::Sixteen(pixels) => image::ImageBuffer::from_raw(width, height, pixels).map(DynamicImage::ImageRgba16),
    }.ok_or_else(|| "JPEG XL patch pixels differ from their decoded shape".into())
}
pub fn prepare_file(path: &Path, command: &Value, budget: usize) -> Result<Prepared, String> {
    Encoded::file(path)?.prepare(command, budget)
}

pub fn apply(
    doc: &mut crate::engine::Document,
    command: &Value,
    image: Prepared,
) -> Result<(), String> {
    use crate::engine::Layer;
    let Prepared { mut pixels, source } = image;
    if let Some(source) = &source {
        source.validate()?;
        if (source.width, source.height) != (pixels.width, pixels.height) {
            return Err("Prepared SVG dimensions differ from its source".into());
        }
    }
    if doc.layers.len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let placement = placement::Placement::new(command)?;
    let parent = placement.parent;
    if let Some(parent) = parent {
        crate::placement::unlocked(doc, parent)?;
        if !doc
            .layers
            .iter()
            .any(|l| l.id == parent && l.kind == "group")
        {
            return Err("Image parent must be a folder".into());
        }
    }
    let [x, y] = placement.origin;
    let name = placement.name;
    if doc.bit_depth == 16 {
        pixels.promote16();
    }
    let promote = pixels.depth == 16 || doc.bit_depth == 16;
    let stored = |r: &Raster| r.stored_bytes() * if promote && r.depth == 8 { 2 } else { 1 };
    let bytes: usize = doc
        .layers
        .iter()
        .map(|l| {
            stored(&l.pixels)
                + l.mask.as_ref().map_or(0, |m| {
                    m.steps.iter().map(|s| stored(&s.pixels)).sum::<usize>()
                })
        })
        .sum();
    if bytes + pixels.stored_bytes() > 512 * 1024 * 1024 {
        return Err("Image import exceeds the document's 512 MiB raster budget".into());
    }
    let mut layer = Layer::new(&name, "paint", pixels.width, pixels.height);
    layer.pixels = pixels;
    layer.source = source.map(Into::into);
    layer.parent = parent.map(str::to_owned);
    layer.x = x;
    layer.y = y;
    let at = parent
        .and_then(|id| doc.layers.iter().position(|l| l.id == id))
        .map_or(0, |i| i + 1);
    doc.layers.insert(at, layer);
    // Imported samples are interpreted/converted into the sRGB working space.
    // Tag standard saves too, so that space does not depend on private sources.
    doc.srgb_tagged = true;
    Ok(())
}
