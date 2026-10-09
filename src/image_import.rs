//! Bounded, precision-preserving raster import shared by document commands and clipboard files.
use crate::raster::{check_size, Raster};
use image::{AnimationDecoder, DynamicImage, ImageDecoder, ImageFormat};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{Cursor, Read},
    path::Path,
};

pub const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "bmp", "gif", "tif", "tiff", "webp", "ico", "pbm", "pgm", "ppm", "pam",
    "pnm", "tga", "qoi", "svg", "svgz",
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

pub struct Encoded {
    bytes: Vec<u8>,
    format: ImageFormat,
    pub info: Info,
    offsets: Vec<usize>,
    source: Option<crate::source::Source>,
}
pub struct Prepared {
    pub pixels: Raster,
    pub source: Option<crate::source::Source>,
}

fn bytes_at(bytes: &[u8], at: usize, len: usize) -> Result<&[u8], String> {
    bytes
        .get(at..at.checked_add(len).ok_or("Invalid image offset")?)
        .ok_or_else(|| "Truncated image structure".into())
}
fn be32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes(bytes.try_into().unwrap())
}

fn jpeg_color(bytes: &[u8]) -> Result<(), String> {
    let mut at = 2;
    while at < bytes.len() {
        if bytes_at(bytes, at, 1)?[0] != 0xff {
            return Err("Invalid JPEG marker".into());
        }
        while bytes_at(bytes, at, 1)?[0] == 0xff {
            at += 1;
        }
        let marker = bytes_at(bytes, at, 1)?[0];
        at += 1;
        if matches!(marker, 0xda | 0xd9) {
            break;
        }
        if matches!(marker, 0xd8 | 0xd0..=0xd7 | 0x01) {
            continue;
        }
        let length = u16::from_be_bytes(bytes_at(bytes, at, 2)?.try_into().unwrap()) as usize;
        if length < 2 {
            return Err("Invalid JPEG segment length".into());
        }
        let data = bytes_at(bytes, at + 2, length - 2)?;
        if (0xc0..=0xcf).contains(&marker) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
            let header = bytes_at(data, 0, 6)?;
            if header[0] != 8 {
                return Err(
                    "JPEG channel precision is not supported; convert explicitly before importing"
                        .into(),
                );
            }
            if !matches!(header[5], 1 | 3) {
                return Err(
                    "CMYK/four-channel JPEG requires explicit RGB conversion before importing"
                        .into(),
                );
            }
            return Ok(());
        }
        at += length;
    }
    Err("JPEG contains no supported image frame header".into())
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
    let header = bytes_at(bytes, 0, 13)?;
    let mut at = 13;
    if header[10] & 128 != 0 {
        at += 3 * (1 << ((header[10] & 7) + 1));
    }
    let mut count = 0;
    loop {
        let block = bytes_at(bytes, at, 1)?[0];
        at += 1;
        match block {
            0x3b => break,
            0x21 => {
                let label = bytes_at(bytes, at, 1)?[0];
                at += 1;
                if label == 0xff
                    && bytes_at(bytes, at, 1)?[0] == 11
                    && bytes_at(bytes, at + 1, 11)? == b"ICCRGBG1012"
                {
                    return Err(
                        "GIF ICC color extensions require explicit conversion before importing"
                            .into(),
                    );
                }
            }
            0x2c => {
                let descriptor = bytes_at(bytes, at, 9)?;
                at += 9;
                if descriptor[8] & 128 != 0 {
                    at += 3 * (1 << ((descriptor[8] & 7) + 1));
                }
                bytes_at(bytes, at, 1)?;
                at += 1;
                count += 1;
                if count > MAX_ITEMS {
                    return Err("GIF animation frame limit exceeded".into());
                }
            }
            _ => return Err("Invalid GIF block".into()),
        }
        loop {
            let len = bytes_at(bytes, at, 1)?[0] as usize;
            at += 1;
            bytes_at(bytes, at, len)?;
            at += len;
            if len == 0 {
                break;
            }
        }
    }
    if count == 0 {
        return Err("GIF contains no frames".into());
    }
    Ok(count)
}

fn webp_frames(bytes: &[u8]) -> Result<usize, String> {
    let mut at = 12;
    let mut count = 0;
    let size = u32::from_le_bytes(bytes_at(bytes, 4, 4)?.try_into().unwrap()) as usize + 8;
    if size != bytes.len() {
        return Err("Invalid WebP container size".into());
    }
    while at < size {
        let header = bytes_at(bytes, at, 8)?;
        let len = u32::from_le_bytes(header[4..].try_into().unwrap()) as usize;
        bytes_at(bytes, at + 8, len)?;
        if &header[..4] == b"ANMF" {
            count += 1;
        }
        if count > MAX_ITEMS {
            return Err("WebP animation frame limit exceeded".into());
        }
        at += 8 + len + (len & 1);
    }
    if at != size {
        return Err("Invalid WebP chunk alignment".into());
    }
    Ok(count.max(1))
}

// Follow the IFD chain without decoding pages. Selecting a page changes only the
// first-IFD pointer in an owned byte buffer; offsets and the source file stay intact.
fn tiff_offsets(bytes: &[u8]) -> Result<Vec<usize>, String> {
    let little = match bytes_at(bytes, 0, 2)? {
        b"II" => true,
        b"MM" => false,
        _ => return Err("Invalid TIFF byte order".into()),
    };
    let number = |at: usize, size: usize| -> Result<usize, String> {
        let b = bytes_at(bytes, at, size)?;
        let mut value = 0u64;
        for &v in if little {
            b.iter().rev().collect::<Vec<_>>()
        } else {
            b.iter().collect()
        } {
            value = (value << 8) | v as u64;
        }
        usize::try_from(value).map_err(|_| "TIFF offset exceeds address space".into())
    };
    let big = match number(2, 2)? {
        42 => false,
        43 => true,
        _ => return Err("Invalid TIFF version".into()),
    };
    if big && (number(4, 2)? != 8 || number(6, 2)? != 0) {
        return Err("Invalid BigTIFF header".into());
    }
    let (pointer, count_size, entry_size) = if big { (8, 8, 20) } else { (4, 2, 12) };
    let mut at = number(if big { 8 } else { 4 }, pointer)?;
    let mut offsets = Vec::new();
    while at != 0 {
        if offsets.len() >= MAX_ITEMS || offsets.contains(&at) {
            return Err("TIFF page limit exceeded or cyclic page chain".into());
        }
        offsets.push(at);
        let entries = number(at, count_size)?;
        let end = entries
            .checked_mul(entry_size)
            .and_then(|n| n.checked_add(at))
            .and_then(|n| n.checked_add(count_size))
            .ok_or("Invalid TIFF directory size")?;
        bytes_at(bytes, at, end - at)?;
        at = number(end, pointer)?;
    }
    if offsets.is_empty() {
        return Err("TIFF contains no pages".into());
    }
    Ok(offsets)
}

impl Encoded {
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
            return Err("Encoded image must be a file at most 128 MiB".into());
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
                format: ImageFormat::Png,
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
            jpeg_color(&bytes)?;
        }
        if format == ImageFormat::Bmp {
            let size = u32::from_le_bytes(bytes_at(&bytes, 14, 4)?.try_into().unwrap());
            if size >= 108 {
                let space = u32::from_le_bytes(bytes_at(&bytes, 70, 4)?.try_into().unwrap());
                if !matches!(space, 0x73524742 | 0x57696e20)
                    && (space != 0 || bytes_at(&bytes, 74, 48)?.iter().any(|&v| v != 0))
                {
                    return Err("Calibrated/profiled BMP color encoding requires explicit RGB conversion before importing".into());
                }
            }
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
        // The animation decoder interface produces only RGBA8. Never narrow an
        // original PNG16 animation into an editable eight-bit frame.
        if format == ImageFormat::Png && png.is_some_and(|p| p.1) && bit_depth == 16 {
            return Err("16-bit APNG frames cannot be imported at their original precision; export a static PNG16 frame first".into());
        }
        drop(decoder);
        Ok(Self {
            bytes,
            format,
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
                let expected = self.info.choice.unwrap_or(match self.format {
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
        if self.format == ImageFormat::Ico {
            let entry = bytes_at(&self.bytes, 6 + index * 16, 16)?;
            let len = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
            let at = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
            let content = bytes_at(&self.bytes, at, len)?;
            if content.starts_with(b"\x89PNG\r\n\x1a\n") {
                let image = Self::png(content.to_vec())?;
                if image.info.choice.is_some() {
                    return Err("An icon variant cannot contain an animation".into());
                }
                let pixels = image.decode(&serde_json::json!({}), budget)?;
                let width = if entry[0] == 0 { 256 } else { entry[0] as u32 };
                let height = if entry[1] == 0 { 256 } else { entry[1] as u32 };
                if (pixels.width, pixels.height) != (width, height) {
                    return Err("Icon directory dimensions do not match its image".into());
                }
                return Ok(pixels);
            }
        }
        let mut patched = None;
        if self.format == ImageFormat::Tiff && index != 0 {
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
        } else if self.format == ImageFormat::Ico {
            let mut bytes = self.bytes.clone();
            let entry = bytes[6 + index * 16..22 + index * 16].to_vec();
            bytes[4..6].copy_from_slice(&1u16.to_le_bytes());
            bytes[6..22].copy_from_slice(&entry);
            patched = Some(bytes);
        }
        let bytes = patched.as_deref().unwrap_or(&self.bytes);
        let mut reader = image::ImageReader::with_format(Cursor::new(bytes), self.format);
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
        let profile = decoder.icc_profile().map_err(|e| e.to_string())?;
        if let Some(profile) = &profile {
            crate::color_profile::supported(profile)?;
        }
        let orientation = decoder.orientation().map_err(|e| e.to_string())?;
        let animated = self.info.choice == Some("frame");
        let mut image = if animated {
            if width as u64 * height as u64 * (index as u64 + 1) > 256 * 1024 * 1024 {
                return Err("Animation decoding work exceeds the import budget; export the selected frame separately".into());
            }
            drop(decoder);
            match self.format {
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
                ImageFormat::Png => {
                    let mut d = image::codecs::png::PngDecoder::new(Cursor::new(bytes))
                        .map_err(|e| e.to_string())?;
                    d.set_limits(limits()).map_err(|e| e.to_string())?;
                    DynamicImage::ImageRgba8(frame(
                        d.apng().map_err(|e| e.to_string())?.into_frames(),
                        index,
                    )?)
                }
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
                crate::color_profile::convert16(&profile, &mut words)?;
            }
            Raster::from_rgba16(w, h, &words)
        } else {
            let mut pixels = image.into_rgba8().into_raw();
            if let Some(profile) = profile.filter(|p| !crate::color_profile::is_srgb_profile(p)) {
                crate::color_profile::convert8(&profile, &mut pixels)?;
            }
            Raster::from_rgba(w, h, &pixels)
        }
    }
    pub fn prepare(&self, command: &Value, budget: usize) -> Result<Prepared, String> {
        Ok(Prepared {
            pixels: self.decode(command, budget)?,
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
fn check_budget(width: u32, height: u32, depth: u16, budget: usize) -> Result<(), String> {
    check_size(width, height)?;
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
pub(crate) fn decoder(mut decoder: impl ImageDecoder, budget: usize) -> Result<Raster, String> {
    decoder.set_limits(limits()).map_err(|e| e.to_string())?;
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
    check_budget(w, h, depth, budget)?;
    let profile = decoder.icc_profile().map_err(|e| e.to_string())?;
    if let Some(profile) = &profile {
        crate::color_profile::supported(profile)?;
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    image.apply_orientation(orientation);
    let (w, h) = (image.width(), image.height());
    if depth == 16 {
        let mut words = image.into_rgba16().into_raw();
        if let Some(profile) = profile.filter(|p| !crate::color_profile::is_srgb_profile(p)) {
            crate::color_profile::convert16(&profile, &mut words)?;
        }
        Raster::from_rgba16(w, h, &words)
    } else {
        let mut bytes = image.into_rgba8().into_raw();
        if let Some(profile) = profile.filter(|p| !crate::color_profile::is_srgb_profile(p)) {
            crate::color_profile::convert8(&profile, &mut bytes)?;
        }
        Raster::from_rgba(w, h, &bytes)
    }
}
pub fn inspect(path: &Path) -> Result<Info, String> {
    Ok(Encoded::file(path)?.info)
}
pub fn decode_file(path: &Path, command: &Value, budget: usize) -> Result<Raster, String> {
    Encoded::file(path)?.decode(command, budget)
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
    let parent = command
        .get("parent")
        .filter(|p| !p.is_null())
        .map(|v| v.as_str().ok_or("Image parent must be a folder ID"))
        .transpose()?;
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
    let coordinate = |key: &str| {
        command.get(key).map_or(Ok(0), |v| {
            v.as_i64()
                .filter(|n| (-100000..=100000).contains(n))
                .map(|n| n as i32)
                .ok_or_else(|| format!("Invalid image {key}"))
        })
    };
    let (x, y) = (coordinate("x")?, coordinate("y")?);
    let path = Path::new(command["path"].as_str().ok_or("Missing image path")?);
    let name = command["name"]
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| {
            path.file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
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
    layer.source = source;
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
