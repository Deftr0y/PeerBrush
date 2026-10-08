use crate::{
    effects::Image,
    engine::{id, Layer, Mask, MaskStep},
    raster::Raster,
};
use serde_json::Value;
use std::collections::VecDeque;
pub fn from_color(layer: &mut Layer, source: &Image, c: &Value) -> Result<(), String> {
    from_color_with_native(layer, source, c, None)
}
pub fn from_color_with_native(
    layer: &mut Layer,
    source: &Image,
    c: &Value,
    native: Option<&crate::depth16::Image16>,
) -> Result<(), String> {
    let x = c["point"][0].as_i64().ok_or("Missing mask point x")? - layer.x as i64;
    let y = c["point"][1].as_i64().ok_or("Missing mask point y")? - layer.y as i64;
    let tolerance = c["tolerance"].as_f64().unwrap_or(0.12);
    if !tolerance.is_finite()
        || !(0.0..=1.0).contains(&tolerance)
        || x < 0
        || y < 0
        || x >= source.width as i64
        || y >= source.height as i64
    {
        return Err("Invalid smart-mask point or tolerance".into());
    }
    let contiguous = c["contiguous"].as_bool().unwrap_or(true);
    let mode = c["mode"].as_str().unwrap_or("replace");
    if !["replace", "add", "subtract"].contains(&mode) {
        return Err("Mask mode must be replace, add or subtract".into());
    }
    let seed = source.get(x as i32, y as i32);
    let matches = |x: i32, y: i32| {
        let p = source.get(x, y);
        let d = (0..4)
            .map(|i| {
                let delta = p[i] as f64 - seed[i] as f64;
                delta * delta
            })
            .sum::<f64>();
        d.sqrt() / 510.0 <= tolerance
    };
    let w = source.width as usize;
    let h = source.height as usize;
    let mut selected = vec![0u8; w * h];
    if contiguous {
        let mut queue = VecDeque::from([(x as i32, y as i32)]);
        selected[y as usize * w + x as usize] = 1;
        while let Some((x, y)) = queue.pop_front() {
            for (nx, ny) in [(x - 1, y), (x + 1, y), (x, y - 1), (x, y + 1)] {
                if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                    continue;
                }
                let at = ny as usize * w + nx as usize;
                if selected[at] != 0 {
                    continue;
                }
                selected[at] = 2;
                if matches(nx, ny) {
                    selected[at] = 1;
                    queue.push_back((nx, ny));
                }
            }
        }
    } else {
        for y in 0..h {
            for x in 0..w {
                if matches(x as i32, y as i32) {
                    selected[y * w + x] = 1;
                }
            }
        }
    }
    let prepared = layer
        .mask
        .as_ref()
        .and_then(|m| m.prepare(source.width, source.height));
    let mut pixels = Raster::new_depth(source.width, source.height, layer.pixels.depth);
    let maximum = if pixels.depth == 16 { 65535u16 } else { 255u16 };
    for y in 0..h {
        for x in 0..w {
            let hit = selected[y * w + x] == 1;
            let old = if mode == "replace" {
                0
            } else {
                if let Some(native) = native {
                    native.get(x as i32, y as i32)[0]
                } else {
                    (layer.mask_value_prepared(x as i32, y as i32, prepared.as_deref(), true)
                        * f32::from(maximum))
                    .round() as u16
                }
            };
            let value = match mode {
                "subtract" => {
                    if hit {
                        0
                    } else {
                        old
                    }
                }
                "add" => {
                    if hit {
                        maximum
                    } else {
                        old
                    }
                }
                _ => {
                    if hit {
                        maximum
                    } else {
                        0
                    }
                }
            };
            if value > 0 {
                if pixels.depth == 16 {
                    pixels.set16(x as i32, y as i32, [value, value, value, maximum]);
                } else {
                    pixels.set(
                        x as i32,
                        y as i32,
                        [value as u8, value as u8, value as u8, 255],
                    );
                }
            }
        }
    }
    // Add/subtract append a refinement to the existing stack; replace starts a fresh editable mask.
    let step = MaskStep {
        id: id(),
        kind: "paint".into(),
        enabled: true,
        value: 0.0,
        pixels,
        settings: Value::Null,
    };
    if mode != "replace" && layer.mask.is_some() {
        let mask = layer.mask.as_mut().unwrap();
        if mask.steps.len() >= 32 {
            return Err("Mask stack limit".into());
        }
        // An opaque refinement records both selected and deselected pixels, preserving earlier effects.
        let mut step = step;
        for y in 0..h {
            for x in 0..w {
                if step.pixels.depth == 16 {
                    let p = step.pixels.get16(x as i32, y as i32);
                    step.pixels
                        .set16(x as i32, y as i32, [p[0], p[0], p[0], 65535]);
                } else {
                    let p = step.pixels.get(x as i32, y as i32);
                    step.pixels.set(x as i32, y as i32, [p[0], p[0], p[0], 255]);
                }
            }
        }
        mask.steps.push(step);
        mask.cache_key = id();
        mask.enabled = true;
    } else {
        layer.mask = Some(Mask {
            enabled: true,
            cache_key: id(),
            steps: vec![
                MaskStep {
                    id: id(),
                    kind: "fill".into(),
                    enabled: true,
                    value: 0.0,
                    pixels: Raster::new_depth(source.width, source.height, layer.pixels.depth),
                    settings: Value::Null,
                },
                step,
            ],
        });
    }
    Ok(())
}
