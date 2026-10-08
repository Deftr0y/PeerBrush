//! Place agent-generated or edited pixels using document coordinates and ordinary history.
use crate::{
    engine::{Document, Layer},
    raster::{blend, check_size, Raster},
    transform::copy_shift,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use std::{io::Cursor, path::Path};

fn decode(command: &Value, depth: u16) -> Result<Raster, String> {
    let bytes = match (command.get("path"), command.get("png")) {
        (Some(path), None) => {
            let path = Path::new(path.as_str().ok_or("Image path must be a string")?);
            if !path.is_absolute() {
                return Err("Use an absolute local image path".into());
            }
            if std::fs::metadata(path).map_err(|e| e.to_string())?.len() > 128 * 1024 * 1024 {
                return Err("Encoded image exceeds 128 MiB".into());
            }
            std::fs::read(path).map_err(|e| e.to_string())?
        }
        (None, Some(png)) => {
            let png = png.as_str().ok_or("png must contain base64 PNG bytes")?;
            if png.len() > 180 * 1024 * 1024 {
                return Err("Encoded image is too large".into());
            }
            let bytes = STANDARD.decode(png).map_err(|e| e.to_string())?;
            if image::guess_format(&bytes).ok() != Some(image::ImageFormat::Png) {
                return Err("png must contain a PNG image".into());
            }
            bytes
        }
        _ => return Err("Provide exactly one absolute path or base64 png".into()),
    };
    let format = image::guess_format(&bytes).map_err(|e| e.to_string())?;
    if !matches!(format, image::ImageFormat::Png | image::ImageFormat::Jpeg) {
        return Err("Place PNG or JPEG pixels".into());
    }
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|e| e.to_string())?;
    if depth == 16 {
        let image = image.to_rgba16();
        Raster::from_rgba16(image.width(), image.height(), image.as_raw())
    } else {
        let image = image.to_rgba8();
        Raster::from_rgba(image.width(), image.height(), image.as_raw())
    }
}

fn unlocked(doc: &Document, target: &str) -> Result<(), String> {
    let mut current = Some(target);
    for _ in 0..=16 {
        let Some(id) = current else { return Ok(()) };
        let layer = doc
            .layers
            .iter()
            .find(|l| l.id == id)
            .ok_or("Unknown image destination")?;
        if layer.locked {
            return Err("Unlock the image destination first".into());
        }
        current = layer.parent.as_deref();
    }
    Err("Group nesting limit exceeded".into())
}

pub fn place(doc: &mut Document, command: &Value) -> Result<(), String> {
    let new_layer = match command.get("new_layer") {
        None => true,
        Some(value) => value.as_bool().ok_or("new_layer must be true or false")?,
    };
    let context = command
        .get("layer")
        .map(|value| value.as_str().ok_or("layer must be a layer or folder ID"))
        .transpose()?;
    if let Some(context) = context {
        unlocked(doc, context)?;
    }
    let context_index = context.and_then(|id| doc.layers.iter().position(|l| l.id == id));
    let parent = if new_layer {
        if let Some(value) = command.get("parent") {
            if value.is_null() {
                None
            } else {
                let id = value.as_str().ok_or("Parent must be a folder ID or null")?;
                unlocked(doc, id)?;
                if !doc.layers.iter().any(|l| l.id == id && l.kind == "group") {
                    return Err("Parent must be a folder".into());
                }
                Some(id.to_owned())
            }
        } else {
            context_index.and_then(|index| {
                let layer = &doc.layers[index];
                if layer.kind == "group" {
                    Some(layer.id.clone())
                } else {
                    layer.parent.clone()
                }
            })
        }
    } else {
        if command.get("parent").is_some() {
            return Err("parent is only used for new layers".into());
        }
        let index =
            context_index.ok_or("Choose the current layer with layer and new_layer:false")?;
        if doc.layers[index].kind == "group" {
            return Err("Place into a paint layer or create a new child layer".into());
        }
        None
    };
    if new_layer && doc.layers.len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let mode = command
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("over");
    if !["over", "replace"].contains(&mode) {
        return Err("Image mode must be over or replace".into());
    }
    let source = decode(command, doc.bit_depth)?;
    let bounds = if let Some(value) = command.get("rect") {
        let area = crate::engine::rect(value).ok_or("Invalid destination rectangle")?;
        if area[0] == area[2] || area[1] == area[3] {
            return Err("Image rectangle must have positive size".into());
        }
        area
    } else {
        let coordinate = |key: &str| -> Result<i32, String> {
            match command.get(key) {
                None => Ok(0),
                Some(value) => value
                    .as_i64()
                    .filter(|n| (-100000..=100000).contains(n))
                    .map(|n| n as i32)
                    .ok_or_else(|| format!("Invalid image {key}")),
            }
        };
        let (x, y) = (coordinate("x")?, coordinate("y")?);
        [x, y, x + source.width as i32, y + source.height as i32]
    };
    let (width, height) = (
        (bounds[2] - bounds[0]) as u32,
        (bounds[3] - bounds[1]) as u32,
    );
    check_size(width, height)?;
    let natural_size = (width, height) == (source.width, source.height);
    // Premultiplied interpolation prevents transparent generated edges acquiring black fringes.
    let sample = |x: u32, y: u32| {
        if natural_size {
            return source.get(x as i32, y as i32);
        }
        source.sample(
            ((x as f32 + 0.5) * source.width as f32 / width as f32 - 0.5)
                .clamp(0.0, source.width as f32 - 1.0),
            ((y as f32 + 0.5) * source.height as f32 / height as f32 - 0.5)
                .clamp(0.0, source.height as f32 - 1.0),
        )
    };
    let sample16 = |x: u32, y: u32| {
        if natural_size {
            return source.get16(x as i32, y as i32);
        }
        source.sample16(
            ((x as f32 + 0.5) * source.width as f32 / width as f32 - 0.5)
                .clamp(0., source.width as f32 - 1.),
            ((y as f32 + 0.5) * source.height as f32 / height as f32 - 0.5)
                .clamp(0., source.height as f32 - 1.),
        )
    };
    if new_layer {
        let name = command
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Placed image");
        let mut layer = Layer::new(name, "paint", width, height);
        layer.x = bounds[0];
        layer.y = bounds[1];
        layer.parent = parent.clone();
        layer.pixels = Raster::new_depth(width, height, doc.bit_depth);
        if natural_size {
            layer.pixels = source.clone();
        } else {
            for y in 0..height {
                for x in 0..width {
                    if doc.bit_depth == 16 {
                        layer.pixels.set16(x as i32, y as i32, sample16(x, y));
                    } else {
                        layer.pixels.set(x as i32, y as i32, sample(x, y));
                    }
                }
            }
        }
        let at = context_index
            .filter(|i| doc.layers[*i].parent == parent && doc.layers[*i].kind != "group")
            .unwrap_or_else(|| {
                parent
                    .as_deref()
                    .and_then(|id| doc.layers.iter().position(|l| l.id == id))
                    .map(|i| i + 1)
                    .unwrap_or(0)
            });
        doc.layers.insert(at, layer);
    } else {
        let layer = &mut doc.layers[context_index.unwrap()];
        let (old_x, old_y) = (layer.x, layer.y);
        let left = old_x.min(bounds[0]);
        let top = old_y.min(bounds[1]);
        let right = (old_x + layer.pixels.width as i32).max(bounds[2]);
        let bottom = (old_y + layer.pixels.height as i32).max(bounds[3]);
        let (w, h) = ((right - left) as u32, (bottom - top) as u32);
        check_size(w, h)?;
        let unchanged_bounds =
            (left, top, w, h) == (old_x, old_y, layer.pixels.width, layer.pixels.height);
        let mut pixels = if layer.kind == "fill" {
            let mut pixels = Raster::new_depth(w, h, doc.bit_depth);
            for y in 0..layer.pixels.height {
                for x in 0..layer.pixels.width {
                    pixels.set(x as i32 + old_x - left, y as i32 + old_y - top, layer.color);
                }
            }
            pixels
        } else if unchanged_bounds {
            layer.pixels.clone()
        } else {
            copy_shift(&layer.pixels, w, h, old_x - left, old_y - top)
        };
        for y in 0..height {
            for x in 0..width {
                let (tx, ty) = (bounds[0] - left + x as i32, bounds[1] - top + y as i32);
                if doc.bit_depth == 16 {
                    let pixel = sample16(x, y);
                    pixels.set16(
                        tx,
                        ty,
                        if mode == "replace" {
                            pixel
                        } else {
                            crate::raster::blend16(pixels.get16(tx, ty), pixel, 1., "normal")
                        },
                    );
                    continue;
                }
                let pixel = sample(x, y);
                pixels.set(
                    tx,
                    ty,
                    if mode == "replace" {
                        pixel
                    } else {
                        blend(pixels.get(tx, ty), pixel, 1.0, "normal")
                    },
                );
            }
        }
        if !unchanged_bounds {
            if let Some(mask) = &mut layer.mask {
                mask.cache_key = crate::engine::id();
                for step in &mut mask.steps {
                    step.pixels = copy_shift(&step.pixels, w, h, old_x - left, old_y - top);
                }
            }
        }
        layer.pixels = pixels;
        layer.kind = "paint".into();
        layer.x = left;
        layer.y = top;
    }
    Ok(())
}
