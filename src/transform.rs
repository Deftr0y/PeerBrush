use crate::{
    engine::{Document, Layer},
    raster::{blend, check_size, Raster},
};
use serde_json::Value;
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
    selection_impl(layer, polygon, c, None)
}
pub fn selection_with_coverage(
    layer: &mut Layer,
    coverage: &crate::selection::Coverage,
    c: &Value,
) -> Result<crate::selection::Coverage, String> {
    if coverage.bounds[0] >= coverage.bounds[2] {
        return Ok(coverage.clone());
    }
    let (bounds, _) = selection_impl(
        layer,
        &crate::selection::rectangle(coverage.bounds),
        c,
        Some(coverage),
    )?;
    let n = |k: &str, d: f32| c[k].as_f64().unwrap_or(d as f64) as f32;
    let moving = c["op"] == "move";
    let (dx, dy) = if moving {
        (n("dx", 0.).round(), n("dy", 0.).round())
    } else {
        (0., 0.)
    };
    let (sx, sy) = if moving {
        (1., 1.)
    } else {
        (n("scale_x", 1.), n("scale_y", 1.))
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
            let v = coverage.mask.sample(ux, uy)[0];
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
        return Err("Select Color to transform the folder and its children".into());
    }
    if doc.selection.is_some() && c["selection_only"] != false {
        return Err(
            "Use selection_only:false for a whole-folder transform, or select a child layer".into(),
        );
    }
    let ids = tree_ids(doc, &[root.into()]);
    if doc.layers.iter().any(|l| ids.contains(&l.id) && l.locked) {
        return Err("A layer inside the folder is locked".into());
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
