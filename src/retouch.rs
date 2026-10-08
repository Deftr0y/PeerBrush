//! Snapshot-based clone and locally color-adapted healing, at the original channel depth.
use crate::{
    brush::{self, Settings},
    engine::{self, Document},
    raster::{blend16, Raster, TILE},
};
use serde_json::Value;

struct Source {
    pixels: Raster,
    x: i32,
    y: i32,
}
impl Source {
    fn sample(&self, x: f32, y: f32) -> [u16; 4] {
        self.pixels.sample16(x - self.x as f32, y - self.y as f32)
    }
}
fn source(doc: &Document, index: usize, c: &Value) -> Result<Source, String> {
    for field in ["mask", "sample_merged"] {
        if c.get(field).is_some_and(|v| !v.is_boolean()) {
            return Err(format!("{field} must be boolean"));
        }
    }
    let mask = c["mask"].as_bool().unwrap_or(false);
    let merged = c["sample_merged"].as_bool().unwrap_or(false);
    if c.get("source_step").is_some() && (!mask || merged) {
        return Err("Source steps require layer mask sampling".into());
    }
    if merged {
        if mask {
            return Err("Mask retouching samples a layer mask, not merged artwork".into());
        }
        if u64::from(doc.width) * u64::from(doc.height) > 16_000_000 {
            return Err("Merged retouch sampling is limited to 16 megapixels".into());
        }
        let pixels = if doc.bit_depth == 16 {
            let image = crate::depth16::render(doc)?;
            Raster::from_rgba16(image.width, image.height, &image.words)?
        } else {
            let (w, h, pixels, _) = doc.preview(None, doc.width.max(doc.height), None, false)?;
            Raster::from_rgba(w, h, &pixels)?
        };
        return Ok(Source { pixels, x: 0, y: 0 });
    }
    let index = if let Some(id) = c.get("source_layer") {
        let id = id.as_str().ok_or("Source layer must be an ID")?;
        doc.layers
            .iter()
            .position(|l| l.id == id)
            .ok_or("Unknown retouch source layer")?
    } else {
        index
    };
    let layer = &doc.layers[index];
    let pixels = if mask {
        let m = layer.mask.as_ref().ok_or("Source layer has no mask")?;
        if let Some(step) = c.get("source_step") {
            let step = step.as_str().ok_or("Source step must be an ID")?;
            m.steps
                .iter()
                .find(|s| s.id == step && s.kind == "paint")
                .ok_or("Unknown source paint mask step")?
                .pixels
                .clone()
        } else {
            let native = if doc.bit_depth == 16 {
                crate::depth16::mask_image(doc, index)?
            } else {
                None
            };
            let gray = m.prepare(layer.pixels.width, layer.pixels.height);
            let mut pixels =
                Raster::new_depth(layer.pixels.width, layer.pixels.height, doc.bit_depth);
            for y in 0..pixels.height as i32 {
                for x in 0..pixels.width as i32 {
                    let v = native.as_ref().map(|m| m.get(x, y)[0]).unwrap_or_else(|| {
                        (layer.mask_value_prepared(x, y, gray.as_deref(), true) * 65535.).round()
                            as u16
                    });
                    pixels.set16(x, y, [v, v, v, 65535]);
                }
            }
            pixels
        }
    } else {
        if layer.kind != "paint" {
            return Err("Choose a paint source layer or sample merged artwork".into());
        }
        layer.pixels.clone()
    };
    Ok(Source {
        pixels,
        x: layer.x,
        y: layer.y,
    })
}

/// Integral sums are tile-local, with a complete halo; native samples never pass through RGB8.
struct Mean {
    sums: Vec<[f64; 4]>,
    stride: usize,
    left: i32,
    top: i32,
}
impl Mean {
    fn new(area: [i32; 4], sample: impl Fn(i32, i32) -> [u16; 4]) -> Self {
        let w = (area[2] - area[0]) as usize;
        let h = (area[3] - area[1]) as usize;
        let stride = w + 1;
        let mut sums = vec![[0.; 4]; stride * (h + 1)];
        for y in 0..h {
            let mut row = [0.; 4];
            for x in 0..w {
                let pixel = sample(x as i32 + area[0], y as i32 + area[1]);
                let a = pixel[3] as f64 / 65535.;
                for channel in 0..4 {
                    row[channel] += if channel == 3 {
                        a
                    } else {
                        pixel[channel] as f64 * a
                    };
                    sums[(y + 1) * stride + x + 1][channel] =
                        row[channel] + sums[y * stride + x + 1][channel];
                }
            }
        }
        Self {
            sums,
            stride,
            left: area[0],
            top: area[1],
        }
    }
    fn at(&self, x: i32, y: i32, r: i32) -> Option<[f64; 3]> {
        let x0 = (x - r - self.left) as usize;
        let y0 = (y - r - self.top) as usize;
        let x1 = (x + r + 1 - self.left) as usize;
        let y1 = (y + r + 1 - self.top) as usize;
        let mut sum = [0.; 4];
        for (c, v) in sum.iter_mut().enumerate() {
            *v = self.sums[y1 * self.stride + x1][c]
                - self.sums[y0 * self.stride + x1][c]
                - self.sums[y1 * self.stride + x0][c]
                + self.sums[y0 * self.stride + x0][c];
        }
        (sum[3] > 0.).then(|| [sum[0] / sum[3], sum[1] / sum[3], sum[2] / sum[3]])
    }
}
pub fn apply(doc: &mut Document, index: usize, c: &Value) -> Result<(), String> {
    let anchor = c["source"]
        .as_array()
        .filter(|a| a.len() == 2)
        .ok_or("Set a clone/heal source [x,y] in document pixels")?;
    let coordinate = |v: &Value| {
        v.as_f64()
            .filter(|v| v.is_finite() && v.abs() <= 100000.)
            .map(|v| v as f32)
            .ok_or("Invalid retouch source coordinate")
    };
    let anchor = [coordinate(&anchor[0])?, coordinate(&anchor[1])?];
    let (mut points, pressures) = brush::points_from_command(c)?;
    let settings = Settings::from_command(c)?;
    let heal = c["op"] == "heal";
    let radius = match c.get("heal_radius") {
        Some(v) => v
            .as_u64()
            .filter(|r| (1..=64).contains(r))
            .ok_or("Healing radius must be 1–64 pixels")? as i32,
        None => settings.radius.round().clamp(1., 32.) as i32,
    };
    let offset = [anchor[0] - points[0][0], anchor[1] - points[0][1]];
    let source = source(doc, index, c)?;
    let layer = &doc.layers[index];
    if layer.kind != "paint" && c["mask"] != true {
        return Err("Retouch a paint layer or a paint mask step".into());
    }
    let origin = [layer.x, layer.y];
    let clip = doc.selection.map(|s| {
        [
            s[0] - origin[0],
            s[1] - origin[1],
            s[2] - origin[0],
            s[3] - origin[1],
        ]
    });
    let polygon = crate::selection::polygon(doc).map(|p| {
        p.iter()
            .map(|p| [p[0] - origin[0] as f32, p[1] - origin[1] as f32])
            .collect::<Vec<_>>()
    });
    for p in &mut points {
        p[0] -= origin[0] as f32;
        p[1] -= origin[1] as f32;
    }
    let layer = &mut doc.layers[index];
    if c["mask"] == true {
        if let Some(m) = &mut layer.mask {
            m.cache_key = engine::id();
        }
    }
    let raster = engine::edit_raster(layer, c)?;
    let coverage = brush::sampled_coverage(
        raster,
        &points,
        pressures.as_deref(),
        settings,
        clip,
        polygon.as_deref(),
    )?;
    let baseline = raster.clone();
    let sample = |x: i32, y: i32| {
        source.sample(
            x as f32 + origin[0] as f32 + offset[0],
            y as f32 + origin[1] as f32 + offset[1],
        )
    };
    for ((tx, ty), coverage) in coverage {
        let left = (tx * TILE) as i32;
        let top = (ty * TILE) as i32;
        let right = (left + TILE as i32).min(raster.width as i32);
        let bottom = (top + TILE as i32).min(raster.height as i32);
        let means = heal.then(|| {
            let area = [left - radius, top - radius, right + radius, bottom + radius];
            (
                Mean::new(area, |x, y| baseline.get16(x, y)),
                Mean::new(area, sample),
            )
        });
        for y in top..bottom {
            for x in left..right {
                let amount = coverage[((y - top) * TILE as i32 + x - left) as usize] as f64
                    / 65535.
                    * settings.opacity as f64;
                if amount <= 0. {
                    continue;
                }
                let old = baseline.get16(x, y);
                let mut sampled = sample(x, y);
                if sampled[3] == 0 {
                    continue;
                }
                let output = if let Some((destination, source)) = &means {
                    if old[3] == 0 {
                        continue;
                    }
                    let (Some(destination), Some(source)) =
                        (destination.at(x, y, radius), source.at(x, y, radius))
                    else {
                        continue;
                    };
                    let amount = amount * sampled[3] as f64 / 65535.;
                    let mut output = old;
                    for ch in 0..3 {
                        let corrected =
                            (sampled[ch] as f64 + destination[ch] - source[ch]).clamp(0., 65535.);
                        output[ch] =
                            (old[ch] as f64 + (corrected - old[ch] as f64) * amount).round() as u16;
                    }
                    output
                } else {
                    sampled[3] = (sampled[3] as f64 * amount).round() as u16;
                    blend16(old, sampled, 1., "normal")
                };
                if output != old {
                    raster.set16(x, y, output);
                }
            }
        }
    }
    Ok(())
}
