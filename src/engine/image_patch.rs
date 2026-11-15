//! Image patches keep source precision and clip in document coordinates.
use super::Layer;
use image::ImageDecoder;
use serde_json::Value;

pub(crate) struct Placement {
    pub origin: [i32; 2],
    rect: Option<[i32; 4]>,
}
impl Placement {
    pub(crate) fn new(command: &Value) -> Result<Self, String> {
        if command["mask"] == true {
            return Err("Image patches require a color paint layer".into());
        }
        let coordinate = |key| match command.get(key) {
            None => Ok(0),
            Some(value) => value
                .as_f64()
                .filter(|v| {
                    v.is_finite()
                        && v.fract() == 0.
                        && *v >= i32::MIN as f64
                        && *v <= i32::MAX as f64
                })
                .map(|v| v as i32)
                .ok_or_else(|| format!("Image patch {key} must be an integer document coordinate")),
        };
        let rect = match command.get("rect").filter(|r| !r.is_null()) {
            Some(rect) => Some(super::rect(rect).ok_or("Invalid rectangle coordinates")?),
            None => None,
        };
        Ok(Self {
            origin: [coordinate("x")?, coordinate("y")?],
            rect,
        })
    }
    pub(crate) fn clip(
        &self,
        size: [u32; 2],
        layer_origin: [i32; 2],
        layer_size: [u32; 2],
        selection: Option<[i32; 4]>,
    ) -> Result<Option<[i32; 4]>, String> {
        let end = |origin: i32, size: u32| i64::from(origin) + i64::from(size);
        let mut area = [
            i64::from(self.origin[0]).max(i64::from(layer_origin[0])),
            i64::from(self.origin[1]).max(i64::from(layer_origin[1])),
            end(self.origin[0], size[0]).min(end(layer_origin[0], layer_size[0])),
            end(self.origin[1], size[1]).min(end(layer_origin[1], layer_size[1])),
        ];
        for bounds in [self.rect, selection].into_iter().flatten() {
            area[0] = area[0].max(i64::from(bounds[0]));
            area[1] = area[1].max(i64::from(bounds[1]));
            area[2] = area[2].min(i64::from(bounds[2]));
            area[3] = area[3].min(i64::from(bounds[3]));
        }
        if area[0] >= area[2] || area[1] >= area[3] {
            return Ok(None);
        }
        let mut result = [0; 4];
        for (out, value) in result.iter_mut().zip(area) {
            *out = i32::try_from(value).map_err(|_| "Image patch exceeds document coordinates")?;
        }
        Ok(Some(result))
    }
}

pub(crate) fn precision(source: u16, target: u16) -> Result<(), String> {
    if ![8, 16].contains(&source) {
        return Err("Image patches require integer 8-bit or 16-bit channels".into());
    }
    if source > target {
        return Err("Use a 16-bit document to preserve this image patch's native channels".into());
    }
    Ok(())
}

fn read_image(path: &std::path::Path, depth: u16) -> Result<image::DynamicImage, String> {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jxl")) {
        return crate::image_import::jpeg_xl_patch_image(path, depth);
    }
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| e.to_string())?
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    if let Some(format @ (image::ImageFormat::Bmp | image::ImageFormat::Ico)) = reader.format() {
        let mut input = reader.into_inner();
        crate::image_import::bmp::check_reader(&mut input, format)?;
        reader = image::ImageReader::with_format(input, format);
    }
    if reader.format() == Some(image::ImageFormat::Jpeg) {
        let mut input = reader.into_inner();
        crate::image_import::jpeg::check_reader(&mut input)?;
        reader = image::ImageReader::with_format(input, image::ImageFormat::Jpeg);
    }
    if reader.format() == Some(image::ImageFormat::WebP) {
        let mut input = reader.into_inner();
        crate::image_import::webp::check_reader(&mut input)?;
        reader = image::ImageReader::with_format(input, image::ImageFormat::WebP);
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits.clone());
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let color = decoder.color_type();
    precision(
        color.bits_per_pixel() / u16::from(color.channel_count()),
        depth,
    )?;
    // Preserve ImageReader::decode's output reservation when inspecting the
    // decoder first to reject precision loss before allocating pixel storage.
    limits
        .reserve(decoder.total_bytes())
        .map_err(|e| e.to_string())?;
    decoder.set_limits(limits).map_err(|e| e.to_string())?;
    image::DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())
}

pub(super) fn apply(
    layer: &mut Layer,
    command: &Value,
    selection: Option<[i32; 4]>,
    polygon: Option<&[[f32; 2]]>,
    coverage: Option<&crate::selection::Coverage>,
) -> Result<(), String> {
    let placement = Placement::new(command)?;
    let image = read_image(std::path::Path::new(super::text(command, "path", "")), layer.pixels.depth)?;
    let Some(area) = placement.clip(
        [image.width(), image.height()],
        [layer.x, layer.y],
        [layer.pixels.width, layer.pixels.height],
        selection,
    )?
    else {
        return Ok(());
    };
    // Raw channels/default image frame match the existing patch command. Image
    // placement/import own color management and EXIF orientation separately.
    let (image8, image16) = if layer.pixels.depth == 16 {
        (None, Some(image.into_rgba16()))
    } else {
        (Some(image.into_rgba8()), None)
    };
    for y in area[1]..area[3] {
        for x in area[0]..area[2] {
            if coverage.map_or_else(
                || {
                    polygon.is_some_and(|p| {
                        !crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5)
                    })
                },
                |m| m.value(x, y) <= 0.,
            ) {
                continue;
            }
            let sx = (i64::from(x) - i64::from(placement.origin[0])) as u32;
            let sy = (i64::from(y) - i64::from(placement.origin[1])) as u32;
            let source = if let Some(image) = &image16 {
                image.get_pixel(sx, sy).0
            } else {
                image8
                    .as_ref()
                    .unwrap()
                    .get_pixel(sx, sy)
                    .0
                    .map(|v| u16::from(v) * 257)
            };
            let (lx, ly) = (x - layer.x, y - layer.y);
            if layer.pixels.depth == 16 {
                layer.pixels.set16(
                    lx,
                    ly,
                    crate::raster::blend16(layer.pixels.get16(lx, ly), source, 1., "normal"),
                );
            } else {
                layer.pixels.set(
                    lx,
                    ly,
                    crate::raster::blend(
                        layer.pixels.get(lx, ly),
                        source.map(crate::raster::project16),
                        1.,
                        "normal",
                    ),
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn patch_intersections_do_not_wrap_at_extreme_document_coordinates() {
        for x in [i32::MIN, i32::MAX] {
            let placement = Placement::new(&json!({"x":x})).unwrap();
            assert_eq!(
                placement
                    .clip([8192, 8192], [-2, 1], [270, 6], None)
                    .unwrap(),
                None
            );
        }
        let placement = Placement::new(&json!({"x":-1,"y":0,"rect":[0,0,267,5]})).unwrap();
        assert_eq!(
            placement
                .clip([270, 6], [-2, 1], [270, 6], Some([250, 2, 266, 5]))
                .unwrap(),
            Some([250, 2, 266, 5])
        );
        assert_eq!(
            placement
                .clip([270, 6], [-2, 1], [270, 6], Some([0; 4]))
                .unwrap(),
            None
        );
    }
}
