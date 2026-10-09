//! Whole-document geometry, shared by native controls, MCP and CLI.
use crate::{
    engine::{id, Document},
    raster::{check_size, Raster},
    selection::Coverage,
};
use serde_json::{json, Value};

#[derive(Clone)]
pub struct Plan {
    pub width: u32,
    pub height: u32,
    pub offset: [i32; 2],
    pub scale: [f32; 2],
    pub image: bool,
    effect_settings: Vec<Vec<Value>>,
    filter_settings: Vec<Value>,
    mask_settings: Vec<Vec<Option<Value>>>,
    radius_scale: f32,
}
fn dimension(c: &Value, key: &str) -> Result<u32, String> {
    c[key]
        .as_u64()
        .filter(|v| *v <= 8192)
        .map(|v| v as u32)
        .ok_or_else(|| format!("{key} must be an integer from 1 to 8192"))
}
pub fn plan(doc: &Document, c: &Value) -> Result<Plan, String> {
    let image = matches!(c["op"].as_str(), Some("image.resize" | "resize"));
    let (width, height, offset) = if c["op"] == "crop" {
        let b = c["rect"]
            .as_array()
            .filter(|b| b.len() == 4)
            .ok_or("Crop needs [left,top,right,bottom]")?;
        let mut r = [0; 4];
        for (i, v) in b.iter().enumerate() {
            r[i] = v
                .as_i64()
                .filter(|v| (-100000..=100000).contains(v))
                .ok_or("Crop coordinates must be bounded integers")? as i32;
        }
        if r[0] >= r[2] || r[1] >= r[3] {
            return Err("Crop needs a positive rectangle".into());
        }
        ((r[2] - r[0]) as u32, (r[3] - r[1]) as u32, [-r[0], -r[1]])
    } else {
        let (w, h) = (dimension(c, "width")?, dimension(c, "height")?);
        let offset = if image {
            [0, 0]
        } else {
            let anchor = c.get("anchor").and_then(Value::as_str).unwrap_or("center");
            let (ax, ay) = match anchor {
                "top_left" => (0., 0.),
                "top" => (0.5, 0.),
                "top_right" => (1., 0.),
                "left" => (0., 0.5),
                "center" => (0.5, 0.5),
                "right" => (1., 0.5),
                "bottom_left" => (0., 1.),
                "bottom" => (0.5, 1.),
                "bottom_right" => (1., 1.),
                _ => return Err("Choose one of the nine canvas anchors".into()),
            };
            if c.get("anchor").is_some_and(|v| !v.is_string()) {
                return Err("Canvas anchor must be a name".into());
            }
            [
                ((w as f64 - doc.width as f64) * ax).round() as i32,
                ((h as f64 - doc.height as f64) * ay).round() as i32,
            ]
        };
        (w, h, offset)
    };
    check_size(width, height)?;
    let scale = if image {
        [
            width as f32 / doc.width as f32,
            height as f32 / doc.height as f32,
        ]
    } else {
        [1., 1.]
    };
    let changing = offset != [0, 0] || scale != [1., 1.];
    let proportional = !image
        || (height as f64 - width as f64 * doc.height as f64 / doc.width as f64).abs() <= 0.500001
        || (width as f64 - height as f64 * doc.width as f64 / doc.height as f64).abs() <= 0.500001;
    let radius_scale = (scale[0] * scale[1]).sqrt();
    let mut filter_settings = vec![];
    for filter in &doc.filters {
        let mut settings = filter.settings.clone();
        map_effect(&filter.kind, &mut settings, scale, offset, proportional)?;
        filter_settings.push(settings);
    }
    let mut effect_settings = vec![];
    let mut mask_settings = vec![];
    if changing && doc.layers.iter().any(|l| l.locked) {
        return Err("Unlock layers before moving or resizing the whole image".into());
    }
    let mut work = 0u64;
    for layer in &doc.layers {
        let x = layer.x as f64 * scale[0] as f64 + offset[0] as f64;
        let y = layer.y as f64 * scale[1] as f64 + offset[1] as f64;
        if x.abs() > 100000. || y.abs() > 100000. {
            return Err("Layer geometry would exceed coordinate limits".into());
        }
        if image {
            let (w, h) = scaled_size(&layer.pixels, scale)?;
            if layer.pixels.bytes() > 0 || layer.kind == "fill" {
                work += u64::from(w) * u64::from(h);
            }
            if let Some(mask) = &layer.mask {
                work += u64::from(w)
                    * u64::from(h)
                    * mask
                        .steps
                        .iter()
                        .filter(|s| s.kind == "paint" && s.pixels.bytes() > 0)
                        .count() as u64;
            }
            if work > 128_000_000 {
                return Err("Image resize exceeds the 128-million-sample work budget".into());
            }
        }
        let world = ["group", "adjustment"].contains(&layer.kind.as_str());
        let mut effects = vec![];
        for effect in &layer.effects {
            let mut settings = effect.settings.clone();
            map_effect(
                &effect.kind,
                &mut settings,
                scale,
                if world { offset } else { [0, 0] },
                proportional,
            )?;
            effects.push(settings);
        }
        effect_settings.push(effects);
        let mut masks = vec![];
        if let Some(mask) = &layer.mask {
            for step in &mask.steps {
                if image && step.kind == "blur" && !proportional {
                    return Err("Mask feather requires proportional image resizing".into());
                }
                if image
                    && step.kind == "blur"
                    && !(0.0..=64.0).contains(&(step.value * radius_scale))
                {
                    return Err("Resized mask feather exceeds supported radius".into());
                }
                if step.kind == "gaussian" {
                    let mut settings = step.settings.clone();
                    map_effect("blur", &mut settings, scale, [0, 0], proportional)?;
                    masks.push(Some(settings));
                } else {
                    masks.push(None);
                }
            }
        }
        mask_settings.push(masks);
    }
    Ok(Plan {
        width,
        height,
        offset,
        scale,
        image,
        effect_settings,
        filter_settings,
        mask_settings,
        radius_scale,
    })
}
fn scaled_size(source: &Raster, scale: [f32; 2]) -> Result<(u32, u32), String> {
    let w = (source.width as f64 * scale[0] as f64).round().max(1.) as u32;
    let h = (source.height as f64 * scale[1] as f64).round().max(1.) as u32;
    check_size(w, h)?;
    Ok((w, h))
}
fn resized(source: &Raster, w: u32, h: u32) -> Raster {
    if (source.width, source.height) == (w, h) {
        return source.clone();
    }
    if source.bytes() == 0 {
        return Raster::new_depth(w, h, source.depth);
    }
    let sx = source.width as f32 / w as f32;
    let sy = source.height as f32 / h as f32;
    crate::transform::resample(source, w, h, None, |x, y| {
        [
            ((x as f32 + 0.5) * sx - 0.5).clamp(0., source.width as f32 - 1.),
            ((y as f32 + 0.5) * sy - 0.5).clamp(0., source.height as f32 - 1.),
        ]
    })
}
fn mapped_coverage(m: &Coverage, scale: [f32; 2], offset: [i32; 2]) -> Result<Coverage, String> {
    if scale == [1., 1.] {
        return Ok(m.local(-offset[0], -offset[1]));
    }
    let (w, h) = scaled_size(&m.mask, scale)?;
    if u64::from(w) * u64::from(h) > 16_000_000 {
        return Err("Resized selection snapshot exceeds 16 megapixels".into());
    }
    let origin = [
        (m.origin[0] as f32 * scale[0]).round() as i32 + offset[0],
        (m.origin[1] as f32 * scale[1]).round() as i32 + offset[1],
    ];
    let mut pixels = Raster::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let fx = ((x as f64 + 0.5) * m.mask.width as f64 / w as f64 - 0.5)
                .clamp(0., m.mask.width as f64 - 1.);
            let fy = ((y as f64 + 0.5) * m.mask.height as f64 / h as f64 - 0.5)
                .clamp(0., m.mask.height as f64 - 1.);
            let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
            let (rx, ry) = (fx - fx.floor(), fy - fy.floor());
            let mut sum = 0.;
            for (dx, dy, weight) in [
                (0, 0, (1. - rx) * (1. - ry)),
                (1, 0, rx * (1. - ry)),
                (0, 1, (1. - rx) * ry),
                (1, 1, rx * ry),
            ] {
                sum += m.mask.get(
                    (ix + dx).min(m.mask.width as i32 - 1),
                    (iy + dy).min(m.mask.height as i32 - 1),
                )[0] as f64
                    * weight;
            }
            let value = sum.round() as u8;
            if value > 0 {
                pixels.set(x as i32, y as i32, [value; 4]);
            }
        }
    }
    Ok(crate::selection::from_mask(pixels)?.local(-origin[0], -origin[1]))
}
pub(crate) fn map_effect(
    kind: &str,
    settings: &mut Value,
    scale: [f32; 2],
    offset: [i32; 2],
    proportional: bool,
) -> Result<(), String> {
    let spatial = matches!(kind, "blur" | "bloom" | "liquify");
    if spatial && !proportional {
        return Err("Spatial effects require proportional image resizing; keep aspect ratio or remove the effect first".into());
    }
    if matches!(kind, "blur" | "bloom") && scale != [1., 1.] {
        let (key, default) = if kind == "blur" {
            ("radius", 8.)
        } else {
            ("spread", 12.)
        };
        settings[key] =
            json!(crate::effects::number(settings, key, default) * (scale[0] * scale[1]).sqrt());
    }
    if kind == "liquify" {
        if scale != [1., 1.] {
            settings["radius"] = json!(
                crate::effects::number(settings, "radius", 40.) * (scale[0] * scale[1]).sqrt()
            );
        }
        if let Some(strokes) = settings["strokes"].as_array_mut() {
            for stroke in strokes {
                for key in ["points", "polygon"] {
                    if let Some(points) = stroke[key].as_array_mut() {
                        for p in points {
                            for axis in 0..2 {
                                p[axis] = json!(
                                    p[axis].as_f64().ok_or("Invalid spatial control point")?
                                        * scale[axis] as f64
                                        + offset[axis] as f64
                                );
                            }
                        }
                    }
                }
                if scale != [1., 1.] && stroke.get("radius").is_some() {
                    stroke["radius"] = json!(
                        crate::effects::number(stroke, "radius", 40.)
                            * (scale[0] * scale[1]).sqrt()
                    );
                }
                if !stroke["coverage"].is_null() {
                    let m: Coverage = serde_json::from_value(stroke["coverage"].clone())
                        .map_err(|_| "Invalid liquify selection snapshot")?;
                    stroke["coverage"] = serde_json::to_value(mapped_coverage(&m, scale, offset)?)
                        .map_err(|e| e.to_string())?;
                }
                if let Some(p) = stroke.get("polygon").filter(|v| !v.is_null()) {
                    let points: Vec<[f32; 2]> =
                        serde_json::from_value(p.clone()).map_err(|_| "Invalid liquify polygon")?;
                    stroke["selection"] = json!(crate::selection::bounds(&points)?);
                } else if let Some(b) = stroke["selection"].as_array().cloned() {
                    let mapped: Vec<i32> = b
                        .iter()
                        .enumerate()
                        .map(|(i, v)| {
                            (v.as_f64().unwrap_or(0.) * scale[i % 2] as f64 + offset[i % 2] as f64)
                                .round() as i32
                        })
                        .collect();
                    if mapped[0] >= mapped[2] || mapped[1] >= mapped[3] {
                        return Err(
                            "Resize would collapse a liquify selection; enlarge the image".into(),
                        );
                    }
                    stroke["selection"] = json!(mapped);
                }
            }
        }
    }
    crate::effects::validate(kind, settings)
}
pub fn apply(doc: &mut Document, c: &Value) -> Result<(), String> {
    let p = plan(doc, c)?;
    if p.image && p.scale == [1., 1.] {
        return Ok(());
    }
    for (index, layer) in doc.layers.iter_mut().enumerate() {
        if p.image {
            let (w, h) = scaled_size(&layer.pixels, p.scale)?;
            layer.pixels = resized(&layer.pixels, w, h);
            if let Some(source) = &mut layer.source {
                source.prepend([p.scale[0], 0., 0., p.scale[1], 0., 0.]);
                layer.pixels = source.render(w, h, layer.pixels.depth)?;
            }
            if let Some(mask) = &mut layer.mask {
                mask.cache_key = id();
                for (step_index, step) in mask.steps.iter_mut().enumerate() {
                    step.pixels = resized(&step.pixels, w, h);
                    if step.kind == "blur" {
                        step.value *= p.radius_scale;
                    }
                    if step.kind == "gaussian" {
                        step.settings = p.mask_settings[index][step_index]
                            .clone()
                            .ok_or("Missing resized mask source")?;
                    }
                }
            }
        }
        layer.x = (layer.x as f64 * p.scale[0] as f64).round() as i32 + p.offset[0];
        layer.y = (layer.y as f64 * p.scale[1] as f64).round() as i32 + p.offset[1];
        for (effect, settings) in layer.effects.iter_mut().zip(&p.effect_settings[index]) {
            effect.settings = settings.clone();
        }
        layer.effect_key = id();
    }
    for (filter, settings) in doc.filters.iter_mut().zip(&p.filter_settings) {
        filter.settings = settings.clone();
    }
    doc.width = p.width;
    doc.height = p.height;
    doc.selection = None;
    doc.selection_polygon = None;
    doc.selection_coverage = None;
    doc.selection_previous = None;
    Ok(())
}
