//! Place agent-generated or edited pixels using document coordinates and ordinary history.
use crate::{
    engine::{Document, Layer},
    raster::{blend, Raster},
    transform::copy_shift,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use std::path::Path;

pub(crate) mod input;

pub(crate) fn decode(command: &Value, depth: u16, budget: usize) -> Result<Raster, String> {
    let mut pixels = match (command.get("path"), command.get("png")) {
        (Some(path), None) => crate::image_import::decode_file(
            Path::new(path.as_str().ok_or("Image path must be a string")?),
            command,
            budget,
        )?,
        (None, Some(png)) => {
            let png = png.as_str().ok_or("png must contain base64 PNG bytes")?;
            if png.len() > 180 * 1024 * 1024 {
                return Err("Encoded image is too large".into());
            }
            crate::image_import::Encoded::png(STANDARD.decode(png).map_err(|e| e.to_string())?)?
                .decode(command, budget)?
        }
        _ => return Err("Provide exactly one absolute path or base64 png".into()),
    };
    if depth == 16 {
        pixels.promote16();
    }
    Ok(pixels)
}

pub(crate) fn unlocked(doc: &Document, target: &str) -> Result<(), String> {
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
    if current.is_some() {
        Err("Group nesting limit exceeded".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod lock_tests {
    use super::*;
    #[test]
    fn sixteen_ancestors_are_valid_but_locks_cycles_and_seventeen_are_not() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.layers[0].id = "leaf".into();
        for i in 0..16 {
            let mut group = crate::engine::Layer::new(&format!("Folder {i}"), "group", 1, 1);
            group.id = format!("g{i}");
            doc.layers.last_mut().unwrap().parent = Some(group.id.clone());
            doc.layers.push(group);
        }
        assert!(unlocked(&doc, "leaf").is_ok());
        doc.layers[16].locked = true;
        assert!(unlocked(&doc, "leaf").unwrap_err().contains("Unlock"));
        doc.layers[16].locked = false;
        doc.layers[16].parent = Some("leaf".into());
        assert!(unlocked(&doc, "leaf").unwrap_err().contains("nesting"));
        let mut extra = crate::engine::Layer::new("Extra", "group", 1, 1);
        extra.id = "extra".into();
        doc.layers[16].parent = Some(extra.id.clone());
        doc.layers.push(extra);
        assert!(unlocked(&doc, "leaf").unwrap_err().contains("nesting"));
    }
}

pub fn place(doc: &mut Document, command: &Value) -> Result<(), String> {
    let plan = input::Plan::new(command, doc.layers.len(), |i| {
        let l = &doc.layers[i];
        input::Layer {
            id: &l.id,
            parent: l.parent.as_deref(),
            kind: &l.kind,
            locked: l.locked,
            source: l.source.is_some(),
        }
    })?;
    let source = decode(command, doc.bit_depth, crate::image_import::BUDGET)?;
    if source.depth == 16 && doc.bit_depth != 16 {
        doc.bit_depth = 16;
        doc.ensure_depth();
    }
    let bounds = input::bounds(command, [source.width, source.height], crate::raster::CanvasLimits::Resident)?;
    let [width, height] = input::shape(bounds, crate::raster::CanvasLimits::Resident)?;
    let natural_size = (width, height) == (source.width, source.height);
    // Premultiplied interpolation prevents transparent generated edges acquiring black fringes.
    let sample = |x: u32, y: u32| {
        if natural_size {
            return source.get(x as i32, y as i32);
        }
        source.sample(
            input::coordinate(x, source.width, width),
            input::coordinate(y, source.height, height),
        )
    };
    let sample16 = |x: u32, y: u32| {
        if natural_size {
            return source.get16(x as i32, y as i32);
        }
        source.sample16(
            input::coordinate(x, source.width, width),
            input::coordinate(y, source.height, height),
        )
    };
    if plan.target.is_none() {
        let name = command
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Placed image");
        let mut layer = Layer::new(name, "paint", width, height);
        layer.x = bounds[0];
        layer.y = bounds[1];
        layer.parent = plan.parent.clone();
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
        doc.layers.insert(plan.index, layer);
    } else {
        let layer = &mut doc.layers[plan.target.unwrap()];
        let (old_x, old_y) = (layer.x, layer.y);
        let [left, top, right, bottom] = input::frame(
            [old_x, old_y],
            [layer.pixels.width, layer.pixels.height],
            bounds,
            crate::raster::CanvasLimits::Resident,
        )?;
        let [w, h] = input::shape([left, top, right, bottom], crate::raster::CanvasLimits::Resident)?;
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
                        if plan.replace {
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
                    if plan.replace {
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
    if !doc.psd_intentionally_untagged {
        doc.srgb_tagged = true;
    }
    Ok(())
}
