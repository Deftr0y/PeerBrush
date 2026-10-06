use crate::{
    engine::Layer,
    raster::{blend, check_size, Raster},
};
use serde_json::Value;
pub(crate) fn copy_shift(source: &Raster, w: u32, h: u32, dx: i32, dy: i32) -> Raster {
    if (source.width, source.height, dx, dy) == (w, h, 0, 0) {
        return source.clone();
    }
    let mut out = Raster::new(w, h);
    for (&(tx, ty), tile) in &source.tiles {
        for (i, p) in tile.chunks_exact(4).enumerate() {
            if p[3] == 0 {
                continue;
            }
            let x = (tx * crate::raster::TILE + i as u32 % crate::raster::TILE) as i32;
            let y = (ty * crate::raster::TILE + i as u32 / crate::raster::TILE) as i32;
            if x < source.width as i32 && y < source.height as i32 {
                out.set(x + dx, y + dy, p.try_into().unwrap());
            }
        }
    }
    out
}
pub fn selection(layer: &mut Layer, area: [i32; 4], c: &Value) -> Result<[i32; 4], String> {
    selection_with_polygon(layer, &crate::selection::rectangle(area), c).map(|(bounds, _)| bounds)
}

pub fn selection_with_polygon(
    layer: &mut Layer,
    polygon: &[[f32; 2]],
    c: &Value,
) -> Result<([i32; 4], Vec<[f32; 2]>), String> {
    let area = crate::selection::bounds(polygon)?;
    if layer.kind == "group" {
        return Err("Transform a paint layer or its mask".into());
    }
    let n = |key: &str, default: f32| c[key].as_f64().unwrap_or(default as f64) as f32;
    let move_only = c["op"] == "move";
    let (dx, dy) = if move_only {
        (n("dx", 0.0).round(), n("dy", 0.0).round())
    } else {
        (0.0, 0.0)
    };
    let angle = if move_only { 0.0 } else { n("angle", 0.0) };
    let (sx, sy) = if move_only {
        (1.0, 1.0)
    } else {
        (n("scale_x", 1.0), n("scale_y", 1.0))
    };
    let px = c["pivot"][0]
        .as_f64()
        .unwrap_or((area[0] as f64 + area[2] as f64) / 2.0) as f32;
    let py = c["pivot"][1]
        .as_f64()
        .unwrap_or((area[1] as f64 + area[3] as f64) / 2.0) as f32;
    if ![dx, dy, angle, px, py]
        .iter()
        .all(|v| v.is_finite() && v.abs() <= 100000.0)
        || !(0.05..=20.0).contains(&sx)
        || !(0.05..=20.0).contains(&sy)
    {
        return Err("Invalid selection transform".into());
    }
    let (mut sin, mut cos) = angle.to_radians().sin_cos();
    if sin.abs() < 0.000001 {
        sin = 0.0;
    }
    if cos.abs() < 0.000001 {
        cos = 0.0;
    }
    let f = |x: f32, y: f32| {
        [
            px + (x - px) * sx * cos - (y - py) * sy * sin + dx,
            py + (x - px) * sx * sin + (y - py) * sy * cos + dy,
        ]
    };
    let transformed = polygon.iter().map(|p| f(p[0], p[1])).collect::<Vec<_>>();
    let b = crate::selection::bounds(&transformed)?;
    let mask = c["mask"].as_bool().unwrap_or(false);
    let (ox, oy) = (layer.x, layer.y);
    let left = ox.min(b[0]);
    let top = oy.min(b[1]);
    let right = (ox + layer.pixels.width as i32).max(b[2]);
    let bottom = (oy + layer.pixels.height as i32).max(b[3]);
    let (w, h) = ((right - left) as u32, (bottom - top) as u32);
    check_size(w, h)?;
    let mask_step = if mask {
        let m = layer.mask.as_ref().ok_or("Layer has no mask")?;
        Some(
            if let Some(step) = c.get("step").and_then(Value::as_str) {
                m.steps
                    .iter()
                    .position(|s| s.id == step && s.kind == "paint")
            } else {
                m.steps.iter().rposition(|s| s.kind == "paint")
            }
            .ok_or("No paint mask step")?,
        )
    } else {
        None
    };
    let mut source = if let Some(index) = mask_step {
        layer.mask.as_ref().unwrap().steps[index].pixels.clone()
    } else {
        layer.pixels.clone()
    };
    if layer.kind == "fill" && !mask {
        for y in 0..source.height {
            for x in 0..source.width {
                source.set(x as i32, y as i32, layer.color);
            }
        }
    }
    let mut cut = Raster::new(source.width, source.height);
    for y in area[1].max(oy)..area[3].min(oy + source.height as i32) {
        for x in area[0].max(ox)..area[2].min(ox + source.width as i32) {
            if !crate::selection::contains(polygon, x as f32 + 0.5, y as f32 + 0.5) {
                continue;
            }
            cut.set(x - ox, y - oy, source.get(x - ox, y - oy));
            source.set(x - ox, y - oy, [0; 4]);
        }
    }
    let mut out = copy_shift(&source, w, h, ox - left, oy - top);
    for y in b[1]..b[3] {
        for x in b[0]..b[2] {
            if !crate::selection::contains(&transformed, x as f32 + 0.5, y as f32 + 0.5) {
                continue;
            }
            let rx = x as f32 + 0.5 - px - dx;
            let ry = y as f32 + 0.5 - py - dy;
            let ux = (rx * cos + ry * sin) / sx + px - ox as f32 - 0.5;
            let uy = (-rx * sin + ry * cos) / sy + py - oy as f32 - 0.5;
            let pixel = cut.sample(ux, uy);
            if pixel[3] > 0 {
                out.set(
                    x - left,
                    y - top,
                    blend(out.get(x - left, y - top), pixel, 1.0, "normal"),
                );
            }
        }
    }
    let expanded = (left, top, w, h) != (ox, oy, layer.pixels.width, layer.pixels.height);
    if let Some(m) = &mut layer.mask {
        if mask || expanded {
            m.cache_key = crate::engine::id();
        }
        if expanded {
            for step in &mut m.steps {
                step.pixels = copy_shift(&step.pixels, w, h, ox - left, oy - top);
            }
        }
    }
    if let Some(index) = mask_step {
        layer.mask.as_mut().unwrap().steps[index].pixels = out;
        if expanded {
            if layer.kind == "fill" {
                for y in 0..layer.pixels.height {
                    for x in 0..layer.pixels.width {
                        layer.pixels.set(x as i32, y as i32, layer.color);
                    }
                }
                layer.kind = "paint".into();
            }
            layer.pixels = copy_shift(&layer.pixels, w, h, ox - left, oy - top);
        }
    } else {
        layer.pixels = out;
        layer.kind = "paint".into();
    }
    layer.x = left;
    layer.y = top;
    Ok((b, transformed))
}
