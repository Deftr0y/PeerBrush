//! Native-depth layer-style effects. Styles receive a derived document-coordinate frame.
use serde_json::{json, Value};

pub const KINDS: &[&str] = &[
    "bevel_emboss",
    "contour",
    "texture",
    "stroke",
    "inner_shadow",
    "inner_glow",
    "satin",
    "color_overlay",
    "gradient_overlay",
    "pattern_overlay",
    "outer_glow",
    "drop_shadow",
];
pub fn contains(kind: &str) -> bool {
    KINDS.contains(&kind)
}
pub fn pixel_aligned_shadow(kind: &str, value: &Value) -> bool {
    matches!(kind, "inner_shadow" | "drop_shadow") && value.get("pixel_snap").is_some()
}
pub fn knockout_shadow(kind: &str, value: &Value) -> bool {
    kind == "drop_shadow" && value.get("source_knockout").is_some()
}
pub fn separate_bevel_channels(kind: &str, value: &Value) -> bool {
    kind == "bevel_emboss"
        && [
            "highlight_opacity",
            "shadow_opacity",
            "highlight_blend",
            "shadow_blend",
        ]
        .iter()
        .any(|key| value.get(key).is_some())
}
pub fn bitmap_defaults() -> Value {
    json!({"bitmap_png":"", "bitmap_scale":1., "bitmap_angle":0., "phase_x":0., "phase_y":0., "sampling":"bilinear"})
}
pub fn defaults(kind: &str) -> Value {
    let mut v = json!({"opacity":0.75,"blend":"normal"});
    match kind {
        "color_overlay" => {
            v["color"] = json!([0.9, 0.25, 0.08]);
        }
        "gradient_overlay" => {
            v["color"] = json!([0.05, 0.15, 0.8]);
            v["color2"] = json!([1., 0.6, 0.1]);
            v["angle"] = json!(90.);
            v["scale"] = json!(1.);
            v["style"] = json!("linear");
            v["reverse"] = json!(false);
        }
        "pattern_overlay" | "texture" => {
            v["color"] = json!([0.1, 0.1, 0.1]);
            v["color2"] = json!([0.9, 0.9, 0.9]);
            v["pattern"] = json!("checker");
            v["scale"] = json!(8.);
            if kind == "texture" {
                v["angle"] = json!(120.);
                v["depth"] = json!(0.4);
            }
            v["invert"] = json!(false);
        }
        "bevel_emboss" => {
            v["size"] = json!(5.);
            v["soften"] = json!(1.);
            v["depth"] = json!(1.);
            v["angle"] = json!(120.);
            v["altitude"] = json!(30.);
            v["style"] = json!("inner");
            v["down"] = json!(false);
            v["color"] = json!([1., 1., 1.]);
            v["color2"] = json!([0., 0., 0.]);
            v["contour"] = json!("linear");
            v["highlight_opacity"] = json!(1.);
            v["shadow_opacity"] = json!(1.);
            v["highlight_blend"] = json!("inherit");
            v["shadow_blend"] = json!("inherit");
        }
        "contour" => {
            v["contour"] = json!("cone");
            v["range"] = json!(1.);
            v["invert"] = json!(false);
        }
        "stroke" => {
            v["size"] = json!(3.);
            v["position"] = json!("outside");
            v["color"] = json!([0., 0., 0.]);
        }
        "satin" => {
            v["color"] = json!([0., 0., 0.]);
            v["size"] = json!(8.);
            v["distance"] = json!(12.);
            v["angle"] = json!(19.);
            v["contour"] = json!("cone");
            v["invert"] = json!(false);
            v["blend"] = json!("multiply");
        }
        "inner_glow" | "outer_glow" => {
            v["color"] = json!([1., 0.85, 0.35]);
            v["size"] = json!(8.);
            v["spread"] = json!(0.);
            v["contour"] = json!("linear");
            if kind == "inner_glow" {
                v["source"] = json!("edge");
            }
            if kind == "outer_glow" {
                v.as_object_mut().unwrap().remove("blend");
            } else {
                v["blend"] = json!("screen");
            }
        }
        "inner_shadow" | "drop_shadow" => {
            v["color"] = json!([0., 0., 0.]);
            v["size"] = json!(5.);
            v["distance"] = json!(8.);
            v["angle"] = json!(120.);
            v["spread"] = json!(0.);
            v["contour"] = json!("linear");
            v["pixel_snap"] = json!(false);
            if kind == "drop_shadow" {
                v.as_object_mut().unwrap().remove("blend");
                v["source_knockout"] = json!(false);
            } else {
                v["blend"] = json!("multiply");
            }
        }
        _ => {}
    }
    v
}
pub fn validate(kind: &str, value: &Value) -> Result<(), String> {
    if !contains(kind) {
        return Err("Unsupported layer-style effect".into());
    }
    let defaults = defaults(kind);
    let bitmap = crate::effects::patterns::active(kind, value);
    let bitmap_defaults = bitmap_defaults();
    for (key, v) in value
        .as_object()
        .ok_or("Layer-style settings must be an object")?
    {
        if defaults.get(key).is_none()
            && !(bitmap && bitmap_defaults.get(key).is_some())
            && !(kind == "gradient_overlay"
                && matches!(
                    key.as_str(),
                    "color_stops" | "alpha_stops" | "offset_x" | "offset_y"
                ))
            && !(crate::effects::contours::contains(kind) && key == "contour_points")
        {
            return Err(format!("Unsupported {kind} setting: {key}"));
        }
        match key.as_str() {
            "contour_points" => {
                crate::effects::contours::Curve::decode(v)?;
            }
            "color_stops" | "alpha_stops" => {
                crate::effects::gradient::validate(v, key == "alpha_stops")?
            }
            "color" | "color2" => {
                let rgb = v
                    .as_array()
                    .filter(|a| a.len() == 3)
                    .ok_or("Style colors require three native-normalized RGB values")?;
                if rgb.iter().any(|c| {
                    c.as_f64()
                        .is_none_or(|n| !n.is_finite() || !(0.0..=1.0).contains(&n))
                }) {
                    return Err("Style RGB values must be finite within 0–1".into());
                }
            }
            "blend" | "highlight_blend" | "shadow_blend" => {
                if !crate::raster::BLENDS
                    .iter()
                    .any(|(name, _)| Some(*name) == v.as_str())
                    && !(key != "blend" && v == "inherit")
                {
                    return Err("Unsupported style blend mode".into());
                }
            }
            "reverse" | "invert" | "down" | "pixel_snap" | "source_knockout" => {
                if !v.is_boolean() {
                    return Err(format!("{key} must be true or false"));
                }
            }
            "contour" => choice(v, &["linear", "cone", "ring", "smooth", "custom"], key)?,
            "pattern" if matches!(kind, "pattern_overlay" | "texture") => {
                choice(v, &["checker", "stripes", "dots", "noise", "bitmap"], key)?
            }
            "pattern" => choice(v, &["checker", "stripes", "dots", "noise"], key)?,
            "bitmap_png" => {
                crate::effects::patterns::get(
                    v.as_str().ok_or("bitmap_png must be base64 PNG text")?,
                )?;
            }
            "sampling" => choice(v, &["bilinear", "nearest"], key)?,
            "position" => choice(v, &["inside", "outside", "center"], key)?,
            "source" => choice(v, &["edge", "center"], key)?,
            "style" if kind == "bevel_emboss" => {
                choice(v, &["inner", "outer", "emboss", "pillow"], key)?
            }
            "style" => choice(
                v,
                &["linear", "radial", "angle", "reflected", "diamond"],
                key,
            )?,
            _ => {
                let (min, max) = match key.as_str() {
                    "opacity" | "spread" | "highlight_opacity" | "shadow_opacity" => (0., 1.),
                    "size" | "soften" => (0., 64.),
                    "distance" => (0., 128.),
                    "angle" => (-180., 180.),
                    "altitude" => (0., 90.),
                    "range" => (0.01, 1.),
                    "scale" if kind == "gradient_overlay" => (0.1, 10.),
                    "offset_x" | "offset_y" => (-1., 1.),
                    "scale" => (1., 128.),
                    "depth" => (0., 10.),
                    "bitmap_scale" => (0.01, 8.),
                    "bitmap_angle" => (-180., 180.),
                    "phase_x" | "phase_y" => (-32768., 32768.),
                    _ => return Err("Unknown numeric style parameter".into()),
                };
                if v.as_f64()
                    .is_none_or(|n| !n.is_finite() || n < min || n > max)
                {
                    return Err(format!("{key} must be {min}–{max}"));
                }
            }
        }
    }
    if value["contour"] == "custom" {
        crate::effects::contours::Curve::decode(&value["contour_points"])?;
    } else if value.get("contour_points").is_some() {
        return Err("Contour points require the Custom contour".into());
    }
    if bitmap {
        crate::effects::patterns::get(
            value["bitmap_png"]
                .as_str()
                .ok_or("Bitmap pattern requires bitmap_png")?,
        )?;
        for key in ["color", "color2", "scale", "invert"] {
            if kind == "texture" && matches!(key, "color" | "color2") {
                continue;
            }
            if value.get(key).is_some_and(|v| *v != defaults[key]) {
                return Err(format!("{key} applies only to procedural patterns"));
            }
        }
    }
    Ok(())
}
fn choice(value: &Value, allowed: &[&str], key: &str) -> Result<(), String> {
    if allowed.contains(&value.as_str().unwrap_or("")) {
        Ok(())
    } else {
        Err(format!("Unsupported style {key}"))
    }
}
fn number(v: &Value, key: &str) -> f64 {
    v[key].as_f64().unwrap_or(0.)
}
fn word(x: f64) -> u16 {
    (x * 65535.).round().clamp(0., 65535.) as u16
}
fn rgb(v: &Value, key: &str) -> [u16; 3] {
    std::array::from_fn(|i| word(v[key][i].as_f64().unwrap_or(0.)))
}
fn profile(x: f64, key: &str) -> f64 {
    let x = x.clamp(0., 1.);
    match key {
        "cone" => 1. - (2. * x - 1.).abs(),
        "ring" => (std::f64::consts::PI * x).sin().powi(2),
        "smooth" => x * x * (3. - 2. * x),
        _ => x,
    }
}
pub fn working_bytes(width: u32, height: u32) -> u64 {
    u64::from(width) * u64::from(height) * 64 + u64::from(width.max(height)) * 64
}
/// Separable box smoothing with transparent samples outside the frame, O(pixels).
fn smooth(values: &mut Vec<f64>, w: usize, h: usize, r: usize) {
    if r == 0 || w == 0 || h == 0 {
        return;
    }
    let mut out = vec![0.; values.len()];
    let span = (r * 2 + 1) as f64;
    for y in 0..h {
        let mut sum = (0..=r.min(w - 1)).map(|x| values[y * w + x]).sum::<f64>();
        for x in 0..w {
            out[y * w + x] = sum / span;
            if x >= r {
                sum -= values[y * w + x - r];
            }
            if x + r + 1 < w {
                sum += values[y * w + x + r + 1];
            }
        }
    }
    for x in 0..w {
        let mut sum = (0..=r.min(h - 1)).map(|y| out[y * w + x]).sum::<f64>();
        for y in 0..h {
            values[y * w + x] = sum / span;
            if y >= r {
                sum -= out[(y - r) * w + x];
            }
            if y + r + 1 < h {
                sum += out[(y + r + 1) * w + x];
            }
        }
    }
}
fn sample(v: &[f64], w: usize, h: usize, x: f64, y: f64) -> f64 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - f64::from(ix);
    let fy = y - f64::from(iy);
    let get = |x: i32, y: i32| {
        if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
            0.
        } else {
            v[y as usize * w + x as usize]
        }
    };
    get(ix, iy) * (1. - fx) * (1. - fy)
        + get(ix + 1, iy) * fx * (1. - fy)
        + get(ix, iy + 1) * (1. - fx) * fy
        + get(ix + 1, iy + 1) * fx * fy
}
fn pattern(x: f64, y: f64, v: &Value) -> f64 {
    let scale = number(v, "scale");
    let (x, y) = (x / scale, y / scale);
    let p = match v["pattern"].as_str().unwrap_or("checker") {
        "stripes" => {
            if x.rem_euclid(1.) < 0.5 {
                0.
            } else {
                1.
            }
        }
        "dots" => {
            if (x.rem_euclid(1.) - 0.5).hypot(y.rem_euclid(1.) - 0.5) < 0.3 {
                1.
            } else {
                0.
            }
        }
        "noise" => {
            let mut n = (x.floor() as i32 as u32).wrapping_mul(374761393)
                ^ (y.floor() as i32 as u32).wrapping_mul(668265263);
            n = (n ^ (n >> 13)).wrapping_mul(1274126177);
            f64::from(n ^ (n >> 16)) / f64::from(u32::MAX)
        }
        _ => ((x.floor() as i64 + y.floor() as i64).rem_euclid(2)) as f64,
    };
    if v["invert"] == true {
        1. - p
    } else {
        p
    }
}
/// Native words throughout; the byte adapter scales only the original byte values.
pub fn apply(
    words: &mut [u16],
    width: u32,
    height: u32,
    kind: &str,
    v: &Value,
) -> Result<(), String> {
    let resolved = crate::effects::normalized(kind, v)?;
    let v = &resolved;
    let (w, h) = (width as usize, height as usize);
    if w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(words.len()) {
        return Err("Style dimensions do not match native samples".into());
    }
    if words.is_empty() || number(v, "opacity") == 0. {
        return Ok(());
    }
    let source = words.to_vec();
    let alpha: Vec<f64> = source
        .chunks_exact(4)
        .map(|p| f64::from(p[3]) / 65535.)
        .collect();
    let mut field = alpha.clone();
    let size = number(v, "size");
    let distance = number(v, "distance");
    let offset = (number(v, "offset_x"), number(v, "offset_y"));
    let angle = number(v, "angle").to_radians();
    let direction = match number(v, "angle") {
        -180. | 180. => (-1., 0.),
        -90. => (0., -1.),
        0. => (1., 0.),
        90. => (0., 1.),
        _ => (angle.cos(), angle.sin()),
    };
    let (dx, dy) = if matches!(kind, "inner_shadow" | "drop_shadow") && v["pixel_snap"] == true {
        // Cast to whole pixels before sampling, using the light-angle form.
        // This follows Krita's PSD calculateOffset and retains its arithmetic
        // at half-pixel boundaries after converting native displacement angles.
        let light = if number(v, "angle") < 0. {
            number(v, "angle") + 180.
        } else {
            number(v, "angle") - 180.
        };
        let light = light.to_radians();
        (
            -(distance * light.cos()).round(),
            (distance * light.sin()).round(),
        )
    } else {
        (direction.0 * distance, -direction.1 * distance)
    };
    if matches!(
        kind,
        "inner_glow" | "outer_glow" | "inner_shadow" | "drop_shadow" | "satin" | "bevel_emboss"
    ) {
        for _ in 0..3 {
            smooth(&mut field, w, h, (size / 3.).ceil() as usize);
        }
    }
    if kind == "bevel_emboss" {
        smooth(&mut field, w, h, number(v, "soften").ceil() as usize);
    }
    let color = rgb(v, "color");
    let color2 = rgb(v, "color2");
    let opacity = number(v, "opacity");
    let bitmap = crate::effects::patterns::from_settings(kind, v)?;
    let blend = v["blend"].as_str().unwrap_or("normal");
    let channel_blend = |key: &str| {
        v[key]
            .as_str()
            .filter(|mode| *mode != "inherit")
            .unwrap_or(blend)
    };
    let highlight_blend = channel_blend("highlight_blend");
    let shadow_blend = channel_blend("shadow_blend");
    let curve = if v["contour"] == "custom" {
        Some(crate::effects::contours::Curve::decode(
            &v["contour_points"],
        )?)
    } else {
        None
    };
    let contour = |position| {
        curve.as_ref().map_or_else(
            || profile(position, v["contour"].as_str().unwrap_or("linear")),
            |curve| curve.sample(position),
        )
    };
    for y in 0..h {
        for x in 0..w {
            let at = (y * w + x) * 4;
            let original: [u16; 4] = source[at..at + 4].try_into().unwrap();
            let a = alpha[y * w + x];
            let mut tint = color;
            let mut coverage = 1.;
            let mut blend = blend;
            let mut behind = false;
            let mut outside = false;
            match kind {
                "gradient_overlay" => {
                    let scale = number(v, "scale");
                    let px = (x as f64 + 0.5) / w as f64 - 0.5 - offset.0;
                    let py = (y as f64 + 0.5) / h as f64 - 0.5 - offset.1;
                    let position = px * direction.0 - py * direction.1;
                    let mut t = match v["style"].as_str().unwrap_or("linear") {
                        "radial" => px.hypot(py) * 2. / scale,
                        "angle" => {
                            (py.atan2(px) + angle).rem_euclid(std::f64::consts::TAU)
                                / std::f64::consts::TAU
                        }
                        "reflected" => position.abs() * 2. / scale,
                        "diamond" => (px.abs() + py.abs()) / scale,
                        _ => 0.5 + position / scale,
                    }
                    .clamp(0., 1.);
                    if v["reverse"] == true {
                        t = 1. - t;
                    }
                    if crate::effects::gradient::extended(kind, v) {
                        (tint, coverage) = crate::effects::gradient::sample(v, t);
                    } else {
                        tint = std::array::from_fn(|c| {
                            ((1. - t) * f64::from(color[c]) + t * f64::from(color2[c])).round()
                                as u16
                        });
                    }
                }
                "pattern_overlay" => {
                    if let Some(bitmap) = &bitmap {
                        let sample = bitmap.sample(x as f64, y as f64, v);
                        tint = sample[..3].try_into().unwrap();
                        coverage = f64::from(sample[3]) / 65535.;
                    } else {
                        let t = pattern(x as f64, y as f64, v);
                        tint = std::array::from_fn(|c| {
                            ((1. - t) * f64::from(color[c]) + t * f64::from(color2[c])).round()
                                as u16
                        });
                    }
                }
                "texture" => {
                    let height = |x: f64, y: f64| {
                        if let Some(bitmap) = &bitmap {
                            let p = bitmap.sample(x, y, v);
                            let luma = (f64::from(p[0]) * 0.2126
                                + f64::from(p[1]) * 0.7152
                                + f64::from(p[2]) * 0.0722)
                                / 65535.;
                            // Transparent samples are neutral height, without projecting channels.
                            0.5 + (luma - 0.5) * f64::from(p[3]) / 65535.
                        } else {
                            pattern(x, y, v)
                        }
                    };
                    let light = (height(x as f64 + angle.cos(), y as f64 - angle.sin())
                        - height(x as f64 - angle.cos(), y as f64 + angle.sin()))
                        * number(v, "depth");
                    coverage = light.abs().min(1.);
                    tint = if light > 0. { color2 } else { color };
                }
                "contour" => {
                    let range = number(v, "range");
                    let luma = (f64::from(original[0]) * 0.2126
                        + f64::from(original[1]) * 0.7152
                        + f64::from(original[2]) * 0.0722)
                        / 65535.;
                    let mut mapped = contour(luma / range);
                    if v["invert"] == true {
                        mapped = 1. - mapped;
                    }
                    tint = [word(mapped); 3];
                }
                "outer_glow" => {
                    coverage =
                        contour((field[y * w + x] / (1. - number(v, "spread")).max(0.01)).min(1.))
                            * (1. - a);
                    behind = true;
                    outside = true;
                }
                "inner_glow" => {
                    coverage = if v["source"] == "center" {
                        field[y * w + x]
                    } else {
                        1. - field[y * w + x]
                    };
                    coverage = contour((coverage / (1. - number(v, "spread")).max(0.01)).min(1.));
                }
                "drop_shadow" => {
                    coverage = contour(
                        (sample(&field, w, h, x as f64 - dx, y as f64 - dy)
                            / (1. - number(v, "spread")).max(0.01))
                        .min(1.),
                    );
                    // Optional pre-composite knockout. The later behind blend
                    // is separate: neither step replaces original alpha/RGB.
                    // Legacy sources omit this setting and retain their casts.
                    if v["source_knockout"] == true {
                        coverage *= 1. - a;
                    }
                    behind = true;
                    outside = true;
                }
                "inner_shadow" => {
                    coverage = contour(
                        ((1. - sample(&field, w, h, x as f64 - dx, y as f64 - dy))
                            / (1. - number(v, "spread")).max(0.01))
                        .min(1.),
                    );
                }
                "satin" => {
                    coverage = contour(
                        (sample(&field, w, h, x as f64 + dx, y as f64 + dy)
                            - sample(&field, w, h, x as f64 - dx, y as f64 - dy))
                        .abs(),
                    );
                    if v["invert"] == true {
                        coverage = 1. - coverage;
                    }
                }
                "bevel_emboss" => {
                    let gx = sample(&field, w, h, x as f64 + 1., y as f64)
                        - sample(&field, w, h, x as f64 - 1., y as f64);
                    let gy = sample(&field, w, h, x as f64, y as f64 + 1.)
                        - sample(&field, w, h, x as f64, y as f64 - 1.);
                    let mut light = (gx * angle.cos() - gy * angle.sin())
                        * number(v, "depth")
                        * (size + 1.)
                        * number(v, "altitude").to_radians().cos();
                    if v["down"] == true {
                        light = -light;
                    }
                    if v["style"] == "pillow" {
                        light *= 2. * field[y * w + x] - 1.;
                    }
                    coverage = contour(light.abs().min(1.));
                    tint = if light >= 0. { color } else { color2 };
                    coverage *= number(
                        v,
                        if light >= 0. {
                            "highlight_opacity"
                        } else {
                            "shadow_opacity"
                        },
                    );
                    blend = if light >= 0. {
                        highlight_blend
                    } else {
                        shadow_blend
                    };
                    if matches!(v["style"].as_str(), Some("outer" | "emboss")) {
                        outside = true;
                        coverage *= if v["style"] == "outer" { 1. - a } else { 1. };
                    }
                }
                "stroke" => {
                    // A circular native-alpha outline is applied after the generic loop.
                    coverage = 0.;
                }
                "color_overlay" => {}
                _ => return Err("Unsupported layer-style effect".into()),
            }
            if kind == "stroke" {
                continue;
            }
            if !outside && original[3] == 0 {
                continue;
            }
            if coverage * opacity == 0. {
                continue;
            }
            let effect = [tint[0], tint[1], tint[2], word(coverage * opacity)];
            let mut result = if behind {
                crate::raster::blend16(effect, original, 1., "normal")
            } else if !outside {
                let opaque = [original[0], original[1], original[2], 65535];
                crate::raster::blend16(opaque, effect, 1., blend)
            } else {
                crate::raster::blend16(original, effect, 1., blend)
            };
            if !outside {
                result[3] = original[3];
            }
            words[at..at + 4].copy_from_slice(&result);
        }
    }
    if kind == "stroke" {
        stroke(words, &source, &alpha, w, h, v);
    }
    Ok(())
}
// Exact squared Euclidean distance transform, linear work per row/column.
fn distances(alpha: &[f64], w: usize, h: usize, inside: bool) -> Vec<f64> {
    let mut values: Vec<f64> = alpha
        .iter()
        .map(|a| if (*a > 0.5) == inside { 1e12 } else { 0. })
        .collect();
    let transform = |input: &[f64]| -> Vec<f64> {
        let n = input.len();
        let mut sites = vec![0usize; n];
        let mut bounds = vec![0.; n + 1];
        let mut k = 0;
        bounds[0] = f64::NEG_INFINITY;
        bounds[1] = f64::INFINITY;
        for q in 1..n {
            let mut split;
            loop {
                let p = sites[k];
                split = ((input[q] + (q * q) as f64) - (input[p] + (p * p) as f64))
                    / (2. * (q - p) as f64);
                if split > bounds[k] || k == 0 {
                    break;
                }
                k -= 1;
            }
            k += 1;
            sites[k] = q;
            bounds[k] = split;
            bounds[k + 1] = f64::INFINITY;
        }
        k = 0;
        let mut result = vec![0.; n];
        for (q, out) in result.iter_mut().enumerate() {
            while bounds[k + 1] < (q as f64) {
                k += 1;
            }
            let p = sites[k];
            *out = ((q as f64 - p as f64).powi(2) + input[p]).max(0.);
        }
        result
    };
    for row in values.chunks_exact_mut(w) {
        row.copy_from_slice(&transform(row));
    }
    let mut column = vec![0.; h];
    for x in 0..w {
        for y in 0..h {
            column[y] = values[y * w + x];
        }
        let result = transform(&column);
        for y in 0..h {
            let d = result[y].sqrt();
            values[y * w + x] = if inside {
                d.min((x + 1).min(w - x).min(y + 1).min(h - y) as f64)
            } else {
                d
            };
        }
    }
    values
}
fn stroke(words: &mut [u16], source: &[u16], alpha: &[f64], w: usize, h: usize, v: &Value) {
    let size = number(v, "size");
    if size == 0. {
        return;
    }
    let radius = if v["position"] == "center" {
        size * 0.5
    } else {
        size
    };
    let outer = distances(alpha, w, h, false);
    let inner = distances(alpha, w, h, true);
    let color = rgb(v, "color");
    let opacity = number(v, "opacity");
    let blend = v["blend"].as_str().unwrap_or("normal");
    for (index, pixel) in words.chunks_exact_mut(4).enumerate() {
        let original: [u16; 4] = source[index * 4..index * 4 + 4].try_into().unwrap();
        let a = alpha[index];
        let coverage = match v["position"].as_str().unwrap_or("outside") {
            "inside" => (radius + 0.5 - inner[index]).clamp(0., 1.) * a,
            "center" => {
                (radius + 0.5 - outer[index]).clamp(0., 1.) * (1. - a)
                    + (radius + 0.5 - inner[index]).clamp(0., 1.) * a
            }
            _ => (radius + 0.5 - outer[index]).clamp(0., 1.) * (1. - a),
        };
        if coverage == 0. {
            continue;
        }
        let effect = [color[0], color[1], color[2], word(coverage * opacity)];
        let mut result = crate::raster::blend16(original, effect, 1., blend);
        if v["position"] == "inside" {
            result[3] = original[3];
        }
        pixel.copy_from_slice(&result);
    }
}
