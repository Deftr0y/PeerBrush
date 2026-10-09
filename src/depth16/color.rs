//! Native-16 editable effects. No 8-bit lookup table or temporary image enters this path.
use super::{Image16, Pixel16, WORKING_BUDGET};
use crate::{effects, liquify};
use serde_json::Value;
const MAX: f64 = 65535.0;
const PREMULT_MAX: f64 = MAX * MAX;
pub fn weighted_working_bytes(
    effect: &effects::Effect,
    width: u32,
    height: u32,
) -> Result<u64, String> {
    Ok(
        working_bytes(width, height, &effect.kind, &effect.settings)?
            + if effect.weight > 0.0 && effect.weight < 1.0 {
                u64::from(width) * u64::from(height) * 8
            } else {
                0
            },
    )
}
pub(crate) fn apply_effect(
    image: &mut Image16,
    effect: &effects::Effect,
    region_source: Option<u64>,
) -> Result<(), String> {
    effects::validate_weight(effect.weight)?;
    if !effect.enabled || effect.weight == 0.0 {
        return Ok(());
    }
    if weighted_working_bytes(effect, image.width, image.height)? > WORKING_BUDGET {
        return Err("Weighted native16 effect exceeds the bounded working budget".into());
    }
    let before = (effect.weight < 1.0).then(|| image.words.clone());
    if let Some(source_pixels) = region_source {
        apply_region(image, &effect.kind, &effect.settings, source_pixels)?;
    } else {
        apply(image, &effect.kind, &effect.settings)?;
    }
    if let Some(before) = before {
        for (source, target) in before.chunks_exact(4).zip(image.words.chunks_exact_mut(4)) {
            target.copy_from_slice(&effects::weighted_pixel(
                source.try_into().unwrap(),
                (&*target).try_into().unwrap(),
                effect.weight,
            ));
        }
    }
    Ok(())
}
fn number(settings: &Value, key: &str, default: f64) -> f64 {
    settings[key].as_f64().unwrap_or(default)
}
fn word(value: f64) -> u16 {
    value.round().clamp(0.0, MAX) as u16
}
pub(crate) fn map(kind: &str, settings: &Value, value: f64) -> f64 {
    match kind {
        "levels" => {
            let black = number(settings, "black", 0.0);
            let white = number(settings, "white", 1.0);
            ((value - black) / (white - black).max(0.01))
                .clamp(0.0, 1.0)
                .powf(1.0 / number(settings, "gamma", 1.0))
        }
        "curves" => effects::curve_value64(settings, value),
        "adjust" => ((value - 0.5) * number(settings, "contrast", 1.0)
            + 0.5
            + number(settings, "brightness", 0.0))
        .clamp(0.0, 1.0),
        "invert" => 1.0 - value,
        _ => value,
    }
}
fn luma(rgb: [f64; 3]) -> f64 {
    rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}
fn smooth(low: f64, high: f64, value: f64) -> f64 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn hsl(rgb: [f64; 3], settings: &Value) -> [f64; 3] {
    let maximum = rgb[0].max(rgb[1]).max(rgb[2]);
    let minimum = rgb[0].min(rgb[1]).min(rgb[2]);
    let delta = maximum - minimum;
    let mut light = (maximum + minimum) * 0.5;
    let hue = if delta <= f64::EPSILON {
        0.0
    } else if maximum == rgb[0] {
        ((rgb[1] - rgb[2]) / delta).rem_euclid(6.0)
    } else if maximum == rgb[1] {
        (rgb[2] - rgb[0]) / delta + 2.0
    } else {
        (rgb[0] - rgb[1]) / delta + 4.0
    };
    let saturation = if delta <= f64::EPSILON {
        0.0
    } else {
        delta / (1.0 - (2.0 * light - 1.0).abs()).max(f64::EPSILON)
    };
    let hue = (hue + number(settings, "hue", 0.0) / 60.0).rem_euclid(6.0);
    let saturation = (saturation * (1.0 + number(settings, "saturation", 0.0))).clamp(0.0, 1.0);
    let lightness = number(settings, "lightness", 0.0);
    light = if lightness < 0.0 {
        light * (1.0 + lightness)
    } else {
        light + (1.0 - light) * lightness
    };
    let c = (1.0 - (2.0 * light - 1.0).abs()) * saturation;
    let x = c * (1.0 - (hue.rem_euclid(2.0) - 1.0).abs());
    let m = light - c * 0.5;
    let color = match hue as usize {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    color.map(|v| v + m)
}
fn balance(rgb: [f64; 3], settings: &Value) -> [f64; 3] {
    let luminance = luma(rgb);
    let shadow = 1.0 - smooth(0.08, 0.48, luminance);
    let highlight = smooth(0.52, 0.92, luminance);
    let weights = [shadow, 1.0 - shadow - highlight, highlight];
    let bands = ["shadows", "midtones", "highlights"];
    let mut adjusted = std::array::from_fn(|c| {
        rgb[c]
            + 0.5
                * (0..3)
                    .map(|b| settings[bands[b]][c].as_f64().unwrap_or(0.0) * weights[b])
                    .sum::<f64>()
    });
    if settings["preserve_luminosity"].as_bool().unwrap_or(true) {
        let correction = luminance - luma(adjusted);
        for c in &mut adjusted {
            *c += correction;
        }
        let mut scale = 1.0_f64;
        for c in adjusted {
            let chroma = c - luminance;
            if chroma > 0.0 {
                scale = scale.min((1.0 - luminance) / chroma);
            } else if chroma < 0.0 {
                scale = scale.min(-luminance / chroma);
            }
        }
        for c in &mut adjusted {
            *c = luminance + (*c - luminance) * scale;
        }
    }
    adjusted
}
pub fn working_bytes(width: u32, height: u32, kind: &str, settings: &Value) -> Result<u64, String> {
    let pixels = u64::from(width) * u64::from(height);
    if matches!(kind, "blur" | "bloom") {
        Ok(pixels * 16 + 1040)
    } else if kind == "liquify" {
        liquify::working_bytes16(width, height, settings).map(|bytes| bytes as u64)
    } else {
        Ok(65536 * 2)
    }
}
fn validated(image: &Image16, kind: &str, settings: &Value) -> Result<Value, String> {
    let settings = effects::normalized(kind, settings)?;
    image.validate()?;
    if working_bytes(image.width, image.height, kind, &settings)? > WORKING_BUDGET {
        return Err("16-bit effect temporary buffers exceed the bounded budget".into());
    }
    Ok(settings)
}
pub fn apply(image: &mut Image16, kind: &str, settings: &Value) -> Result<(), String> {
    let settings = validated(image, kind, settings)?;
    if crate::gpu::try_apply16(image, kind, &settings) {
        return Ok(());
    }
    apply_cpu_prepared(image, kind, settings)
}
pub fn apply_cpu(image: &mut Image16, kind: &str, settings: &Value) -> Result<(), String> {
    let settings = validated(image, kind, settings)?;
    apply_cpu_prepared(image, kind, settings)
}
pub(crate) fn apply_region(
    image: &mut Image16,
    kind: &str,
    settings: &Value,
    source_pixels: u64,
) -> Result<(), String> {
    let settings = validated(image, kind, settings)?;
    if crate::gpu::try_apply16_region(image, kind, &settings, source_pixels) {
        return Ok(());
    }
    apply_cpu_prepared(image, kind, settings)
}
fn apply_cpu_prepared(image: &mut Image16, kind: &str, settings: Value) -> Result<(), String> {
    match kind {
        "channel_clamp" => {
            let (channel, minimum, maximum) =
                effects::channel_clamp::native_bounds(&settings, 65535)?;
            for pixel in image.words.chunks_exact_mut(4) {
                pixel[channel] = pixel[channel].clamp(minimum, maximum);
            }
            return Ok(());
        }
        "posterize" => {
            let levels = effects::posterize::levels(&settings)?;
            let table: Vec<u16> = (0..=65535u16)
                .map(|v| effects::posterize::sample(v, 65535, levels))
                .collect();
            for pixel in image.words.chunks_exact_mut(4) {
                for channel in &mut pixel[..3] {
                    *channel = table[*channel as usize];
                }
            }
            return Ok(());
        }
        "blur" => return blur(image, number(&settings, "radius", 8.0) as f32),
        "bloom" => return bloom(image, &settings),
        "liquify" => {
            let mapping = liquify::mapping16(image.width, image.height, &settings)?;
            let [left, top, right, bottom] = mapping.bounds();
            if right <= left || bottom <= top {
                return Ok(());
            }
            let width = (right - left) as usize;
            let mut output = vec![0u16; width * (bottom - top) as usize * 4];
            for y in top..bottom {
                for x in left..right {
                    let pixel = if let Some(position) = mapping.source_position(x, y) {
                        sample(image, position[0] as f64, position[1] as f64)
                    } else {
                        image.get(x, y)
                    };
                    let at = ((y - top) as usize * width + (x - left) as usize) * 4;
                    output[at..at + 4].copy_from_slice(&pixel);
                }
            }
            for y in top..bottom {
                let target = (y as usize * image.width as usize + left as usize) * 4;
                let start = (y - top) as usize * width * 4;
                image.words[target..target + width * 4]
                    .copy_from_slice(&output[start..start + width * 4]);
            }
            return Ok(());
        }
        _ => {}
    }
    let table: Vec<u16> = (0..65536)
        .map(|v| word(map(kind, &settings, v as f64 / MAX) * MAX))
        .collect();
    let saturation = number(&settings, "saturation", 1.0);
    for p in image.words.chunks_exact_mut(4) {
        if p[3] == 0 {
            continue;
        }
        match kind {
            "grayscale" => {
                let value = word(luma([p[0] as f64, p[1] as f64, p[2] as f64]));
                p[..3].fill(value);
            }
            "hsl" => {
                let out = hsl(
                    [p[0] as f64 / MAX, p[1] as f64 / MAX, p[2] as f64 / MAX],
                    &settings,
                );
                for c in 0..3 {
                    p[c] = word(out[c] * MAX);
                }
            }
            "color_balance" => {
                let out = balance(
                    [p[0] as f64 / MAX, p[1] as f64 / MAX, p[2] as f64 / MAX],
                    &settings,
                );
                for c in 0..3 {
                    p[c] = word(out[c] * MAX);
                }
            }
            _ => {
                for c in 0..3 {
                    p[c] = table[p[c] as usize];
                }
                if kind == "adjust" {
                    let gray = luma([p[0] as f64, p[1] as f64, p[2] as f64]);
                    for c in 0..3 {
                        p[c] = word(gray + (p[c] as f64 - gray) * saturation);
                    }
                }
            }
        }
    }
    Ok(())
}
// In-place sliding windows preserve originals in a radius-sized ring rather
// than allocating a second full image or a canvas-length scratch line.
fn boxes(pixels: &mut [u32], width: usize, height: usize, sigma: f32) {
    for radius in effects::gaussian_radii(sigma) {
        if radius == 0 {
            continue;
        }
        let mut ring = vec![0u32; radius + 1];
        let count = (radius * 2 + 1) as u64;
        for vertical in [false, true] {
            let (lines, length, stride) = if vertical {
                (width, height, width * 4)
            } else {
                (height, width, 4)
            };
            for line in 0..lines {
                let base = if vertical { line * 4 } else { line * width * 4 };
                for c in 0..4 {
                    let first = pixels[base + c];
                    let last = pixels[base + (length - 1) * stride + c];
                    let mut sum = (radius as u64 + 1) * u64::from(first);
                    for offset in 1..=radius {
                        sum += u64::from(pixels[base + offset.min(length - 1) * stride + c]);
                    }
                    for offset in 0..length {
                        ring[offset % (radius + 1)] = pixels[base + offset * stride + c];
                        pixels[base + offset * stride + c] = ((sum + count / 2) / count) as u32;
                        let add = if offset + radius + 1 >= length {
                            last
                        } else {
                            pixels[base + (offset + radius + 1) * stride + c]
                        };
                        let remove = if offset < radius {
                            first
                        } else {
                            ring[(offset - radius) % (radius + 1)]
                        };
                        sum += u64::from(add);
                        sum -= u64::from(remove);
                    }
                }
            }
        }
    }
}
fn blur(image: &mut Image16, sigma: f32) -> Result<(), String> {
    if sigma < 0.5 || image.width == 0 || image.height == 0 {
        return Ok(());
    }
    let mut pixels: Vec<u32> = Vec::with_capacity(image.words.len());
    for p in image.words.chunks_exact(4) {
        pixels.extend([
            u32::from(p[0]) * u32::from(p[3]),
            u32::from(p[1]) * u32::from(p[3]),
            u32::from(p[2]) * u32::from(p[3]),
            u32::from(p[3]) * 65535,
        ]);
    }
    boxes(
        &mut pixels,
        image.width as usize,
        image.height as usize,
        sigma,
    );
    for (p, v) in image.words.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
        p[3] = ((u64::from(v[3]) + 32767) / 65535).min(65535) as u16;
        for c in 0..3 {
            p[c] = if p[3] == 0 || v[3] == 0 {
                0
            } else {
                ((u64::from(v[c]) * 65535 + u64::from(v[3]) / 2) / u64::from(v[3])).min(65535)
                    as u16
            };
        }
    }
    Ok(())
}
fn bloom(image: &mut Image16, settings: &Value) -> Result<(), String> {
    let threshold = number(settings, "threshold", 0.75);
    let strength = number(settings, "strength", 0.5);
    if threshold == 1.0 || strength == 0.0 {
        return Ok(());
    }
    let mut glow = Vec::with_capacity(image.words.len());
    let mut emitted = false;
    for p in image.words.chunks_exact(4) {
        let emission = ((luma([p[0] as f64 / MAX, p[1] as f64 / MAX, p[2] as f64 / MAX])
            - threshold)
            / (1.0 - threshold).max(f64::EPSILON))
        .clamp(0.0, 1.0);
        let alpha = p[3] as f64 * MAX * emission;
        emitted |= alpha > 0.0;
        glow.extend(
            [
                p[0] as f64 * alpha / MAX,
                p[1] as f64 * alpha / MAX,
                p[2] as f64 * alpha / MAX,
                alpha,
            ]
            .map(|v| v.round() as u32),
        );
    }
    if !emitted {
        return Ok(());
    }
    boxes(
        &mut glow,
        image.width as usize,
        image.height as usize,
        number(settings, "spread", 12.0) as f32,
    );
    for (p, g) in image.words.chunks_exact_mut(4).zip(glow.chunks_exact(4)) {
        let base = p[3] as f64 / MAX;
        let alpha = base + (1.0 - base) * (g[3] as f64 / PREMULT_MAX * strength).clamp(0.0, 1.0);
        if alpha <= 0.0 {
            continue;
        }
        let out_alpha = word(alpha * MAX);
        for c in 0..3 {
            p[c] = if out_alpha == 0 {
                0
            } else {
                word(
                    ((p[c] as f64 / MAX * base + g[c] as f64 / PREMULT_MAX * strength).min(alpha)
                        / alpha)
                        * MAX,
                )
            };
        }
        p[3] = out_alpha;
    }
    Ok(())
}
pub fn sample(image: &Image16, x: f64, y: f64) -> Pixel16 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - ix as f64;
    let fy = y - iy as f64;
    let mut color = [0.0; 3];
    let mut alpha = 0.0;
    for (dx, dy, weight) in [
        (0, 0, (1.0 - fx) * (1.0 - fy)),
        (1, 0, fx * (1.0 - fy)),
        (0, 1, (1.0 - fx) * fy),
        (1, 1, fx * fy),
    ] {
        let p = image.get(ix + dx, iy + dy);
        let a = p[3] as f64 * weight;
        alpha += a;
        for c in 0..3 {
            color[c] += p[c] as f64 * a;
        }
    }
    if alpha <= f64::EPSILON {
        return [0; 4];
    }
    [
        word(color[0] / alpha),
        word(color[1] / alpha),
        word(color[2] / alpha),
        word(alpha),
    ]
}
