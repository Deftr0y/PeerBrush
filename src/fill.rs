//! Tile-wise foreground fills, clipped in document coordinates and committed by Engine.edit.
use crate::{
    engine::{Document, Layer},
    raster::{blend, check_size, Pixel, Raster, TILE},
};
use serde_json::Value;
use std::{collections::btree_map::Entry, sync::Arc};

fn fill_tiles(
    raster: &mut Raster,
    area: [i32; 4],
    color: Pixel,
    origin: [i32; 2],
    polygon: Option<&[[f32; 2]]>,
) {
    if raster.depth == 16 {
        fill_tiles16(raster, area, color.map(|v| v as u16 * 257), origin, polygon);
        return;
    }
    if color[3] == 0 {
        return;
    }
    let area = [
        area[0].max(0),
        area[1].max(0),
        area[2].min(raster.width as i32),
        area[3].min(raster.height as i32),
    ];
    if area[0] >= area[2] || area[1] >= area[3] {
        return;
    }
    let mut constant = vec![0; (TILE * TILE * 4) as usize];
    for pixel in constant.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }
    let constant = Arc::new(constant);
    for ty in area[1] as u32 / TILE..=(area[3] - 1) as u32 / TILE {
        for tx in area[0] as u32 / TILE..=(area[2] - 1) as u32 / TILE {
            let tile_x = (tx * TILE) as i32;
            let tile_y = (ty * TILE) as i32;
            let (x0, x1, y0, y1) = (
                area[0].max(tile_x),
                area[2].min(tile_x + TILE as i32),
                area[1].max(tile_y),
                area[3].min(tile_y + TILE as i32),
            );
            let full = x0 == tile_x
                && y0 == tile_y
                && x1 == tile_x + TILE as i32
                && y1 == tile_y + TILE as i32;
            if polygon.is_none()
                && full
                && (color[3] == 255 || !raster.tiles.contains_key(&(tx, ty)))
            {
                raster.tiles.insert((tx, ty), constant.clone());
                continue;
            }
            let tile = match raster.tiles.entry((tx, ty)) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(Arc::new(vec![0; (TILE * TILE * 4) as usize])),
            };
            let pixels = Arc::make_mut(tile);
            for y in y0..y1 {
                for x in x0..x1 {
                    if polygon.is_some_and(|p| {
                        !crate::selection::contains(
                            p,
                            (x + origin[0]) as f32 + 0.5,
                            (y + origin[1]) as f32 + 0.5,
                        )
                    }) {
                        continue;
                    }
                    let at = (((y - tile_y) as u32 * TILE + (x - tile_x) as u32) * 4) as usize;
                    let old = pixels[at..at + 4].try_into().unwrap();
                    let result = if color[3] == 255 {
                        color
                    } else {
                        blend(old, color, 1.0, "normal")
                    };
                    pixels[at..at + 4].copy_from_slice(&result);
                }
            }
        }
    }
}

fn fill_tiles16(
    raster: &mut Raster,
    area: [i32; 4],
    color: [u16; 4],
    origin: [i32; 2],
    polygon: Option<&[[f32; 2]]>,
) {
    if color[3] == 0 {
        return;
    }
    let area = [
        area[0].max(0),
        area[1].max(0),
        area[2].min(raster.width as i32),
        area[3].min(raster.height as i32),
    ];
    if area[0] >= area[2] || area[1] >= area[3] {
        return;
    }
    let mut constant = vec![0; (TILE * TILE * 4) as usize];
    for pixel in constant.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }
    let constant = Arc::new(constant);
    for ty in area[1] as u32 / TILE..=(area[3] - 1) as u32 / TILE {
        for tx in area[0] as u32 / TILE..=(area[2] - 1) as u32 / TILE {
            let tile_x = (tx * TILE) as i32;
            let tile_y = (ty * TILE) as i32;
            let (x0, x1, y0, y1) = (
                area[0].max(tile_x),
                area[2].min(tile_x + TILE as i32),
                area[1].max(tile_y),
                area[3].min(tile_y + TILE as i32),
            );
            let full = x0 == tile_x
                && y0 == tile_y
                && x1 == tile_x + TILE as i32
                && y1 == tile_y + TILE as i32;
            if polygon.is_none()
                && full
                && (color[3] == 65535 || !raster.samples16.contains_key(&(tx, ty)))
            {
                raster.samples16.insert((tx, ty), constant.clone());
                continue;
            }
            let tile = match raster.samples16.entry((tx, ty)) {
                Entry::Occupied(entry) => entry.into_mut(),
                Entry::Vacant(entry) => entry.insert(Arc::new(vec![0; (TILE * TILE * 4) as usize])),
            };
            let pixels = Arc::make_mut(tile);
            for y in y0..y1 {
                for x in x0..x1 {
                    if polygon.is_some_and(|p| {
                        !crate::selection::contains(
                            p,
                            (x + origin[0]) as f32 + 0.5,
                            (y + origin[1]) as f32 + 0.5,
                        )
                    }) {
                        continue;
                    }
                    let at = (((y - tile_y) as u32 * TILE + (x - tile_x) as u32) * 4) as usize;
                    let old = pixels[at..at + 4].try_into().unwrap();
                    let result = if color[3] == 65535 {
                        color
                    } else {
                        crate::raster::blend16(old, color, 1.0, "normal")
                    };
                    pixels[at..at + 4].copy_from_slice(&result);
                }
            }
        }
    }
}

pub fn apply(doc: &mut Document, command: &Value) -> Result<(), String> {
    let target = command["layer"].as_str().ok_or("Choose a layer to fill")?;
    let index = doc
        .layers
        .iter()
        .position(|l| l.id == target)
        .ok_or("Layer no longer exists")?;
    let mut area = command
        .get("rect")
        .and_then(crate::engine::rect)
        .or(doc.selection)
        .unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
    if let Some(s) = doc.selection {
        area = [
            area[0].max(s[0]),
            area[1].max(s[1]),
            area[2].min(s[2]),
            area[3].min(s[3]),
        ];
    }
    area = [
        area[0].max(0),
        area[1].max(0),
        area[2].min(doc.width as i32),
        area[3].min(doc.height as i32),
    ];
    if area[0] >= area[2] || area[1] >= area[3] {
        return Ok(());
    }
    let values = command["color"]
        .as_array()
        .ok_or("Fill color must be RGBA")?;
    if values.len() != 4 || values.iter().any(|v| v.as_u64().is_none_or(|v| v > 255)) {
        return Err("Fill color must contain four byte values".into());
    }
    let mut color: Pixel = [
        values[0].as_u64().unwrap() as u8,
        values[1].as_u64().unwrap() as u8,
        values[2].as_u64().unwrap() as u8,
        values[3].as_u64().unwrap() as u8,
    ];
    let mask = command["mask"].as_bool().unwrap_or(false);
    let polygon = crate::selection::polygon(doc).map(Vec::from);
    if doc.layers[index].kind == "group" && !mask {
        if doc.layers.len() >= 100 {
            return Err("Initial version supports up to 100 layers".into());
        }
        let mut child = Layer::new("Foreground fill", "paint", doc.width, doc.height);
        child.parent = Some(target.into());
        child.pixels = Raster::new_depth(doc.width, doc.height, doc.bit_depth);
        fill_tiles(&mut child.pixels, area, color, [0, 0], polygon.as_deref());
        doc.layers.insert(index + 1, child);
        return Ok(());
    }
    if mask {
        color = [color[0], color[0], color[0], color[3]];
    }
    let layer = &mut doc.layers[index];
    let mask_step = if mask {
        let m = layer.mask.as_ref().ok_or("Layer has no mask")?;
        Some(
            if let Some(step) = command.get("step").and_then(Value::as_str) {
                m.steps
                    .iter()
                    .position(|s| s.id == step && s.kind == "paint")
            } else {
                m.steps.iter().rposition(|s| s.kind == "paint")
            }
            .ok_or("Add a paint step to the mask")?,
        )
    } else {
        None
    };
    let (old_x, old_y) = (layer.x, layer.y);
    let left = old_x.min(area[0]);
    let top = old_y.min(area[1]);
    let right = (old_x + layer.pixels.width as i32).max(area[2]);
    let bottom = (old_y + layer.pixels.height as i32).max(area[3]);
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    check_size(width, height)?;
    let expanded =
        (left, top, width, height) != (old_x, old_y, layer.pixels.width, layer.pixels.height);
    // Legacy fill sources remain intact until an explicit raster edit needs materialized pixels.
    if layer.kind == "fill" && (!mask || expanded) {
        let bounds = [0, 0, layer.pixels.width as i32, layer.pixels.height as i32];
        fill_tiles(&mut layer.pixels, bounds, layer.color, [0, 0], None);
        layer.kind = "paint".into();
    }
    if expanded {
        layer.pixels =
            crate::transform::copy_shift(&layer.pixels, width, height, old_x - left, old_y - top);
        if let Some(mask) = &mut layer.mask {
            for step in &mut mask.steps {
                step.pixels = crate::transform::copy_shift(
                    &step.pixels,
                    width,
                    height,
                    old_x - left,
                    old_y - top,
                );
            }
        }
        layer.x = left;
        layer.y = top;
    }
    if let Some(mask) = &mut layer.mask {
        mask.cache_key = crate::engine::id();
    }
    let raster = if let Some(step) = mask_step {
        &mut layer.mask.as_mut().unwrap().steps[step].pixels
    } else {
        &mut layer.pixels
    };
    fill_tiles(
        raster,
        [area[0] - left, area[1] - top, area[2] - left, area[3] - top],
        color,
        [left, top],
        polygon.as_deref(),
    );
    Ok(())
}

/// Erase only current coverage, without moving sources, changing masks/effects,
/// or expanding raster bounds. The Engine owns rollback, reservations and undo.
pub fn clear_selection(doc: &mut Document, command: &Value) -> Result<(), String> {
    let coverage = crate::selection::current(doc).ok_or("Select pixels before clearing")?;
    if coverage.bounds[0] >= coverage.bounds[2] || coverage.bounds[1] >= coverage.bounds[3] {
        return Ok(());
    }
    let target = command["layer"].as_str().ok_or("Choose a layer to clear")?;
    let layer = doc
        .layers
        .iter()
        .find(|l| l.id == target)
        .ok_or("Layer no longer exists")?;
    let mask = command["mask"] == true;
    let ids = if layer.kind == "group" && !mask {
        crate::transform::tree_ids(doc, &[target.into()])
    } else {
        vec![target.into()]
    };
    let mut count = 0;
    for layer in &mut doc.layers {
        if !ids.contains(&layer.id) {
            continue;
        }
        if layer.locked {
            return Err("A selected layer or folder child is locked".into());
        }
        if layer.kind == "group" && !mask {
            continue;
        }
        if !mask && (!matches!(layer.kind.as_str(), "paint" | "fill") || layer.source.is_some()) {
            return Err("Rasterize editable text/vector or adjustment layers before clearing selected color pixels".into());
        }
        if !mask && layer.kind == "fill" {
            let area = [0, 0, layer.pixels.width as i32, layer.pixels.height as i32];
            fill_tiles(&mut layer.pixels, area, layer.color, [0, 0], None);
            layer.kind = "paint".into();
        }
        let (ox, oy) = (layer.x, layer.y);
        let raster = crate::engine::edit_raster(layer, command)?;
        let bounds = coverage.bounds;
        for y in bounds[1].max(0).max(oy)
            ..bounds[3]
                .min(doc.height as i32)
                .min(oy + raster.height as i32)
        {
            for x in bounds[0].max(0).max(ox)
                ..bounds[2]
                    .min(doc.width as i32)
                    .min(ox + raster.width as i32)
            {
                let remaining = 1. - coverage.value(x, y) as f64;
                if remaining >= 1. {
                    continue;
                }
                let (x, y) = (x - ox, y - oy);
                if raster.depth == 16 {
                    let mut pixel = raster.get16(x, y);
                    pixel[3] = (pixel[3] as f64 * remaining).round() as u16;
                    if pixel[3] == 0 {
                        pixel = [0; 4];
                    }
                    raster.set16(x, y, pixel);
                } else {
                    let mut pixel = raster.get(x, y);
                    pixel[3] = (pixel[3] as f64 * remaining).round() as u8;
                    if pixel[3] == 0 {
                        pixel = [0; 4];
                    }
                    raster.set(x, y, pixel);
                }
            }
        }
        if mask {
            layer.mask.as_mut().unwrap().cache_key = crate::engine::id();
        }
        count += 1;
    }
    if count == 0 {
        return Err("Folder has no raster pixels to clear".into());
    }
    Ok(())
}
