//! Whole-document geometry, shared by native controls, MCP and CLI.
use crate::{
    engine::{id, Document},
    raster::Raster,
    selection::Coverage,
};
use serde_json::{json, Value};
pub(crate) mod input;
pub(crate) mod native;

pub(crate) fn clipped_segment(a: [f32; 2], b: [f32; 2], bounds: [f32; 4]) -> Option<([f32; 2], [f32; 2])> {
    let delta = [b[0] - a[0], b[1] - a[1]];
    let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
    for (origin, d, min, max) in [
        (a[0], delta[0], bounds[0], bounds[2]),
        (a[1], delta[1], bounds[1], bounds[3]),
    ] {
        if d.abs() < 0.000001 {
            if origin < min || origin > max {
                return None;
            }
        } else {
            let (x, y) = ((min - origin) / d, (max - origin) / d);
            lo = lo.max(x.min(y));
            hi = hi.min(x.max(y));
        }
    }
    if hi < lo {
        return None;
    }
    Some((
        [a[0] + delta[0] * lo, a[1] + delta[1] * lo],
        [a[0] + delta[0] * hi, a[1] + delta[1] * hi],
    ))
}

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
pub fn plan(doc: &Document, c: &Value) -> Result<Plan, String> {
    let frame = input::Frame::new(
        [doc.width, doc.height],
        c,
        crate::raster::CanvasLimits::Resident,
    )?;
    let input::Frame {
        width,
        height,
        offset,
        scale,
        image,
        proportional,
    } = frame;
    let changing = offset != [0, 0] || scale != [1., 1.];
    let radius_scale = (scale[0] * scale[1]).sqrt();
    let mut filter_settings = vec![];
    for filter in &doc.filters {
        let mut settings = filter.settings.as_ref().clone();
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
        frame.origin([layer.x, layer.y])?;
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
            let mut settings = effect.settings.as_ref().clone();
            map_effect(
                &effect.kind,
                &mut settings,
                scale,
                if world || crate::effects::patterns::active(&effect.kind, &effect.settings) {
                    offset
                } else {
                    [0, 0]
                },
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
    let [w, h] = input::scaled_size(
        [source.width, source.height],
        scale,
        crate::raster::CanvasLimits::Resident,
    )?;
    Ok((w, h))
}
fn resized(source: &Raster, w: u32, h: u32) -> Raster {
    if (source.width, source.height) == (w, h) {
        return source.clone();
    }
    if source.bytes() == 0 {
        return Raster::new_depth(w, h, source.depth);
    }
    crate::transform::resample(source, w, h, None, |x, y| {
        [
            input::coordinate(x, source.width, w),
            input::coordinate(y, source.height, h),
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
            let value = input::coverage(x, y, [m.mask.width, m.mask.height], [w, h], |x, y| {
                Ok::<_, std::convert::Infallible>(m.mask.get(x, y)[0])
            })
            .unwrap();
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
    let spatial =
        matches!(kind, "blur" | "bloom" | "liquify") || crate::effects::styles::contains(kind);
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
    if crate::effects::patterns::active(kind, settings) {
        settings["bitmap_scale"] =
            json!(settings["bitmap_scale"].as_f64().unwrap_or(1.) * f64::from(scale[0]));
        for (axis, key) in ["phase_x", "phase_y"].into_iter().enumerate() {
            settings[key] = json!(
                settings[key].as_f64().unwrap_or(0.) * f64::from(scale[axis])
                    + f64::from(offset[axis])
            );
        }
    } else if crate::effects::styles::contains(kind) && scale != [1., 1.] {
        let defaults = crate::effects::styles::defaults(kind);
        for key in ["size", "soften", "distance", "scale"] {
            if key == "scale" && kind == "gradient_overlay" {
                continue;
            }
            if let Some(default) = defaults[key].as_f64() {
                settings[key] = json!(
                    settings[key].as_f64().unwrap_or(default)
                        * f64::from((scale[0] * scale[1]).sqrt())
                );
            }
        }
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
                let source = std::sync::Arc::make_mut(source);
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
            effect.settings = settings.clone().into();
        }
        layer.effect_key = id();
    }
    for (filter, settings) in doc.filters.iter_mut().zip(&p.filter_settings) {
        filter.settings = settings.clone().into();
    }
    doc.width = p.width;
    doc.height = p.height;
    doc.selection = None;
    doc.selection_polygon = None;
    doc.selection_coverage = None;
    doc.selection_previous = None;
    Ok(())
}
