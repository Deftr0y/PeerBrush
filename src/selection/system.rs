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
    collections::HashMap,
    sync::Arc,
};
/// Runtime replay source, scoped to unchanged artwork and its original combination baseline.
#[derive(Clone, Debug)]
pub struct WandSource {
    pub id: String,
    pub revision: u64,
    pub command: Value,
    before: Option<Coverage>,
    previous: Option<Coverage>,
}
pub fn wand_source(doc: &Document) -> Option<&WandSource> {
    doc.wand_source
        .as_deref()
        .filter(|s| s.revision == doc.revision)
}
pub(crate) fn restore_wand_revision(doc: &mut Document, revision: u64) {
    if let Some(source) = &mut doc.wand_source {
        if source.revision == doc.revision {
            Arc::make_mut(source).revision = revision;
        }
    }
    doc.revision = revision;
}
fn update_wand(doc: &mut Document, c: &Value) -> Result<(), String> {
    let source = doc
        .wand_source
        .clone()
        .filter(|s| s.revision == doc.revision || s.revision == doc.revision.saturating_add(1))
        .ok_or("Click with the Magic Wand again after changing the artwork or selection")?;
    let mut command = source.command.clone();
    super::wand::update(&mut command, c)?;
    doc.selection = source.before.as_ref().map(|s| s.bounds);
    doc.selection_polygon = None;
    doc.selection_coverage = source.before.clone();
    doc.selection_previous = source.previous.clone();
    apply(doc, &command)?;
    let updated = Arc::make_mut(doc.wand_source.as_mut().ok_or("Wand source unavailable")?);
    updated.id = source.id.clone();
    Ok(())
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
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
pub(super) fn put(doc: &mut Document, mask: Raster) -> Result<(), String> {
    doc.selection_previous = current(doc).or_else(|| doc.selection_previous.clone());
    let coverage = finish(mask)?;
    doc.selection = Some(coverage.bounds);
    doc.selection_polygon = None;
    doc.selection_coverage = Some(coverage);
    Ok(())
}
pub(super) fn snapshot(doc: &Document, c: &Value) -> Result<(u32, u32, Vec<u8>), String> {
    let target = if c["sample_merged"] == true {
        None
    } else {
        c["layer"].as_str()
    };
    let (w, h, p, _) = doc.preview(None, doc.width.max(doc.height), target, false)?;
    Ok((w, h, p))
}
use super::sampling::difference;
pub fn apply(doc: &mut Document, c: &Value) -> Result<(), String> {
    if c["op"] == "selection.wand_update" {
        return update_wand(doc, c);
    }
    if c["op"] == "selection.import" {
        return super::import::apply(doc, c);
    }
    if c["op"] == "selection.refine" {
        return super::refine::apply(doc, c);
    }
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
    let area = c["rect"]
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
        let color_similarity = c
            .get("color_similarity")
            .map(|value| value.as_bool().ok_or("Color similarity must be boolean"))
            .transpose()?
            .unwrap_or(false);
        if color_similarity {
            if let Some(value) = c.get("tolerance") {
                value
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0. && *v <= 255.)
                    .ok_or("Similarity tolerance must be between 0 and 255")?;
            }
        }
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
                            difference(&pixels[i..i + 4], &pixels[j..j + 4], color_similarity)
                                > tolerance
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
                        if difference(&pixels[i..i + 4], reference, color_similarity) <= tolerance {
                            mask.set(x, y, [255; 4]);
                        }
                    }
                }
            } else {
                let seeds: Vec<[f32; 2]> = serde_json::from_value(c["points"].clone())
                    .unwrap_or_else(|_| vec![[at[0] as f32, at[1] as f32]]);
                let radius = if kind == "quick" {
                    num(c, "radius", 12.0).clamp(1.0, 256.0) as i32
                } else {
                    0
                };
                if seeds.len() > super::MAX_BRUSH_SEEDS {
                    return Err("Use at most 8192 selection brush seeds".into());
                }
                flood_mask(
                    &pixels,
                    &mut mask,
                    &seeds,
                    radius,
                    tolerance,
                    color_similarity,
                )?;
            }
        }
    } else {
        let points: Option<Vec<[f32; 2]>> = serde_json::from_value(c["polygon"].clone()).ok();
        let geometry = super::geometry::Geometry::new([w, h], c, points.as_deref())?;
        let mut crossings = Vec::with_capacity(geometry.points());
        let mut row = vec![0; w as usize];
        for y in 0..h {
            geometry.row(y, &mut row, &mut crossings);
            for (x, value) in row.iter().enumerate() {
                if *value > 0 {
                    mask.set(x as i32, y as i32, [*value; 4]);
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
    let source = if kind == "wand" && op == "selection" {
        let command = serde_json::to_value(super::wand::Command(c)).map_err(|e| e.to_string())?;
        Some(Arc::new(WandSource {
            id: crate::engine::id(),
            revision: doc.revision + 1,
            command,
            before: prior,
            previous: doc.selection_previous.clone(),
        }))
    } else {
        None
    };
    put(doc, mask)?;
    doc.wand_source = source;
    Ok(())
}
/// Existing resident admission only. The worker-facing flood core accepts
/// caller-owned charged buffers; this legacy path keeps its unchanged 16MP cap.
fn flood_mask(
    pixels: &[u8],
    mask: &mut Raster,
    seeds: &[[f32; 2]],
    radius: i32,
    tolerance: f32,
    color_similarity: bool,
) -> Result<(), String> {
    use super::flood::{Flood, Layout, MAX_FRONTIER};
    let (w, h) = (mask.width, mask.height);
    if (w as usize)
        .checked_mul(h as usize)
        .and_then(|n| n.checked_mul(4))
        != Some(pixels.len())
    {
        return Err("Selection source dimensions differ".into());
    }
    let shape = Layout::new(w, h).map_err(str::to_owned)?;
    let allocate = |length: usize| -> Result<Vec<u8>, String> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(length)
            .map_err(|_| "Selection workspace allocation failed")?;
        bytes.resize(length, 0);
        Ok(bytes)
    };
    let mut states = allocate(shape.states)?;
    let mut generations = allocate(shape.generations)?;
    let mut frontier = allocate(MAX_FRONTIER * 4)?;
    let points: Vec<_> = seeds
        .iter()
        .map(|seed| {
            let [x, y] = seed.map(|v| v.floor() as i32);
            if x >= 0 && y >= 0 && x < w as i32 && y < h as i32 {
                [x as u32, y as u32]
            } else {
                [u32::MAX; 2]
            }
        })
        .collect();
    let mut flood = Flood::new(
        shape,
        &mut states,
        &mut generations,
        &mut frontier,
        64_000_000,
    )
    .map_err(str::to_owned)?;
    flood
        .run(
            &points,
            (radius > 0).then_some(radius as u32),
            || Ok::<_, String>(()),
            |seed, x, y| {
                let point = points[seed];
                let base = ((point[1] * w + point[0]) * 4) as usize;
                let at = ((y * w + x) * 4) as usize;
                Ok(difference(
                    &pixels[at..at + 4],
                    &pixels[base..base + 4],
                    color_similarity,
                ) <= tolerance)
            },
            |y, left, right| {
                for x in left..right {
                    mask.set(x as i32, y as i32, [255; 4]);
                }
                Ok(())
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}
pub fn restrict_raster(after: &mut Raster, before: &Raster, coverage: &Coverage, origin: [i32; 2]) {
    for key in after.changed_tile_keys(before) {
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
    crate::render::magnetic::resident(doc, point, width, &mut || Ok(())).unwrap_or(point)
}

pub(super) fn morph<T: Copy + Ord + Default>(line: &[T], radius: usize, expand: bool) -> Vec<T> {
    let mut out = vec![T::default(); line.len()];
    let mut queue = vec![0; line.len()];
    super::morphology::line(line, radius, expand, &mut out, &mut queue);
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

#[cfg(test)]
mod flood_tests {
    use super::*;

    #[test]
    fn actual_resident_flood_mask_matches_independent_bfs_at_tile_edges_and_multiple_seed_colors() {
        let (width, height) = (257u32, 259u32);
        let pixels: Vec<u8> = (0..height)
            .flat_map(|y| {
                (0..width).flat_map(move |x| {
                    let value = if x < 128 {
                        [17, 179, 71, 255]
                    } else if y < 130 {
                        [91, 37, 203, 255]
                    } else {
                        [19, 181, 73, 127]
                    };
                    value
                })
            })
            .collect();
        let seeds = [
            [-1., 4.],
            [2., 2.],
            [256., 258.],
            [130., 2.],
            [500., 500.],
            [2., 2.],
        ];
        for radius in [0, 3, 256] {
            for similar in [false, true] {
                for tolerance in [0., 32., 255.] {
                    let mut expected = vec![false; (width * height) as usize];
                    for point in seeds {
                        let [sx, sy] = point.map(|p: f32| p.floor() as i32);
                        if sx < 0 || sy < 0 || sx >= width as i32 || sy >= height as i32 {
                            continue;
                        }
                        let at = ((sy as u32 * width + sx as u32) * 4) as usize;
                        let reference = &pixels[at..at + 4];
                        let mut seen = vec![false; expected.len()];
                        let mut queue = std::collections::VecDeque::from([[sx, sy]]);
                        while let Some([x, y]) = queue.pop_front() {
                            let index = (y as u32 * width + x as u32) as usize;
                            if seen[index] {
                                continue;
                            }
                            seen[index] = true;
                            if difference(&pixels[index * 4..index * 4 + 4], reference, similar)
                                > tolerance
                                || (radius > 0
                                    && (x - sx).pow(2) + (y - sy).pow(2) > radius * radius)
                            {
                                continue;
                            }
                            expected[index] = true;
                            for [nx, ny] in [[x - 1, y], [x + 1, y], [x, y - 1], [x, y + 1]] {
                                if nx >= 0 && ny >= 0 && nx < width as i32 && ny < height as i32 {
                                    queue.push_back([nx, ny]);
                                }
                            }
                        }
                    }
                    let mut actual = Raster::new(width, height);
                    flood_mask(&pixels, &mut actual, &seeds, radius, tolerance, similar).unwrap();
                    for y in 0..height {
                        for x in 0..width {
                            assert_eq!(actual.get(x as i32,y as i32),if expected[(y*width+x) as usize] {[255;4]} else {[0;4]},"radius={radius},similar={similar},tolerance={tolerance},pixel={x},{y}");
                        }
                    }
                }
            }
        }
    }
}
