//! Raster selections with soft edges, holes, and disjoint regions.
use super::*;
use crate::raster::{Raster, TILE};
fn num(v: &Value, key: &str, default: f64) -> f64 {
    v[key].as_f64().filter(|n| n.is_finite()).unwrap_or(default)
}
fn text<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v[key].as_str().unwrap_or(default)
}
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::Arc,
};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Coverage {
    pub mask: Raster,
    pub bounds: [i32; 4],
    #[serde(default)]
    pub origin: [i32; 2],
    pub contours: Vec<Vec<[f32; 2]>>,
}
impl Coverage {
    pub fn value(&self, x: i32, y: i32) -> f32 {
        self.mask.get(x - self.origin[0], y - self.origin[1])[0] as f32 / 255.0
    }
    pub fn local(&self, x: i32, y: i32) -> Self {
        let mut out = self.clone();
        out.origin[0] -= x;
        out.origin[1] -= y;
        out.bounds = [
            out.bounds[0] - x,
            out.bounds[1] - y,
            out.bounds[2] - x,
            out.bounds[3] - y,
        ];
        for contour in &mut out.contours {
            for p in contour {
                p[0] -= x as f32;
                p[1] -= y as f32;
            }
        }
        out
    }
}
pub fn current(doc: &Document) -> Option<Coverage> {
    if let Some(mask) = &doc.selection_coverage {
        if doc.selection == Some(mask.bounds) {
            return Some(mask.clone());
        }
    }
    let area = doc.selection?;
    let mut mask = Raster::new(doc.width, doc.height);
    let p = polygon(doc);
    for y in area[1].max(0)..area[3].min(doc.height as i32) {
        for x in area[0].max(0)..area[2].min(doc.width as i32) {
            if p.is_none_or(|p| contains(p, x as f32 + 0.5, y as f32 + 0.5)) {
                mask.set(x, y, [255; 4]);
            }
        }
    }
    Some(Coverage {
        mask,
        bounds: area,
        origin: [0, 0],
        contours: vec![p.map_or_else(|| rectangle(area), <[_]>::to_vec)],
    })
}
pub fn from_mask(mask: Raster) -> Result<Coverage, String> {
    finish(mask)
}
fn finish(mask: Raster) -> Result<Coverage, String> {
    let mut bounds = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for (&(tx, ty), tile) in &mask.tiles {
        for (i, p) in tile.chunks_exact(4).enumerate() {
            if p[0] > 0 {
                let x = (tx * TILE + i as u32 % TILE) as i32;
                let y = (ty * TILE + i as u32 / TILE) as i32;
                if x < mask.width as i32 && y < mask.height as i32 {
                    bounds = [
                        bounds[0].min(x),
                        bounds[1].min(y),
                        bounds[2].max(x + 1),
                        bounds[3].max(y + 1),
                    ];
                }
            }
        }
    }
    if bounds[0] >= bounds[2] {
        return Ok(Coverage {
            mask,
            bounds: [0; 4],
            origin: [0, 0],
            contours: vec![],
        });
    }
    let mut edges: HashMap<(i32, i32), Vec<(i32, i32)>> = HashMap::new();
    let inside = |x, y| mask.get(x, y)[0] >= 128;
    let mut count = 0usize;
    for y in bounds[1]..bounds[3] {
        for x in bounds[0]..bounds[2] {
            if inside(x, y) {
                for (present, a, b) in [
                    (inside(x, y - 1), (x, y), (x + 1, y)),
                    (inside(x + 1, y), (x + 1, y), (x + 1, y + 1)),
                    (inside(x, y + 1), (x + 1, y + 1), (x, y + 1)),
                    (inside(x - 1, y), (x, y + 1), (x, y)),
                ] {
                    if !present {
                        if count >= 500_000 {
                            return Err(
                                "Selection boundary exceeds 500000 edges; simplify the region"
                                    .into(),
                            );
                        }
                        edges.entry(a).or_default().push(b);
                        count += 1;
                    }
                }
            }
        }
    }
    let mut contours = vec![];
    while let Some(&start) = edges.keys().next() {
        let mut at = start;
        let mut line = vec![];
        loop {
            line.push([at.0 as f32, at.1 as f32]);
            let Some(next) = edges.get_mut(&at).and_then(|v| v.pop()) else {
                break;
            };
            if edges.get(&at).is_some_and(|v| v.is_empty()) {
                edges.remove(&at);
            }
            at = next;
            if at == start {
                break;
            }
            if line.len() > 500_000 {
                break;
            }
        }
        // Remove collinear vertices while retaining each independent boundary and hole.
        if line.len() >= 3 {
            let n = line.len();
            let simple = (0..n)
                .filter(|&i| {
                    let a = line[(i + n - 1) % n];
                    let b = line[i];
                    let c = line[(i + 1) % n];
                    (b[0] - a[0]) * (c[1] - b[1]) != (b[1] - a[1]) * (c[0] - b[0])
                })
                .map(|i| line[i])
                .collect::<Vec<_>>();
            if simple.len() >= 3 {
                contours.push(simple);
            }
        }
    }
    Ok(Coverage {
        mask,
        bounds,
        origin: [0, 0],
        contours,
    })
}
fn put(doc: &mut Document, mask: Raster) -> Result<(), String> {
    doc.selection_previous = current(doc).or_else(|| doc.selection_previous.clone());
    let coverage = finish(mask)?;
    doc.selection = Some(coverage.bounds);
    doc.selection_polygon = None;
    doc.selection_coverage = Some(coverage);
    Ok(())
}
fn snapshot(doc: &Document, c: &Value) -> Result<(u32, u32, Vec<u8>), String> {
    let target = if c["sample_merged"] == true {
        None
    } else {
        c["layer"].as_str()
    };
    let (w, h, p, _) = doc.preview(None, doc.width.max(doc.height), target, false)?;
    Ok((w, h, p))
}
fn difference(a: &[u8], b: &[u8]) -> f32 {
    let rgb = (0..3)
        .map(|i| (a[i] as f32 - b[i] as f32).powi(2))
        .sum::<f32>()
        .sqrt()
        / 3.0_f32.sqrt();
    rgb.max((a[3] as f32 - b[3] as f32).abs())
}
pub fn apply(doc: &mut Document, c: &Value) -> Result<(), String> {
    let kind = text(c, "kind", "rectangle");
    let op = text(c, "op", "selection");
    if op == "selection.reselect" {
        if let Some(previous) = doc.selection_previous.clone() {
            let now = current(doc);
            doc.selection = Some(previous.bounds);
            doc.selection_polygon = None;
            doc.selection_coverage = Some(previous);
            doc.selection_previous = now;
        }
        return Ok(());
    }
    if op == "selection.clear"
        || (op == "selection"
            && c["rect"].is_null()
            && c["polygon"].is_null()
            && c["kind"].is_null())
    {
        doc.selection_previous = current(doc).or_else(|| doc.selection_previous.clone());
        doc.selection = None;
        doc.selection_polygon = None;
        doc.selection_coverage = None;
        return Ok(());
    }
    if ![
        "rectangle",
        "ellipse",
        "lasso",
        "polygon",
        "magnetic",
        "wand",
        "quick",
        "object",
        "row",
        "column",
    ]
    .contains(&kind)
    {
        return Err("Unknown selection kind".into());
    }
    if !c["polygon"].is_null() {
        let points: Vec<[f32; 2]> = serde_json::from_value(c["polygon"].clone())
            .map_err(|_| "Selection polygon needs coordinate pairs")?;
        bounds(&points)?;
    } else if ["lasso", "polygon", "magnetic"].contains(&kind) {
        return Err("Lasso selections need polygon points".into());
    }
    if !c["rect"].is_null() {
        let a = c["rect"]
            .as_array()
            .filter(|a| a.len() == 4)
            .ok_or("Selection rectangle needs four integer coordinates")?;
        if a.iter()
            .any(|v| v.as_i64().is_none_or(|n| n.unsigned_abs() > 100000))
        {
            return Err("Invalid selection rectangle coordinates".into());
        }
    }
    let w = doc.width;
    let h = doc.height;
    if u64::from(w) * u64::from(h) > 16_000_000 {
        return Err("Raster selection work is limited to 16 megapixels".into());
    }
    let mut mask = Raster::new(w, h);
    let mut area = c["rect"]
        .as_array()
        .filter(|a| a.len() == 4)
        .map(|a| std::array::from_fn(|i| a[i].as_i64().unwrap_or(0) as i32))
        .unwrap_or([0, 0, w as i32, h as i32]);
    let prior = current(doc);
    if op == "selection.invert" || op == "selection.modify" {
        let radius = num(c, "radius", 1.0).round().clamp(1.0, 64.0) as usize;
        if op == "selection.invert" {
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let v = (255.0 * (1.0 - prior.as_ref().map_or(0.0, |m| m.value(x, y)))).round()
                        as u8;
                    if v > 0 {
                        mask.set(x, y, [v; 4]);
                    }
                }
            }
        } else {
            let expand = text(c, "mode", "expand") == "expand";
            let values = (0..h as i32)
                .flat_map(|y| {
                    let p = &prior;
                    (0..w as i32).map(move |x| {
                        (p.as_ref().map_or(0.0, |m| m.value(x, y)) * 255.0).round() as u8
                    })
                })
                .collect::<Vec<_>>();
            let mut tmp = vec![0; values.len()];
            let mut out = vec![0; values.len()];
            for y in 0..h as usize {
                let line = &values[y * w as usize..(y + 1) * w as usize];
                let result = morph(line, radius, expand);
                tmp[y * w as usize..(y + 1) * w as usize].copy_from_slice(&result);
            }
            for x in 0..w as usize {
                let line = (0..h as usize)
                    .map(|y| tmp[y * w as usize + x])
                    .collect::<Vec<_>>();
                let result = morph(&line, radius, expand);
                for (y, v) in result.into_iter().enumerate() {
                    out[y * w as usize + x] = v;
                }
            }
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    let v = out[(y as u32 * w + x as u32) as usize];
                    if v > 0 {
                        mask.set(x, y, [v; 4]);
                    }
                }
            }
        }
    } else if ["wand", "quick", "object"].contains(&kind) {
        let (_, _, pixels) = snapshot(doc, c)?;
        let point = c["point"].as_array().filter(|p| p.len() == 2);
        let at = point
            .map(|p| {
                [
                    p[0].as_f64().unwrap_or(0.0).floor() as i32,
                    p[1].as_f64().unwrap_or(0.0).floor() as i32,
                ]
            })
            .unwrap_or([(area[0] + area[2]) / 2, (area[1] + area[3]) / 2]);
        let valid = |x: i32, y: i32| x >= 0 && y >= 0 && x < w as i32 && y < h as i32;
        if !valid(at[0], at[1]) {
            return Err("Select inside the canvas".into());
        }
        let tolerance = num(c, "tolerance", 24.0).clamp(0.0, 255.0) as f32;
        let base = ((at[1] as u32 * w + at[0] as u32) * 4) as usize;
        let reference = &pixels[base..base + 4];
        if kind == "object" {
            // Deterministic foreground extraction from the region's perimeter colors.
            let a = [
                area[0].max(0),
                area[1].max(0),
                area[2].min(w as i32),
                area[3].min(h as i32),
            ];
            if a[0] >= a[2] || a[1] >= a[3] {
                return Err("Object region must intersect the canvas with positive area".into());
            }
            let corners = [
                [a[0], a[1]],
                [a[2] - 1, a[1]],
                [a[0], a[3] - 1],
                [a[2] - 1, a[3] - 1],
            ];
            for y in a[1]..a[3] {
                for x in a[0]..a[2] {
                    let i = ((y as u32 * w + x as u32) * 4) as usize;
                    if pixels[i + 3] > 0
                        && corners.iter().all(|p| {
                            let j = ((p[1] as u32 * w + p[0] as u32) * 4) as usize;
                            difference(&pixels[i..i + 4], &pixels[j..j + 4]) > tolerance
                        })
                    {
                        mask.set(x, y, [255; 4]);
                    }
                }
            }
        } else {
            let contiguous = c["contiguous"] != false;
            if !contiguous {
                for y in 0..h as i32 {
                    for x in 0..w as i32 {
                        let i = ((y as u32 * w + x as u32) * 4) as usize;
                        if difference(&pixels[i..i + 4], reference) <= tolerance {
                            mask.set(x, y, [255; 4]);
                        }
                    }
                }
            } else {
                let mut visited = vec![0u32; (w * h) as usize];
                let mut queue = VecDeque::new();
                let seeds: Vec<[f32; 2]> = serde_json::from_value(c["points"].clone())
                    .unwrap_or_else(|_| vec![[at[0] as f32, at[1] as f32]]);
                let radius = if kind == "quick" {
                    num(c, "radius", 12.0).clamp(1.0, 256.0) as i32
                } else {
                    0
                };
                if seeds.len() > 8192 {
                    return Err("Use at most 8192 selection brush seeds".into());
                }
                let mut work = 0usize;
                for (seed_number, seed) in seeds.into_iter().enumerate() {
                    let stamp = seed_number as u32 + 1;
                    let seed = [seed[0].floor() as i32, seed[1].floor() as i32];
                    if !valid(seed[0], seed[1]) {
                        continue;
                    }
                    let si = ((seed[1] as u32 * w + seed[0] as u32) * 4) as usize;
                    let seed_color = &pixels[si..si + 4];
                    queue.push_back(seed);
                    visited[(seed[1] as u32 * w + seed[0] as u32) as usize] = stamp;
                    while let Some([x, y]) = queue.pop_front() {
                        let i = (y as u32 * w + x as u32) as usize;
                        work += 1;
                        if work > 64_000_000 {
                            return Err(
                                "Selection brush work limit exceeded; use a shorter gesture".into(),
                            );
                        }
                        if difference(&pixels[i * 4..i * 4 + 4], seed_color) > tolerance {
                            continue;
                        }
                        // Quick Selection grows only around the painted region; Wand floods its component.
                        if kind == "quick"
                            && (x - seed[0]).pow(2) + (y - seed[1]).pow(2) > radius * radius
                        {
                            continue;
                        }
                        mask.set(x, y, [255; 4]);
                        for [nx, ny] in [[x - 1, y], [x + 1, y], [x, y - 1], [x, y + 1]] {
                            if valid(nx, ny) {
                                let ni = (ny as u32 * w + nx as u32) as usize;
                                if visited[ni] != stamp {
                                    visited[ni] = stamp;
                                    queue.push_back([nx, ny]);
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        let points: Option<Vec<[f32; 2]>> = serde_json::from_value(c["polygon"].clone()).ok();
        if let Some(p) = &points {
            area = bounds(p)?;
        }
        for y in area[1].max(0)..area[3].min(h as i32) {
            if let Some(points) = &points {
                let cy = y as f32 + 0.5;
                let mut crossings = Vec::new();
                for (a, b) in points
                    .iter()
                    .zip(points.iter().cycle().skip(1))
                    .take(points.len())
                {
                    if (a[1] > cy) != (b[1] > cy) {
                        crossings.push(a[0] + (b[0] - a[0]) * (cy - a[1]) / (b[1] - a[1]));
                    }
                }
                crossings.sort_by(f32::total_cmp);
                for pair in crossings.chunks_exact(2) {
                    for x in ((pair[0] - 0.5).ceil() as i32).max(0)
                        ..=((pair[1] - 0.5).floor() as i32).min(w as i32 - 1)
                    {
                        mask.set(x, y, [255; 4]);
                    }
                }
                continue;
            }
            for x in area[0].max(0)..area[2].min(w as i32) {
                let inside = if let Some(p) = &points {
                    contains(p, x as f32 + 0.5, y as f32 + 0.5)
                } else if kind == "ellipse" {
                    let rx = (area[2] - area[0]) as f32 / 2.0;
                    let ry = (area[3] - area[1]) as f32 / 2.0;
                    rx > 0.0
                        && ry > 0.0
                        && ((x as f32 + 0.5 - area[0] as f32 - rx) / rx).powi(2)
                            + ((y as f32 + 0.5 - area[1] as f32 - ry) / ry).powi(2)
                            <= 1.0
                } else {
                    true
                };
                if inside {
                    mask.set(x, y, [255; 4]);
                }
            }
        }
    }
    let feather = num(c, "feather", 0.0).round().clamp(0.0, 64.0) as i32;
    if feather > 0 {
        // Separable box filter, bounded O(pixels) for every radius.
        let mut horizontal = vec![0u8; (w * h) as usize];
        for y in 0..h as i32 {
            let mut sum = 0u32;
            for x in -feather..=feather {
                sum += mask.get(x, y)[0] as u32;
            }
            for x in 0..w as i32 {
                horizontal[(y as u32 * w + x as u32) as usize] =
                    (sum / (feather as u32 * 2 + 1)) as u8;
                sum = sum + mask.get(x + feather + 1, y)[0] as u32
                    - mask.get(x - feather, y)[0] as u32;
            }
        }
        let sample = |x: i32, y: i32| {
            if y >= 0 && y < h as i32 {
                horizontal[(y as u32 * w + x as u32) as usize] as u32
            } else {
                0
            }
        };
        let mut soft = Raster::new(w, h);
        for x in 0..w as i32 {
            let mut sum = 0;
            for y in -feather..=feather {
                sum += sample(x, y);
            }
            for y in 0..h as i32 {
                let v = (sum / (feather as u32 * 2 + 1)) as u8;
                if v > 0 {
                    soft.set(x, y, [v; 4]);
                }
                sum = sum + sample(x, y + feather + 1) - sample(x, y - feather);
            }
        }
        mask = soft;
    }
    let mode = if op == "selection.modify" {
        "replace"
    } else {
        text(c, "mode", "replace")
    };
    if mode != "replace" {
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let a = prior.as_ref().map_or(0.0, |m| m.value(x, y));
                let b = mask.get(x, y)[0] as f32 / 255.0;
                let v = match mode {
                    "add" => a.max(b),
                    "subtract" => a * (1.0 - b),
                    "intersect" => a.min(b),
                    _ => return Err("Choose replace, add, subtract or intersect".into()),
                };
                mask.set(x, y, [(v * 255.0).round() as u8; 4]);
            }
        }
    }
    put(doc, mask)
}
pub fn restrict_raster(after: &mut Raster, before: &Raster, coverage: &Coverage, origin: [i32; 2]) {
    let keys: BTreeSet<_> = if after.depth == 16 {
        after
            .samples16
            .keys()
            .chain(before.samples16.keys())
            .copied()
            .collect()
    } else {
        after
            .tiles
            .keys()
            .chain(before.tiles.keys())
            .copied()
            .collect()
    };
    for key in keys {
        let same = if after.depth == 16 {
            match (after.samples16.get(&key), before.samples16.get(&key)) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
        } else {
            match (after.tiles.get(&key), before.tiles.get(&key)) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                _ => false,
            }
        };
        if same {
            continue;
        }
        let (tx, ty) = key;
        let left = (tx * TILE) as i32;
        let top = (ty * TILE) as i32;
        for y in top..(top + TILE as i32).min(after.height as i32) {
            for x in left..(left + TILE as i32).min(after.width as i32) {
                let t = coverage.value(x + origin[0], y + origin[1]);
                if t == 1.0 {
                    continue;
                }
                let old = before.get16(x, y);
                let new = after.get16(x, y);
                if old == new {
                    continue;
                }
                let alpha = old[3] as f64 * (1.0 - t as f64) + new[3] as f64 * t as f64;
                let mut out = [0; 4];
                out[3] = alpha.round() as u16;
                for i in 0..3 {
                    out[i] = if alpha > 0.0 {
                        ((old[i] as f64 * old[3] as f64 * (1.0 - t as f64)
                            + new[i] as f64 * new[3] as f64 * t as f64)
                            / alpha)
                            .round() as u16
                    } else {
                        0
                    };
                }
                after.set16(x, y, out);
            }
        }
    }
}
pub fn restrict_document(after: &mut Document, before: &Document, coverage: &Coverage) {
    for layer in &mut after.layers {
        if let Some(old) = before.layers.iter().find(|l| l.id == layer.id) {
            restrict_raster(&mut layer.pixels, &old.pixels, coverage, [layer.x, layer.y]);
            if let Some(mask) = &mut layer.mask {
                for step in &mut mask.steps {
                    if let Some(previous) = old
                        .mask
                        .as_ref()
                        .and_then(|m| m.steps.iter().find(|s| s.id == step.id))
                    {
                        restrict_raster(
                            &mut step.pixels,
                            &previous.pixels,
                            coverage,
                            [layer.x, layer.y],
                        );
                    }
                }
            }
        } else {
            let empty =
                Raster::new_depth(layer.pixels.width, layer.pixels.height, layer.pixels.depth);
            restrict_raster(&mut layer.pixels, &empty, coverage, [layer.x, layer.y]);
        }
    }
}
pub fn magnetic_point(doc: &Document, point: [f32; 2], width: f32) -> [f32; 2] {
    let r = width.clamp(2.0, 32.0) as i32;
    let x = point[0] as i32;
    let y = point[1] as i32;
    let area = [
        (x - r).max(0),
        (y - r).max(0),
        (x + r + 1).min(doc.width as i32),
        (y + r + 1).min(doc.height as i32),
    ];
    let Ok((w, h, p, a)) = doc.preview(Some(area), (2 * r + 1) as u32, None, false) else {
        return point;
    };
    let lum = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        0.2126 * p[i] as f32 + 0.7152 * p[i + 1] as f32 + 0.0722 * p[i + 2] as f32
    };
    let mut best = point;
    let mut score = 8.0;
    for yy in 1..h.saturating_sub(1) {
        for xx in 1..w.saturating_sub(1) {
            let dx = a[0] as f32 + xx as f32 - point[0];
            let dy = a[1] as f32 + yy as f32 - point[1];
            let edge = ((lum(xx + 1, yy) - lum(xx - 1, yy)).powi(2)
                + (lum(xx, yy + 1) - lum(xx, yy - 1)).powi(2))
            .sqrt();
            let value = edge / (1.0 + 0.2 * (dx * dx + dy * dy).sqrt());
            if value > score {
                score = value;
                best = [a[0] as f32 + xx as f32, a[1] as f32 + yy as f32];
            }
        }
    }
    best
}

fn morph(line: &[u8], radius: usize, expand: bool) -> Vec<u8> {
    let mut queue: VecDeque<usize> = VecDeque::new();
    let mut out = vec![0; line.len()];
    let mut end = 0;
    for (i, value) in out.iter_mut().enumerate() {
        while end < (i + radius + 1).min(line.len()) {
            while queue.back().is_some_and(|&j| {
                if expand {
                    line[j] <= line[end]
                } else {
                    line[j] >= line[end]
                }
            }) {
                queue.pop_back();
            }
            queue.push_back(end);
            end += 1;
        }
        while queue.front().is_some_and(|&j| j < i.saturating_sub(radius)) {
            queue.pop_front();
        }
        *value = if !expand && (i < radius || i + radius >= line.len()) {
            0
        } else {
            line[*queue.front().unwrap()]
        };
    }
    out
}
impl Coverage {
    pub fn validate(&self) -> Result<(), String> {
        self.mask.validate_layout()?;
        if self.mask.depth != 8
            || self.mask.bytes() > 128 * 1024 * 1024
            || self.origin.iter().any(|v| v.unsigned_abs() > 100000)
            || self.bounds.iter().any(|v| v.unsigned_abs() > 100000)
            || self.bounds[0] > self.bounds[2]
            || self.bounds[1] > self.bounds[3]
            || self.contours.iter().map(Vec::len).sum::<usize>() > 500000
            || self
                .contours
                .iter()
                .flatten()
                .flatten()
                .any(|p| !p.is_finite() || p.abs() > 100000.0)
        {
            return Err("Invalid or oversized selection coverage".into());
        }
        Ok(())
    }
}
