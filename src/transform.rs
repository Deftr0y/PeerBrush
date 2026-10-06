use crate::{
    engine::Layer,
    raster::{blend, check_size, Raster},
};
use serde_json::Value;
pub(crate) fn copy_shift(source: &Raster, w: u32, h: u32, dx: i32, dy: i32) -> Raster {
    let mut out = Raster::new(w, h);
    for (&(tx, ty), tile) in &source.tiles {
        for (i, p) in tile.chunks_exact(4).enumerate() {
            if p[3] == 0 {
                continue;
            }
            let x = (tx * crate::raster::TILE + i as u32 % crate::raster::TILE) as i32;
            let y = (ty * crate::raster::TILE + i as u32 / crate::raster::TILE) as i32;
            out.set(x + dx, y + dy, p.try_into().unwrap());
        }
    }
    out
}
pub fn selection(layer: &mut Layer, area: [i32; 4], c: &Value) -> Result<[i32; 4], String> {
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
    let corners = [
        f(area[0] as f32, area[1] as f32),
        f(area[2] as f32, area[1] as f32),
        f(area[2] as f32, area[3] as f32),
        f(area[0] as f32, area[3] as f32),
    ];
    let b = [
        corners
            .iter()
            .map(|p| p[0])
            .fold(f32::INFINITY, f32::min)
            .floor() as i32,
        corners
            .iter()
            .map(|p| p[1])
            .fold(f32::INFINITY, f32::min)
            .floor() as i32,
        corners
            .iter()
            .map(|p| p[0])
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil() as i32,
        corners
            .iter()
            .map(|p| p[1])
            .fold(f32::NEG_INFINITY, f32::max)
            .ceil() as i32,
    ];
    let mask = c["mask"].as_bool().unwrap_or(false);
    let (ox, oy) = (layer.x, layer.y);
    let left = ox.min(b[0]);
    let top = oy.min(b[1]);
    let right = (ox + layer.pixels.width as i32).max(b[2]);
    let bottom = (oy + layer.pixels.height as i32).max(b[3]);
    let (w, h) = ((right - left) as u32, (bottom - top) as u32);
    check_size(w, h)?;
    let mut source = if mask {
        let m = layer.mask.as_ref().ok_or("Layer has no mask")?;
        m.steps
            .iter()
            .filter(|s| s.kind == "paint")
            .find(|s| c["step"].as_str() == Some(s.id.as_str()))
            .or_else(|| m.steps.iter().rev().find(|s| s.kind == "paint"))
            .ok_or("No paint mask step")?
            .pixels
            .clone()
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
            cut.set(x - ox, y - oy, source.get(x - ox, y - oy));
            source.set(x - ox, y - oy, [0; 4]);
        }
    }
    let mut out = copy_shift(&source, w, h, ox - left, oy - top);
    for y in b[1]..b[3] {
        for x in b[0]..b[2] {
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
    if let Some(m) = &mut layer.mask {
        m.cache_key = crate::engine::id();
        for step in &mut m.steps {
            step.pixels = copy_shift(&step.pixels, w, h, ox - left, oy - top);
        }
    }
    if mask {
        let m = layer.mask.as_mut().unwrap();
        let index = m
            .steps
            .iter()
            .position(|s| c["step"].as_str() == Some(s.id.as_str()))
            .or_else(|| m.steps.iter().rposition(|s| s.kind == "paint"))
            .unwrap();
        m.steps[index].pixels = out;
        layer.pixels = copy_shift(&layer.pixels, w, h, ox - left, oy - top);
    } else {
        layer.pixels = out;
        layer.kind = "paint".into();
    }
    layer.x = left;
    layer.y = top;
    Ok(b)
}
