use crate::{
    engine::{Document, Layer},
    raster::{blend, check_size, Raster},
};
use serde_json::Value;
pub const MOVE_SNAP: f64 = 8.0;
pub const ANGLE_SNAP: f64 = 15.0;
pub const SCALE_SNAP: f64 = 0.1;
pub const SKEW_LIMIT: f64 = 80.0;

/// Translation after the affine placement, in rounded document pixels.
pub(crate) fn translation(c: &Value) -> Result<[f64; 2], String> {
    let mut delta = [0.; 2];
    for (index, name) in ["dx", "dy"].into_iter().enumerate() {
        let value = c.get(name).map_or(Ok(0.), |value| {
            value
                .as_f64()
                .ok_or("Transform translation must be a number")
        })?;
        if !value.is_finite() || value.abs() > 100000. {
            return Err("Transform translation must be between -100000 and 100000 pixels".into());
        }
        delta[index] = value.round();
    }
    Ok(delta)
}

fn skew_angles(c: &Value) -> Result<[f64; 2], String> {
    let mut angles = [0.; 2];
    if c["op"] == "move" {
        return Ok(angles);
    }
    for (index, name) in ["skew_x", "skew_y"].into_iter().enumerate() {
        let angle = c.get(name).map_or(Ok(0.), |value| {
            value.as_f64().ok_or("Transform skew must be a number")
        })?;
        if !angle.is_finite() || angle.abs() > SKEW_LIMIT {
            return Err("Transform skew must be between -80 and 80 degrees".into());
        }
        angles[index] = angle;
    }
    Ok(angles)
}

/// Horizontal then vertical shear precede the existing scale/reflection/rotation.
pub(crate) fn skewed([a, b, c, d]: [f64; 4], command: &Value) -> Result<[f64; 4], String> {
    let angles = skew_angles(command)?;
    if angles == [0., 0.] {
        return Ok([a, b, c, d]);
    }
    let [x, y] = angles.map(|angle| angle.to_radians().tan());
    Ok([
        a + c * y,
        b + d * y,
        a * x + c * (1. + x * y),
        b * x + d * (1. + x * y),
    ])
}

fn selected_skew(c: &Value) -> Result<(), String> {
    if c["op"] != "move" {
        translation(c)?;
    }
    if skew_angles(c)? != [0., 0.] {
        return Err("Skew does not support selected pixels yet. Use selection_only:false for the whole target; the selection stays unchanged.".into());
    }
    Ok(())
}

pub(crate) fn work_budget(width: u32, height: u32, rasters: usize) -> Result<(), String> {
    if u64::from(width) * u64::from(height) * rasters as u64 > 128_000_000 {
        return Err("Transform exceeds the 128-million-sample work budget".into());
    }
    Ok(())
}

/// Positive scale magnitudes and explicit reflection flags share one interpretation.
pub fn scales(c: &Value) -> Result<(f64, f64), String> {
    let axis = |name: &str, mirror: &str| {
        let value = c.get(name).map_or(Ok(1.), |v| {
            v.as_f64().ok_or("Transform scale must be a number")
        })?;
        let reflected = c.get(mirror).map_or(Ok(false), |v| {
            v.as_bool()
                .ok_or("Transform mirror flags must be true or false")
        })?;
        if !value.is_finite() || !(0.05..=20.).contains(&value) {
            return Err("Transform scale must be 0.05–20".to_string());
        }
        Ok(if reflected { -value } else { value })
    };
    Ok((axis("scale_x", "mirror_x")?, axis("scale_y", "mirror_y")?))
}

/// Resolve document-space snapping before previews, reservations and history.
pub fn resolve_command(command: &Value) -> Result<Value, String> {
    if !matches!(command["op"].as_str(), Some("move" | "transform")) {
        return Ok(command.clone());
    }
    if command["op"] == "transform" {
        scales(command)?;
        skew_angles(command)?;
        translation(command)?;
    }
    let Some(snap) = command.get("snap") else {
        return Ok(command.clone());
    };
    if !snap
        .as_bool()
        .ok_or("Transform snap must be true or false")?
    {
        return Ok(command.clone());
    }
    let mut resolved = command.clone();
    let mut fields = if command["op"] == "move" {
        vec![
            ("dx", 0., MOVE_SNAP, -100000., 100000.),
            ("dy", 0., MOVE_SNAP, -100000., 100000.),
        ]
    } else {
        vec![
            ("angle", 0., ANGLE_SNAP, -100000., 100000.),
            ("scale_x", 1., SCALE_SNAP, 0.05, 20.),
            ("scale_y", 1., SCALE_SNAP, 0.05, 20.),
        ]
    };
    if command["op"] == "transform" {
        for name in ["dx", "dy"] {
            if command.get(name).is_some() {
                fields.push((name, 0., MOVE_SNAP, -100000., 100000.));
            }
        }
        for name in ["skew_x", "skew_y"] {
            if command.get(name).is_some() {
                fields.push((name, 0., ANGLE_SNAP, -SKEW_LIMIT, SKEW_LIMIT));
            }
        }
    }
    for (key, default, step, min, max) in fields {
        let number = command.get(key).map_or(Ok(default), |v| {
            v.as_f64().ok_or("Snapped transform values must be numbers")
        })?;
        if !number.is_finite() || number < min || number > max {
            return Err("Snapped transform values exceed their supported bounds".into());
        }
        let lower = (min / step).ceil() * step;
        let upper = (max / step).floor() * step;
        resolved[key] = serde_json::json!(((number / step).round() * step).clamp(lower, upper));
    }
    Ok(resolved)
}
pub fn resolve_commands(mut commands: Vec<Value>) -> Result<Vec<Value>, String> {
    for command in &mut commands {
        if command["op"] == "transform" {
            scales(command)?;
            skew_angles(command)?;
            translation(command)?;
        }
        if command.get("snap").is_some()
            && matches!(command["op"].as_str(), Some("move" | "transform"))
        {
            *command = resolve_command(command)?;
        }
    }
    Ok(commands)
}
pub(crate) fn copy_shift(source: &Raster, w: u32, h: u32, dx: i32, dy: i32) -> Raster {
    if (source.width, source.height, dx, dy) == (w, h, 0, 0) {
        return source.clone();
    }
    if source.depth == 16 {
        let mut out = Raster::new_depth(w, h, 16);
        for (&(tx, ty), tile) in &source.samples16 {
            if tx >= source.width.div_ceil(crate::raster::TILE)
                || ty >= source.height.div_ceil(crate::raster::TILE)
            {
                continue;
            }
            for (i, p) in tile.chunks_exact(4).enumerate() {
                if p.iter().all(|v| *v == 0) {
                    continue;
                }
                let x = (tx * crate::raster::TILE + i as u32 % crate::raster::TILE) as i32;
                let y = (ty * crate::raster::TILE + i as u32 / crate::raster::TILE) as i32;
                if x < source.width as i32 && y < source.height as i32 {
                    out.set16(x + dx, y + dy, p.try_into().unwrap());
                }
            }
        }
        return out;
    }
    let mut out = Raster::new(w, h);
    for (&(tx, ty), tile) in &source.tiles {
        for (i, p) in tile.chunks_exact(4).enumerate() {
            if p.iter().all(|v| *v == 0) {
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
    selected_skew(c)?;
    if layer.mask.as_ref().is_some_and(|mask| mask.linked) {
        return linked_selection(layer, polygon, c, None);
    }
    if c["mask"] == true && c["step"].is_null() {
        return mask_selection(layer, polygon, c, None);
    }
    selection_impl(layer, polygon, c, None)
}
pub fn selection_with_coverage(
    layer: &mut Layer,
    coverage: &crate::selection::Coverage,
    c: &Value,
) -> Result<crate::selection::Coverage, String> {
    selected_skew(c)?;
    if coverage.bounds[0] >= coverage.bounds[2] {
        return Ok(coverage.clone());
    }
    let polygon = crate::selection::rectangle(coverage.bounds);
    let (bounds, _) = if layer.mask.as_ref().is_some_and(|mask| mask.linked) {
        linked_selection(layer, &polygon, c, Some(coverage))?
    } else if c["mask"] == true && c["step"].is_null() {
        mask_selection(layer, &polygon, c, Some(coverage))?
    } else {
        selection_impl(layer, &polygon, c, Some(coverage))?
    };
    let n = |k: &str, d: f32| c[k].as_f64().unwrap_or(d as f64) as f32;
    let moving = c["op"] == "move";
    let (dx, dy) = if moving {
        (n("dx", 0.).round(), n("dy", 0.).round())
    } else {
        let [dx, dy] = translation(c)?;
        (dx as f32, dy as f32)
    };
    let (sx, sy) = if moving {
        (1., 1.)
    } else {
        let (sx, sy) = scales(c)?;
        (sx as f32, sy as f32)
    };
    let (sin, cos) = if moving {
        (0., 1.)
    } else {
        n("angle", 0.).to_radians().sin_cos()
    };
    let px = c["pivot"][0]
        .as_f64()
        .unwrap_or((coverage.bounds[0] as f64 + coverage.bounds[2] as f64) / 2.)
        as f32;
    let py = c["pivot"][1]
        .as_f64()
        .unwrap_or((coverage.bounds[1] as f64 + coverage.bounds[3] as f64) / 2.)
        as f32;
    let mut mask = Raster::new(
        (bounds[2] - bounds[0]) as u32,
        (bounds[3] - bounds[1]) as u32,
    );
    for y in bounds[1]..bounds[3] {
        for x in bounds[0]..bounds[2] {
            let rx = x as f32 + 0.5 - px - dx;
            let ry = y as f32 + 0.5 - py - dy;
            let ux = (rx * cos + ry * sin) / sx + px - coverage.origin[0] as f32 - 0.5;
            let uy = (-rx * sin + ry * cos) / sy + py - coverage.origin[1] as f32 - 0.5;
            // Coverage is scalar; RGBA sampling would unpremultiply its edge values.
            let (ix, iy) = (ux.floor() as i32, uy.floor() as i32);
            let (fx, fy) = (ux - ix as f32, uy - iy as f32);
            let at = |x, y| coverage.mask.get(x, y)[0] as f32;
            let v = (at(ix, iy) * (1. - fx) * (1. - fy)
                + at(ix + 1, iy) * fx * (1. - fy)
                + at(ix, iy + 1) * (1. - fx) * fy
                + at(ix + 1, iy + 1) * fx * fy)
                .round() as u8;
            if v > 0 {
                mask.set(x - bounds[0], y - bounds[1], [v; 4]);
            }
        }
    }
    Ok(crate::selection::from_mask(mask)?.local(-bounds[0], -bounds[1]))
}
fn selection_impl(
    layer: &mut Layer,
    polygon: &[[f32; 2]],
    c: &Value,
    coverage: Option<&crate::selection::Coverage>,
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
        let [dx, dy] = translation(c)?;
        (dx as f32, dy as f32)
    };
    let angle = if move_only { 0.0 } else { n("angle", 0.0) };
    let (sx, sy) = if move_only {
        (1.0, 1.0)
    } else {
        let (sx, sy) = scales(c)?;
        (sx as f32, sy as f32)
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
        || !(0.05..=20.0).contains(&sx.abs())
        || !(0.05..=20.0).contains(&sy.abs())
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
    if dx == 0.0 && dy == 0.0 && angle.rem_euclid(360.0) == 0.0 && sx == 1.0 && sy == 1.0 {
        return Ok((b, transformed));
    }
    let mask = c["mask"].as_bool().unwrap_or(false);
    let (ox, oy) = (layer.x, layer.y);
    let left = ox.min(b[0]);
    let top = oy.min(b[1]);
    let right = (ox + layer.pixels.width as i32).max(b[2]);
    let bottom = (oy + layer.pixels.height as i32).max(b[3]);
    let (w, h) = ((right - left) as u32, (bottom - top) as u32);
    check_size(w, h)?;
    work_budget(w, h, 1 + layer.mask.as_ref().map_or(0, |m| m.steps.len()))?;
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
    let mut cut = Raster::new_depth(source.width, source.height, source.depth);
    for y in area[1].max(oy)..area[3].min(oy + source.height as i32) {
        for x in area[0].max(ox)..area[2].min(ox + source.width as i32) {
            if !crate::selection::contains(polygon, x as f32 + 0.5, y as f32 + 0.5) {
                continue;
            }
            let factor = coverage.map_or(1.0, |m| m.value(x, y));
            if factor <= 0.0 {
                continue;
            }
            let mut p = source.get16(x - ox, y - oy);
            let mut selected = p;
            selected[3] = (p[3] as f64 * factor as f64).round() as u16;
            p[3] = (p[3] as f64 * (1.0 - factor as f64)).round() as u16;
            if p[3] == 0 {
                p = [0; 4];
            }
            cut.set16(x - ox, y - oy, selected);
            source.set16(x - ox, y - oy, p);
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
            if cut.depth == 16 {
                let pixel = cut.sample16(ux, uy);
                if pixel[3] > 0 {
                    out.set16(
                        x - left,
                        y - top,
                        crate::raster::blend16(out.get16(x - left, y - top), pixel, 1.0, "normal"),
                    );
                }
                continue;
            }
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
    let shift_unedited = |source: &Raster| {
        let mut shifted = copy_shift(source, w, h, ox - left, oy - top);
        shifted.retained = source.retained.clone();
        if let Some(original) = &mut shifted.retained {
            original.matrix = crate::retained::compose(
                [1., 0., 0., 1., (ox - left) as f64, (oy - top) as f64],
                original.matrix,
            );
        }
        shifted
    };
    if let Some(m) = &mut layer.mask {
        if mask || expanded {
            m.cache_key = crate::engine::id();
        }
        if expanded {
            for step in &mut m.steps {
                step.pixels = shift_unedited(&step.pixels);
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
            layer.pixels = shift_unedited(&layer.pixels);
        }
    } else {
        layer.pixels = out;
        layer.kind = "paint".into();
    }
    layer.x = left;
    layer.y = top;
    if expanded {
        for effect in &mut layer.effects {
            if effect.kind == "liquify" {
                crate::geometry::map_effect(
                    &effect.kind,
                    &mut effect.settings,
                    [1., 1.],
                    [ox - left, oy - top],
                    true,
                )?;
            }
        }
    }
    Ok((b, transformed))
}

fn linked_selection(
    layer: &mut Layer,
    polygon: &[[f32; 2]],
    c: &Value,
    coverage: Option<&crate::selection::Coverage>,
) -> Result<([i32; 4], Vec<[f32; 2]>), String> {
    plain_mask(layer)?;
    let mut color = layer.clone();
    color.mask = None;
    let mut command = c.clone();
    command["mask"] = serde_json::json!(false);
    let result = selection_impl(&mut color, polygon, &command, coverage)?;
    let mut masked = layer.clone();
    mask_selection(&mut masked, polygon, &command, coverage)?;
    reframe_mask(
        &mut masked,
        color.x,
        color.y,
        color.pixels.width,
        color.pixels.height,
    )?;
    color.mask = masked.mask;
    *layer = color;
    Ok(result)
}

fn mask_selection(
    layer: &mut Layer,
    polygon: &[[f32; 2]],
    c: &Value,
    coverage: Option<&crate::selection::Coverage>,
) -> Result<([i32; 4], Vec<[f32; 2]>), String> {
    plain_mask(layer)?;
    let steps = layer
        .mask
        .as_ref()
        .ok_or("Layer has no mask")?
        .steps
        .iter()
        .filter(|step| step.kind == "paint")
        .map(|step| step.id.clone())
        .collect::<Vec<_>>();
    let work = steps.len() * (1 + layer.mask.as_ref().unwrap().steps.len());
    work_budget(layer.pixels.width, layer.pixels.height, work)?;
    let mut result = None;
    for step in steps {
        let mut command = c.clone();
        command["mask"] = serde_json::json!(true);
        command["step"] = serde_json::json!(step);
        result = Some(selection_impl(layer, polygon, &command, coverage)?);
        work_budget(layer.pixels.width, layer.pixels.height, work)?;
    }
    result.ok_or("Select a paint mask step to transform".into())
}

pub(crate) fn plain_mask(layer: &Layer) -> Result<(), String> {
    if layer.mask.as_ref().is_some_and(|mask| {
        mask.steps
            .iter()
            .any(|step| !matches!(step.kind.as_str(), "fill" | "paint"))
    }) {
        return Err("Independent or selected linked transforms require a fill/paint mask; mask effects are preserved unchanged. Unlink and choose a paint step for a mask-only transform.".into());
    }
    Ok(())
}

pub(crate) fn reframe_mask(
    layer: &mut Layer,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) -> Result<(), String> {
    if (layer.x, layer.y) == (x, y)
        && layer.mask.as_ref().is_none_or(|mask| {
            mask.steps
                .iter()
                .all(|step| (step.pixels.width, step.pixels.height) == (width, height))
        })
    {
        return Ok(());
    }
    if let Some(mask) = &mut layer.mask {
        work_budget(width, height, mask.steps.len())?;
        mask.cache_key = crate::engine::id();
        for step in &mut mask.steps {
            let mut original = crate::retained::Original::from(&step.pixels);
            original.matrix = crate::retained::compose(
                [
                    1.,
                    0.,
                    0.,
                    1.,
                    f64::from(layer.x - x),
                    f64::from(layer.y - y),
                ],
                original.matrix,
            );
            step.pixels = original.render(width, height)?;
        }
    }
    layer.x = x;
    layer.y = y;
    Ok(())
}

/// A tile writer avoids per-pixel tree mutation. Dense sources use contiguous reads.
pub(crate) fn resample(
    source: &Raster,
    w: u32,
    h: u32,
    fill: Option<crate::raster::Pixel>,
    map: impl Fn(u32, u32) -> [f32; 2],
) -> Raster {
    use crate::raster::{Pixel, TILE};
    use std::sync::Arc;
    if source.depth == 16 {
        let dense = if fill.is_none()
            && u64::from(w) * u64::from(h) >= 262144
            && u64::from(source.width) * u64::from(source.height) <= 16 * 1024 * 1024
            && source.samples16.len() * 8
                >= (source.width.div_ceil(TILE) * source.height.div_ceil(TILE)) as usize
        {
            Some(source.rgba16())
        } else {
            None
        };
        let mut out = Raster::new_depth(w, h, 16);
        let fill = fill.map(|p| p.map(|v| u16::from(v) * 257));
        for ty in 0..h.div_ceil(crate::raster::TILE) {
            for tx in 0..w.div_ceil(crate::raster::TILE) {
                let mut tile = vec![0u16; (crate::raster::TILE * crate::raster::TILE * 4) as usize];
                let mut nonempty = false;
                for y in 0..crate::raster::TILE.min(h - ty * crate::raster::TILE) {
                    for x in 0..crate::raster::TILE.min(w - tx * crate::raster::TILE) {
                        let [u, v] =
                            map(tx * crate::raster::TILE + x, ty * crate::raster::TILE + y);
                        let p = if fill.is_none() && u.fract() == 0.0 && v.fract() == 0.0 {
                            source.get16(u as i32, v as i32)
                        } else if let Some(color) = fill {
                            sample_fill16(source, u, v, color)
                        } else if let Some(words) = &dense {
                            sample_words16(words, source.width, source.height, u, v)
                        } else {
                            source.sample16(u, v)
                        };
                        let at = ((y * crate::raster::TILE + x) * 4) as usize;
                        tile[at..at + 4].copy_from_slice(&p);
                        nonempty |= p.iter().any(|v| *v != 0);
                    }
                }
                if nonempty {
                    out.samples16.insert((tx, ty), std::sync::Arc::new(tile));
                }
            }
        }
        return out;
    }
    let dense = if fill.is_none()
        && w as u64 * h as u64 >= 262144
        && source.width as u64 * source.height as u64 <= 16 * 1024 * 1024
        && source.tiles.len() * 8
            >= (source.width.div_ceil(TILE) * source.height.div_ceil(TILE)) as usize
    {
        Some(source.rgba())
    } else {
        None
    };
    let get = |x: i32, y: i32| -> Pixel {
        if let Some(bytes) = &dense {
            if x < 0 || y < 0 || x >= source.width as i32 || y >= source.height as i32 {
                return [0; 4];
            }
            let at = ((y as u32 * source.width + x as u32) * 4) as usize;
            bytes[at..at + 4].try_into().unwrap()
        } else {
            source.get(x, y)
        }
    };
    let mut out = Raster::new(w, h);
    for ty in 0..h.div_ceil(TILE) {
        for tx in 0..w.div_ceil(TILE) {
            let mut tile = vec![0; (TILE * TILE * 4) as usize];
            let mut nonzero = false;
            for y in 0..TILE.min(h - ty * TILE) {
                for x in 0..TILE.min(w - tx * TILE) {
                    let [ux, uy] = map(tx * TILE + x, ty * TILE + y);
                    let p = if fill.is_none() && ux.fract() == 0.0 && uy.fract() == 0.0 {
                        get(ux as i32, uy as i32)
                    } else if let Some(color) = fill {
                        if ux >= -0.5
                            && uy >= -0.5
                            && ux < source.width as f32 - 0.5
                            && uy < source.height as f32 - 0.5
                        {
                            color
                        } else {
                            [0; 4]
                        }
                    } else {
                        let ix = ux.floor() as i32;
                        let iy = uy.floor() as i32;
                        let (fx, fy) = (ux - ux.floor(), uy - uy.floor());
                        let mut sum = [0.0f32; 4];
                        for (dx, dy, weight) in [
                            (0, 0, (1. - fx) * (1. - fy)),
                            (1, 0, fx * (1. - fy)),
                            (0, 1, (1. - fx) * fy),
                            (1, 1, fx * fy),
                        ] {
                            let p = get(ix + dx, iy + dy);
                            let alpha = p[3] as f32 / 255.;
                            for c in 0..3 {
                                sum[c] += p[c] as f32 * alpha * weight
                            }
                            sum[3] += alpha * weight;
                        }
                        if sum[3] <= 0.00001 {
                            [0; 4]
                        } else {
                            [
                                (sum[0] / sum[3]).round() as u8,
                                (sum[1] / sum[3]).round() as u8,
                                (sum[2] / sum[3]).round() as u8,
                                (sum[3] * 255.).round() as u8,
                            ]
                        }
                    };
                    let at = ((y * TILE + x) * 4) as usize;
                    tile[at..at + 4].copy_from_slice(&p);
                    nonzero |= p != [0; 4];
                }
            }
            if nonzero {
                out.tiles.insert((tx, ty), Arc::new(tile));
            }
        }
    }
    out
}

#[cfg(test)]
mod resample_tests {
    use super::*;
    #[test]
    fn tiled_dense_and_sparse_sampling_match_original_at_edges_and_fractional_alpha() {
        for dense in [false, true] {
            let n = if dense { 520 } else { 35 };
            let mut source = Raster::new(n, n);
            for y in 0..n {
                for x in 0..n {
                    source.set(
                        x as i32,
                        y as i32,
                        [
                            (x % 251) as u8,
                            (y % 233) as u8,
                            91,
                            if (x + y) % 3 == 0 {
                                0
                            } else {
                                ((x * 17 + y * 11) % 256) as u8
                            },
                        ],
                    );
                }
            }
            let map = |x: u32, y: u32| [x as f32 * 0.93 - 2.7, y as f32 * 0.91 + 1.2];
            let transformed = resample(&source, n, n, None, map);
            for y in 0..n {
                for x in 0..n {
                    let [u, v] = map(x, y);
                    assert_eq!(transformed.get(x as i32, y as i32), source.sample(u, v));
                }
            }
            let shifted = resample(&source, 31, 29, Some([200, 50, 10, 127]), |x, y| {
                [x as f32 - 2., y as f32 - 1.]
            });
            assert_eq!(shifted.get(0, 0), [0; 4]);
            assert_eq!(shifted.get(2, 1), [200, 50, 10, 127]);
        }
    }
}

fn sample_fill16(source: &Raster, x: f32, y: f32, color: [u16; 4]) -> [u16; 4] {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = f64::from(x - x.floor());
    let fy = f64::from(y - y.floor());
    let mut weight = 0.0;
    for (dx, dy, w) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        if ix + dx >= 0
            && iy + dy >= 0
            && ix + dx < source.width as i32
            && iy + dy < source.height as i32
        {
            weight += w;
        }
    }
    if weight <= 0.0 {
        [0; 4]
    } else {
        [
            color[0],
            color[1],
            color[2],
            (f64::from(color[3]) * weight).round() as u16,
        ]
    }
}

fn sample_words16(words: &[u16], w: u32, h: u32, x: f32, y: f32) -> [u16; 4] {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = f64::from(x - x.floor());
    let fy = f64::from(y - y.floor());
    let mut sum = [0.0; 4];
    for (dx, dy, weight) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let (sx, sy) = (ix + dx, iy + dy);
        if sx < 0 || sy < 0 || sx >= w as i32 || sy >= h as i32 {
            continue;
        }
        let at = ((sy as u32 * w + sx as u32) * 4) as usize;
        let alpha = f64::from(words[at + 3]) / 65535.0;
        for c in 0..3 {
            sum[c] += f64::from(words[at + c]) * alpha * weight;
        }
        sum[3] += alpha * weight;
    }
    if sum[3] <= 0.0 {
        [0; 4]
    } else {
        [
            (sum[0] / sum[3]).round().clamp(0.0, 65535.0) as u16,
            (sum[1] / sum[3]).round().clamp(0.0, 65535.0) as u16,
            (sum[2] / sum[3]).round().clamp(0.0, 65535.0) as u16,
            (sum[3] * 65535.0).round().clamp(0.0, 65535.0) as u16,
        ]
    }
}

/// The shared whole-layer affine operation, including native mask sources.
pub(crate) fn layer(layer: &mut Layer, c: &Value) -> Result<(), String> {
    if c["mask"] == true && layer.mask.as_ref().is_none_or(|mask| !mask.linked) {
        return crate::retained::mask(layer, c);
    }
    if layer.mask.as_ref().is_some_and(|mask| !mask.linked) {
        plain_mask(layer)?;
        let mut color = layer.clone();
        color.mask = None;
        crate::retained::layer(&mut color, c)?;
        reframe_mask(
            layer,
            color.x,
            color.y,
            color.pixels.width,
            color.pixels.height,
        )?;
        color.mask = layer.mask.take();
        *layer = color;
        return Ok(());
    }
    crate::retained::layer(layer, c)
}
pub(crate) fn move_layer(layer: &mut Layer, c: &Value) -> Result<(), String> {
    let nx = layer.x as f64 + c["dx"].as_f64().unwrap_or(0.);
    let ny = layer.y as f64 + c["dy"].as_f64().unwrap_or(0.);
    if !nx.is_finite() || !ny.is_finite() || nx.abs() > 100000. || ny.abs() > 100000. {
        return Err("Move is outside the initial coordinate limits".into());
    }
    layer.x = nx.round() as i32;
    layer.y = ny.round() as i32;
    if layer.kind == "adjustment" && layer.mask.is_none() {
        layer.x = 0;
        layer.y = 0;
    }
    Ok(())
}

/// Includes hidden descendants so a later visibility change cannot leave old geometry behind.
pub fn tree_ids(doc: &Document, roots: &[String]) -> Vec<String> {
    let mut ids: std::collections::HashSet<_> = roots.iter().cloned().collect();
    for _ in 0..=16 {
        let before = ids.len();
        for layer in &doc.layers {
            if layer.parent.as_ref().is_some_and(|p| ids.contains(p)) {
                ids.insert(layer.id.clone());
            }
        }
        if ids.len() == before {
            break;
        }
    }
    doc.layers
        .iter()
        .filter(|l| ids.contains(&l.id))
        .map(|l| l.id.clone())
        .collect()
}
pub fn tree_bounds(doc: &Document, roots: &[String]) -> Option<[i32; 4]> {
    let ids = tree_ids(doc, roots);
    doc.layers
        .iter()
        .filter(|l| ids.contains(&l.id) && !["group", "adjustment"].contains(&l.kind.as_str()))
        .map(|l| {
            let p = l.pixels.content_bounds().unwrap_or([
                0,
                0,
                l.pixels.width as i32,
                l.pixels.height as i32,
            ]);
            [p[0] + l.x, p[1] + l.y, p[2] + l.x, p[3] + l.y]
        })
        .reduce(|a, b| {
            [
                a[0].min(b[0]),
                a[1].min(b[1]),
                a[2].max(b[2]),
                a[3].max(b[3]),
            ]
        })
}
pub(crate) fn folder(doc: &mut Document, root: &str, c: &Value) -> Result<(), String> {
    if c["mask"] == true {
        if doc.selection.is_some() && c["selection_only"] != false {
            let coverage = crate::selection::current(doc).ok_or("Selection no longer exists")?;
            let node = doc
                .layers
                .iter_mut()
                .find(|l| l.id == root)
                .ok_or("Folder no longer exists")?;
            let mut temporary = node.clone();
            temporary.kind = "paint".into();
            let transformed = selection_with_coverage(&mut temporary, &coverage, c)?;
            temporary.kind = "group".into();
            *node = temporary;
            doc.selection = Some(transformed.bounds);
            doc.selection_polygon = None;
            doc.selection_coverage = Some(transformed);
            return Ok(());
        }
        return layer(
            doc.layers
                .iter_mut()
                .find(|l| l.id == root)
                .ok_or("Folder no longer exists")?,
            c,
        );
    }
    let ids = tree_ids(doc, &[root.into()]);
    if doc.layers.iter().any(|l| ids.contains(&l.id) && l.locked) {
        return Err("A layer inside the folder is locked".into());
    }
    if doc.selection.is_some() && c["selection_only"] != false {
        let coverage = crate::selection::current(doc).ok_or("Selection no longer exists")?;
        let mut command = c.clone();
        if command["pivot"].is_null() {
            command["pivot"] = serde_json::json!([
                (coverage.bounds[0] as f64 + coverage.bounds[2] as f64) / 2.,
                (coverage.bounds[1] as f64 + coverage.bounds[3] as f64) / 2.
            ]);
        }
        // Every descendant uses the same document-space selection, including hidden
        // children. Container masks and hierarchy remain attached to their sources.
        let mut transformed = None;
        for node in &mut doc.layers {
            if ids.contains(&node.id) && node.kind != "group" {
                if !matches!(node.kind.as_str(), "paint" | "fill") || node.source.is_some() {
                    return Err("Rasterize editable text/vector or adjustment children before transforming selected folder pixels".into());
                }
                transformed = Some(selection_with_coverage(node, &coverage, &command)?);
            }
        }
        let transformed = transformed.ok_or("Folder has no raster content to transform")?;
        doc.selection = Some(transformed.bounds);
        doc.selection_polygon = None;
        doc.selection_coverage = Some(transformed);
        return Ok(());
    }
    let bounds =
        tree_bounds(doc, &[root.into()]).unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
    let mut command = c.clone();
    if command["pivot"].is_null() {
        command["pivot"] = serde_json::json!([
            (bounds[0] as f64 + bounds[2] as f64) / 2.,
            (bounds[1] as f64 + bounds[3] as f64) / 2.
        ]);
    }
    for node in &mut doc.layers {
        if ids.contains(&node.id) {
            layer(node, &command)?;
            node.effect_key = crate::engine::id();
        }
    }
    Ok(())
}
