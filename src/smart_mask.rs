use crate::{
    effects::Image,
    engine::{id, Layer, Mask, MaskStep},
    raster::Raster,
};
use serde_json::Value;
use std::collections::VecDeque;
pub(crate) struct Parameters<'a> {
    pub point: [u32; 2],
    pub tolerance: f64,
    pub contiguous: bool,
    pub mode: &'a str,
}
impl<'a> Parameters<'a> {
    pub fn new(c: &'a Value, origin: [i32; 2], size: [u32; 2]) -> Result<Self, String> {
        let mut point = [0; 2];
        for i in 0..2 {
            let value = c["point"][i].as_i64().ok_or(if i == 0 {
                "Missing mask point x"
            } else {
                "Missing mask point y"
            })?;
            point[i] = value
                .checked_sub(i64::from(origin[i]))
                .filter(|v| *v >= 0 && *v < i64::from(size[i]))
                .ok_or("Invalid smart-mask point or tolerance")? as u32;
        }
        let tolerance = c["tolerance"].as_f64().unwrap_or(0.12);
        if !tolerance.is_finite() || !(0.0..=1.0).contains(&tolerance) {
            return Err("Invalid smart-mask point or tolerance".into());
        }
        let mode = c["mode"].as_str().unwrap_or("replace");
        if !["replace", "add", "subtract"].contains(&mode) {
            return Err("Mask mode must be replace, add or subtract".into());
        }
        Ok(Self {
            point,
            tolerance,
            contiguous: c["contiguous"].as_bool().unwrap_or(true),
            mode,
        })
    }
    pub fn matches(&self, pixel: [u8; 4], seed: [u8; 4]) -> bool {
        (0..4)
            .map(|i| (f64::from(pixel[i]) - f64::from(seed[i])).powi(2))
            .sum::<f64>()
            .sqrt()
            / 510.0
            <= self.tolerance
    }
}
pub fn from_color(layer: &mut Layer, source: &Image, c: &Value) -> Result<(), String> {
    from_color_with_native(layer, source, c, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn mask_points_are_checked_before_local_coordinate_conversion() {
        for (point, origin) in [(i64::MIN, 2), (i64::MAX, -2)] {
            assert!(Parameters::new(&json!({"point":[point,0]}), [origin, 0], [4, 4]).is_err());
        }
        let command = json!({"point":[-2,3]});
        assert_eq!(
            Parameters::new(&command, [-2, 3], [4, 4]).unwrap().point,
            [0, 0]
        );
        assert!(Parameters::new(&json!({"point":[2,3]}), [-2, 3], [4, 4]).is_err());
    }
}
pub fn from_color_with_native(
    layer: &mut Layer,
    source: &Image,
    c: &Value,
    native: Option<&crate::depth16::Image16>,
) -> Result<(), String> {
    let parameters = Parameters::new(c, [layer.x, layer.y], [source.width, source.height])?;
    let [x, y] = parameters.point;
    let contiguous = parameters.contiguous;
    let mode = parameters.mode;
    let seed = source.get(x as i32, y as i32);
    let matches = |x: i32, y: i32| parameters.matches(source.get(x, y), seed);
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
        weight: 1.0,
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
            linked: true,
            enabled: true,
            cache_key: id(),
            steps: vec![
                MaskStep {
                    weight: 1.0,
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
