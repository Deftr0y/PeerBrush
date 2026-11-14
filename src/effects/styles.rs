//! Native-depth layer-style effects. Styles receive a derived document-coordinate frame.
use serde_json::{json, Value};

pub(crate) const MAX_STROKE_SIZE: f64 = 128.;

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

#[derive(Clone, Copy)]
pub(crate) struct GlobalLight {
    pub angle: u32,
    pub local_angle: f64,
}

impl GlobalLight {
    pub fn from_value(value: &Value) -> Option<Self> {
        let [angle, local_angle] = value.as_array()?.as_slice() else { return None };
        let angle = angle.as_u64().filter(|&angle| angle < 360)? as u32;
        let local_angle = local_angle.as_f64().filter(|angle| (-180. ..=180.).contains(angle))?;
        Some(Self { angle, local_angle })
    }

    pub fn cast_angle(light: f64) -> f64 {
        (light + 360.).rem_euclid(360.) - 180.
    }

    pub fn is_linked(self, settings: &Value) -> bool {
        settings["angle"].as_f64() == Some(Self::cast_angle(f64::from(self.angle)))
    }

    pub(crate) fn for_document(settings: &Value, angle: Option<[u8; 4]>) -> Option<Self> {
        Self::from_value(&settings["global_light"])
            .filter(|light| angle.map(u32::from_be_bytes) == Some(light.angle) && light.is_linked(settings))
    }

    pub(crate) fn set_angle(self, settings: &mut Value, angle: u32) {
        settings["global_light"] = json!([angle, self.local_angle]);
        settings["angle"] = json!(Self::cast_angle(f64::from(angle)));
        if settings["size"] == 0. {
            if angle % 90 == 0 {
                settings.as_object_mut().unwrap().remove("pixel_snap");
            } else {
                settings["pixel_snap"] = json!(true);
            }
        }
    }
}

pub(crate) fn shadow_kind(kind: &str) -> bool {
    matches!(kind, "inner_shadow" | "drop_shadow")
}

pub(crate) fn global_light_request(kind: &str, command: &Value) -> Result<Option<bool>, String> {
    command.get("use_global_light").map(|value| {
        if !shadow_kind(kind) {
            return Err("Global Light links are available for Inner and Drop Shadows".into());
        }
        value.as_bool().ok_or_else(|| "Use Global Light must be true or false".into())
    }).transpose()
}

pub(crate) fn apply_global_light_request(
    settings: &mut Value, resource: &mut Option<[u8; 4]>, request: Option<bool>,
) {
    match request {
        Some(true) if GlobalLight::for_document(settings, *resource).is_none() => {
            let local_angle = GlobalLight::cast_angle(settings["angle"].as_f64().unwrap_or(120.));
            let angle = resource.map_or_else(
                || (local_angle.round() as i32).rem_euclid(360) as u32,
                u32::from_be_bytes,
            );
            GlobalLight { angle, local_angle }.set_angle(settings, angle);
            *resource = Some(angle.to_be_bytes());
        }
        Some(false) => detach_global_light(settings),
        None if !global_light_matches(settings, *resource) => detach_global_light(settings),
        _ => {}
    }
}

pub(crate) fn global_light_matches(settings: &Value, angle: Option<[u8; 4]>) -> bool {
    settings.get("global_light").is_none() || GlobalLight::for_document(settings, angle).is_some()
}

pub(crate) fn detach_global_light(settings: &mut Value) {
    if let Some(object) = settings.as_object_mut() {
        object.remove("global_light");
    }
}

pub(crate) fn localize_shadow_light(settings: &mut Value) {
    if GlobalLight::from_value(&settings["global_light"]).is_some_and(|light| !light.is_linked(settings)) {
        detach_global_light(settings);
    }
}
pub(crate) fn pattern_coverage(kind: &str, value: &Value) -> bool {
    kind == "pattern_overlay" && value.get("use_pattern_alpha").is_some()
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
    validate_with_bitmap(kind, value, |source| {
        crate::effects::patterns::get(source).map(|_| ())
    })
}
fn validate_with_bitmap(
    kind: &str,
    value: &Value,
    mut validate_bitmap: impl FnMut(&str) -> Result<(), String>,
) -> Result<(), String> {
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
            && !(bitmap && kind == "pattern_overlay" && key == "use_pattern_alpha")
            && !(kind == "gradient_overlay"
                && matches!(
                    key.as_str(),
                    "color_stops"
                        | "alpha_stops"
                        | "offset_x"
                        | "offset_y"
                        | "pixel_grid"
                        | "align_layer"
                        | "smoothness"
                        | "use_gradient_alpha"
                ))
            && !(crate::effects::contours::contains(kind) && key == "contour_points")
            && !(matches!(kind, "inner_shadow" | "drop_shadow") && key == "global_light")
        {
            return Err(format!("Unsupported {kind} setting: {key}"));
        }
        match key.as_str() {
            "global_light" => {
                if GlobalLight::from_value(v).is_none() {
                    return Err("Invalid Photoshop global and local shadow angles".into());
                }
            }
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
            "reverse" | "invert" | "down" | "pixel_snap" | "source_knockout" | "pixel_grid"
            | "align_layer" | "use_gradient_alpha" | "use_pattern_alpha" => {
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
                validate_bitmap(v.as_str().ok_or("bitmap_png must be base64 PNG text")?)?;
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
                    "smoothness" => (0., 1.),
                    "size" if kind == "stroke" => (0., MAX_STROKE_SIZE),
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
    if value["align_layer"] == true && value["pixel_grid"] != true {
        return Err("Layer-aligned gradients require pixel-aligned placement".into());
    }
    if value["contour"] == "custom" {
        crate::effects::contours::Curve::decode(&value["contour_points"])?;
    } else if value.get("contour_points").is_some() {
        return Err("Contour points require the Custom contour".into());
    }
    if bitmap {
        validate_bitmap(
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

pub(crate) fn normalized_prepared(
    kind: &str,
    value: &Value,
    bitmap: Option<&crate::effects::patterns::PreparedBitmap>,
) -> Result<Value, String> {
    let active = crate::effects::patterns::active(kind, value);
    if active != bitmap.is_some() {
        return Err("Regional bitmap styles require their exact admitted native payload".into());
    }
    validate_with_bitmap(kind, value, |source| {
        bitmap
            .ok_or("Regional bitmap was not prepared")?
            .matches_source(source)
    })?;
    let mut result = defaults(kind);
    if active {
        for (key, value) in bitmap_defaults().as_object().unwrap() {
            result[key] = value.clone();
        }
    }
    for (key, value) in value.as_object().unwrap() {
        result[key] = value.clone();
    }
    localize_shadow_light(&mut result);
    Ok(result)
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
pub(crate) fn profile(x: f64, key: &str) -> f64 {
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
fn filled<T: Clone>(length: usize, value: T) -> Result<Vec<T>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(length)
        .map_err(|_| "Style buffer allocation failed")?;
    result.resize(length, value);
    Ok(result)
}
fn copied<T: Clone>(source: &[T]) -> Result<Vec<T>, String> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(source.len())
        .map_err(|_| "Style buffer allocation failed")?;
    result.extend_from_slice(source);
    Ok(result)
}
/// Separable box smoothing with transparent samples outside the frame, O(pixels).
pub(super) fn smooth(values: &mut Vec<f64>, w: usize, h: usize, r: usize) -> Result<(), String> {
    if r == 0 || w == 0 || h == 0 {
        return Ok(());
    }
    let mut out = filled(values.len(), 0.)?;
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
    Ok(())
}
fn sample(v: &[f64], w: usize, h: usize, origin: [u32; 2], x: f64, y: f64) -> f64 {
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - f64::from(ix);
    let fy = y - f64::from(iy);
    let get = |x: i32, y: i32| {
        let x = x - origin[0] as i32;
        let y = y - origin[1] as i32;
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
    apply_window(words, width, height, kind, v, Window::full(width, height))
}

/// Global alpha geometry is an explicit reduction of this exact stage's input.
/// An unknown reduction must never be replaced by the current crop's bounds.
#[derive(Clone, Copy, Debug)]
pub(crate) enum AlphaBounds {
    Empty,
    Nonzero([u32; 4]),
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Window {
    pub origin: [u32; 2],
    pub frame: [u32; 2],
    pub alpha_bounds: Option<AlphaBounds>,
}
impl Window {
    pub(crate) fn full(width: u32, height: u32) -> Self {
        Self {
            origin: [0, 0],
            frame: [width, height],
            alpha_bounds: None,
        }
    }
    fn validate(self, width: u32, height: u32) -> Result<(), String> {
        if self.origin[0]
            .checked_add(width)
            .is_none_or(|v| v > self.frame[0])
            || self.origin[1]
                .checked_add(height)
                .is_none_or(|v| v > self.frame[1])
        {
            return Err("Style window is outside its declared full frame".into());
        }
        if let Some(AlphaBounds::Nonzero([left, top, right, bottom])) = self.alpha_bounds {
            if left >= right || top >= bottom || right > self.frame[0] || bottom > self.frame[1] {
                return Err("Style alpha reduction is outside its declared full frame".into());
            }
        }
        Ok(())
    }
}

/// The caller must prepare the complete finite-support input window first.
/// Only output samples beyond the kernel halo are authoritative.
pub(crate) fn apply_window(
    words: &mut [u16],
    width: u32,
    height: u32,
    kind: &str,
    v: &Value,
    window: Window,
) -> Result<(), String> {
    apply_window_field(words, width, height, kind, v, window, None)
}

pub(crate) fn smoothing_radii(kind: &str, settings: &Value) -> Vec<u32> {
    let default_size = match kind {
        "inner_glow" | "outer_glow" | "satin" => 8.,
        "inner_shadow" | "drop_shadow" | "bevel_emboss" => 5.,
        _ => return Vec::new(),
    };
    let size = settings["size"].as_f64().unwrap_or(default_size);
    let mut radii = vec![(size / 3.).ceil() as u32; 3];
    if kind == "bevel_emboss" {
        radii.push(settings["soften"].as_f64().unwrap_or(1.).ceil() as u32);
    }
    radii
}

pub(crate) fn field_padding(kind: &str, settings: &Value) -> i32 {
    match kind {
        "inner_shadow" | "drop_shadow" | "satin" => {
            let default = if kind == "satin" { 12. } else { 8. };
            settings["distance"].as_f64().unwrap_or(default).ceil() as i32 + 1
        }
        // Bilinear sampling reads the neighbouring cell even at an integer
        // probe, so +/-1 gradient probes require two declared edge samples.
        "bevel_emboss" => 2,
        _ => 0,
    }
}

pub(crate) fn apply_window_field(
    words: &mut [u16],
    width: u32,
    height: u32,
    kind: &str,
    v: &Value,
    window: Window,
    prepared_field: Option<&super::field::PreparedField>,
) -> Result<(), String> {
    let resolved = crate::effects::normalized(kind, v)?;
    let bitmap = crate::effects::patterns::from_settings(kind, &resolved)?;
    apply_window_resolved(
        words,
        width,
        height,
        kind,
        &resolved,
        window,
        prepared_field,
        bitmap
            .as_deref()
            .map(|b| b as &dyn crate::effects::patterns::Sampler),
    )
}

pub(crate) fn apply_window_prepared(
    words: &mut [u16],
    width: u32,
    height: u32,
    kind: &str,
    v: &Value,
    window: Window,
    prepared_field: Option<&super::field::PreparedField>,
    bitmap: Option<&crate::effects::patterns::PreparedBitmap>,
) -> Result<(), String> {
    // The regional Plan captured and validated these normalized settings and
    // source-bound owner once. Borrow them; cloning PNG strings or decoding a
    // legacy cache here would create unadmitted per-tile allocations.
    if crate::effects::patterns::active(kind, v) != bitmap.is_some() {
        return Err("Captured regional style is missing its admitted bitmap owner".into());
    }
    apply_window_resolved(
        words,
        width,
        height,
        kind,
        v,
        window,
        prepared_field,
        bitmap.map(|b| b as &dyn crate::effects::patterns::Sampler),
    )
}

fn apply_window_resolved(
    words: &mut [u16],
    width: u32,
    height: u32,
    kind: &str,
    v: &Value,
    window: Window,
    prepared_field: Option<&super::field::PreparedField>,
    bitmap: Option<&dyn crate::effects::patterns::Sampler>,
) -> Result<(), String> {
    let (w, h) = (width as usize, height as usize);
    if w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(words.len()) {
        return Err("Style dimensions do not match native samples".into());
    }
    let fill_coverage = kind == "gradient_overlay" && v["use_gradient_alpha"] == true
        || kind == "pattern_overlay" && v["use_pattern_alpha"] == true;
    if words.is_empty() || (number(v, "opacity") == 0. && !fill_coverage) {
        return Ok(());
    }
    window.validate(width, height)?;
    if prepared_field.is_none()
        && (window.origin != [0, 0] || window.frame != [width, height])
        && smoothing_radii(kind, v).iter().any(|r| *r > 0)
    {
        return Err("Regional spatial styles require an exact full-origin smoothing field".into());
    }
    if let Some(field) = prepared_field {
        let pad = field_padding(kind, v);
        let need = [
            (window.origin[0] as i32 - pad).max(0),
            (window.origin[1] as i32 - pad).max(0),
            (window.origin[0] as i32 + width as i32 + pad).min(window.frame[0] as i32),
            (window.origin[1] as i32 + height as i32 + pad).min(window.frame[1] as i32),
        ];
        let bounds = field.bounds();
        if field.frame() != window.frame
            || bounds[0] > need[0]
            || bounds[1] > need[1]
            || bounds[2] < need[2]
            || bounds[3] < need[3]
        {
            return Err(
                "Prepared style field does not cover its exact global sampling footprint".into(),
            );
        }
    }
    let source = copied(words)?;
    let mut alpha = filled(w * h, 0.)?;
    for (a, p) in alpha.iter_mut().zip(source.chunks_exact(4)) {
        *a = f64::from(p[3]) / 65535.;
    }
    if kind == "stroke" {
        return stroke(words, &source, &alpha, w, h, v, window);
    }
    let mut field = if prepared_field.is_none() {
        copied(&alpha)?
    } else {
        Vec::new()
    };
    let gradient_grid = if kind == "gradient_overlay" && v["pixel_grid"] == true {
        let full = window.origin == [0, 0] && window.frame == [width, height];
        if full && window.alpha_bounds.is_none() {
            Some(crate::effects::gradient::Grid::new(&alpha, w, h, v))
        } else {
            let bounds = if v["align_layer"] == true {
                match window.alpha_bounds {
                    Some(AlphaBounds::Nonzero(b)) => b,
                    Some(AlphaBounds::Empty) => [0, 0, window.frame[0], window.frame[1]],
                    None => return Err(
                        "Regional layer-aligned gradient requires exact global input-alpha bounds"
                            .into(),
                    ),
                }
            } else {
                [0, 0, window.frame[0], window.frame[1]]
            };
            Some(crate::effects::gradient::Grid::from_bounds(
                (
                    bounds[0] as usize,
                    bounds[1] as usize,
                    bounds[2] as usize,
                    bounds[3] as usize,
                ),
                v,
            ))
        }
    } else {
        None
    };
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
    if prepared_field.is_none() {
        for radius in smoothing_radii(kind, v) {
            smooth(&mut field, w, h, radius as usize)?;
        }
    }
    let field_value = |x: usize, y: usize| -> Result<f64, String> {
        prepared_field.map_or_else(
            || Ok(field[y * w + x]),
            |f| {
                f.value(
                    x as i32 + window.origin[0] as i32,
                    y as i32 + window.origin[1] as i32,
                )
            },
        )
    };
    let field_sample = |x: usize, y: usize, dx: f64, dy: f64| -> Result<f64, String> {
        let x = (x + window.origin[0] as usize) as f64 + dx;
        let y = (y + window.origin[1] as usize) as f64 + dy;
        prepared_field.map_or_else(
            || Ok(sample(&field, w, h, window.origin, x, y)),
            |f| f.sample(x, y),
        )
    };
    let color = rgb(v, "color");
    let color2 = rgb(v, "color2");
    let opacity = number(v, "opacity");
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
            let global_x = x + window.origin[0] as usize;
            let global_y = y + window.origin[1] as usize;
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
                    let px = (global_x as f64 + 0.5) / window.frame[0] as f64 - 0.5 - offset.0;
                    let py = (global_y as f64 + 0.5) / window.frame[1] as f64 - 0.5 - offset.1;
                    let position = px * direction.0 - py * direction.1;
                    let style = v["style"].as_str().unwrap_or("linear");
                    let mut t = if let Some(grid) = &gradient_grid {
                        grid.position(global_x, global_y, style)
                    } else {
                        match style {
                            "radial" => px.hypot(py) * 2. / scale,
                            "angle" => {
                                (py.atan2(px) + angle).rem_euclid(std::f64::consts::TAU)
                                    / std::f64::consts::TAU
                            }
                            "reflected" => position.abs() * 2. / scale,
                            "diamond" => (px.abs() + py.abs()) / scale,
                            _ => 0.5 + position / scale,
                        }
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
                        let sample = bitmap.sample(global_x as f64, global_y as f64, v);
                        tint = sample[..3].try_into().unwrap();
                        coverage = f64::from(sample[3]) / 65535.;
                    } else {
                        let t = pattern(global_x as f64, global_y as f64, v);
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
                    let light =
                        (height(global_x as f64 + angle.cos(), global_y as f64 - angle.sin())
                            - height(global_x as f64 - angle.cos(), global_y as f64 + angle.sin()))
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
                    coverage = contour(
                        (field_value(x, y)? / (1. - number(v, "spread")).max(0.01)).min(1.),
                    ) * (1. - a);
                    behind = true;
                    outside = true;
                }
                "inner_glow" => {
                    coverage = if v["source"] == "center" {
                        field_value(x, y)?
                    } else {
                        1. - field_value(x, y)?
                    };
                    coverage = contour((coverage / (1. - number(v, "spread")).max(0.01)).min(1.));
                }
                "drop_shadow" => {
                    coverage = contour(
                        (field_sample(x, y, -dx, -dy)? / (1. - number(v, "spread")).max(0.01))
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
                        ((1. - field_sample(x, y, -dx, -dy)?)
                            / (1. - number(v, "spread")).max(0.01))
                        .min(1.),
                    );
                }
                "satin" => {
                    coverage = contour(
                        (field_sample(x, y, dx, dy)? - field_sample(x, y, -dx, -dy)?).abs(),
                    );
                    if v["invert"] == true {
                        coverage = 1. - coverage;
                    }
                }
                "bevel_emboss" => {
                    let gx = field_sample(x, y, 1., 0.)? - field_sample(x, y, -1., 0.)?;
                    let gy = field_sample(x, y, 0., 1.)? - field_sample(x, y, 0., -1.)?;
                    let mut light = (gx * angle.cos() - gy * angle.sin())
                        * number(v, "depth")
                        * (size + 1.)
                        * number(v, "altitude").to_radians().cos();
                    if v["down"] == true {
                        light = -light;
                    }
                    if v["style"] == "pillow" {
                        light *= 2. * field_value(x, y)? - 1.;
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
                "color_overlay" => {}
                _ => return Err("Unsupported layer-style effect".into()),
            }
            if !outside && original[3] == 0 {
                continue;
            }
            if coverage * opacity == 0. && !fill_coverage {
                continue;
            }
            // Fill coverage changes alpha independently of the sampled RGB.
            // Ordinary overlays retain their historical opacity interpolation.
            let effect = [tint[0], tint[1], tint[2], if fill_coverage { 65535 } else { word(coverage * opacity) }];
            let mut result = if behind {
                crate::raster::blend16(effect, original, 1., "normal")
            } else if !outside {
                let opaque = [original[0], original[1], original[2], 65535];
                crate::raster::blend16(opaque, effect, 1., blend)
            } else {
                crate::raster::blend16(original, effect, 1., blend)
            };
            if !outside {
                result[3] = if fill_coverage { word(a * coverage * opacity) } else { original[3] };
            }
            words[at..at + 4].copy_from_slice(&result);
        }
    }
    Ok(())
}
// Exact squared Euclidean distance transform, linear work per row/column.
fn distances(
    alpha: &[f64],
    w: usize,
    h: usize,
    inside: bool,
    window: Window,
) -> Result<Vec<f64>, String> {
    let mut values = filled(alpha.len(), 0.)?;
    for (value, a) in values.iter_mut().zip(alpha) {
        *value = if (*a > 0.5) == inside { 1e12 } else { 0. };
    }
    let transform = |input: &[f64]| -> Result<Vec<f64>, String> {
        let n = input.len();
        let mut sites = filled(n, 0usize)?;
        let mut bounds = filled(n + 1, 0.)?;
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
        let mut result = filled(n, 0.)?;
        for (q, out) in result.iter_mut().enumerate() {
            while bounds[k + 1] < (q as f64) {
                k += 1;
            }
            let p = sites[k];
            *out = ((q as f64 - p as f64).powi(2) + input[p]).max(0.);
        }
        Ok(result)
    };
    for row in values.chunks_exact_mut(w) {
        row.copy_from_slice(&transform(row)?);
    }
    let mut column = filled(h, 0.)?;
    for x in 0..w {
        for y in 0..h {
            column[y] = values[y * w + x];
        }
        let result = transform(&column)?;
        for y in 0..h {
            let d = result[y].sqrt();
            values[y * w + x] = if inside {
                let gx = x + window.origin[0] as usize;
                let gy = y + window.origin[1] as usize;
                d.min(
                    (gx + 1)
                        .min(window.frame[0] as usize - gx)
                        .min(gy + 1)
                        .min(window.frame[1] as usize - gy) as f64,
                )
            } else {
                d
            };
        }
    }
    Ok(values)
}
fn stroke(
    words: &mut [u16],
    source: &[u16],
    alpha: &[f64],
    w: usize,
    h: usize,
    v: &Value,
    window: Window,
) -> Result<(), String> {
    let size = number(v, "size");
    if size == 0. {
        return Ok(());
    }
    let position = v["position"].as_str().unwrap_or("outside");
    let radius = if position == "center" {
        size * 0.5
    } else {
        size
    };
    let outer = if position != "inside" {
        distances(alpha, w, h, false, window)?
    } else {
        Vec::new()
    };
    let inner = if matches!(position, "inside" | "center") {
        distances(alpha, w, h, true, window)?
    } else {
        Vec::new()
    };
    let color = rgb(v, "color");
    let opacity = number(v, "opacity");
    let blend = v["blend"].as_str().unwrap_or("normal");
    for (index, pixel) in words.chunks_exact_mut(4).enumerate() {
        let original: [u16; 4] = source[index * 4..index * 4 + 4].try_into().unwrap();
        let a = alpha[index];
        let coverage = match position {
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
        if position == "inside" {
            result[3] = original[3];
        }
        pixel.copy_from_slice(&result);
    }
    Ok(())
}

#[cfg(test)]
mod window_tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};

    #[test]
    fn stroke_positions_keep_their_native_outline_coverage() {
        for (position, center_red, next_alpha, fringe_alpha) in [
            ("inside", 65535, 0, 0),
            ("outside", 0, 65535, 32768),
            ("center", 32768, 32768, 0),
        ] {
            let mut words = vec![0; 5 * 5 * 4];
            let center = (2 * 5 + 2) * 4;
            words[center..center + 4].copy_from_slice(&[0, 0, 65535, 65535]);
            let settings = json!({
                "size": 2., "position": position, "color": [1., 0., 0.], "opacity": 1.,
            });
            apply(&mut words, 5, 5, "stroke", &settings).unwrap();
            assert_eq!(words[center], center_red, "{position} color");
            assert_eq!(words[center + 3], 65535, "{position} source alpha");
            assert_eq!(words[center + 7], next_alpha, "{position} adjacent alpha");
            assert_eq!(words[center + 11], fringe_alpha, "{position} fringe alpha");
        }
    }

    #[test]
    fn bitmap_fill_coverage_keeps_native_colors_and_multiplies_alpha() {
        let tile = crate::raster::png16(2, 1, &[0, 0, 0, 0, 12347, 23459, 34571, 29123]).unwrap();
        let mut settings = json!({"pattern":"bitmap", "bitmap_png":STANDARD.encode(tile),
            "sampling":"nearest", "use_pattern_alpha":true, "opacity":0.5});
        let original = [65535, 65535, 65535, 65535, 65535, 65535, 65535, 40001];
        let mut fill = original;
        apply(&mut fill, 2, 1, "pattern_overlay", &settings).unwrap();
        assert_eq!(fill, [0, 0, 0, 0, 12347, 23459, 34571, 8888]);
        settings["use_pattern_alpha"] = json!(false);
        let mut overlay = original;
        apply(&mut overlay, 2, 1, "pattern_overlay", &settings).unwrap();
        assert_eq!(&overlay[..4], &original[..4]);
        assert_eq!(overlay[7], 40001);
        settings["use_pattern_alpha"] = json!(1);
        assert!(validate("pattern_overlay", &settings).is_err());
    }

    #[test]
    fn gradient_coverage_retains_color_and_multiplies_native_alpha_including_zero() {
        let mut settings = defaults("gradient_overlay");
        settings["opacity"] = json!(0.5);
        settings["use_gradient_alpha"] = json!(true);
        settings["pixel_grid"] = json!(true);
        settings["angle"] = json!(0.);
        settings["color_stops"] = json!([
            {"position":0.,"color":[1.,0.,0.]},
            {"position":1.,"color":[1.,0.,0.]}
        ]);
        settings["alpha_stops"] = json!([
            {"position":0.,"opacity":0.}, {"position":1.,"opacity":1.}
        ]);
        validate("gradient_overlay", &settings).unwrap();
        let source = vec![12347, 23459, 34571, 65535, 23459, 34571, 45683, 40001];
        let mut fill = source.clone();
        apply(&mut fill, 2, 1, "gradient_overlay", &settings).unwrap();
        assert_eq!(fill, [65535, 0, 0, 0, 65535, 0, 0, 10000]);
        settings["opacity"] = json!(0.);
        let mut hidden = source.clone();
        apply(&mut hidden, 2, 1, "gradient_overlay", &settings).unwrap();
        assert_eq!(hidden, [65535, 0, 0, 0, 65535, 0, 0, 0]);
        settings["opacity"] = json!(0.5);
        settings["use_gradient_alpha"] = json!(false);
        let mut overlay = source.clone();
        apply(&mut overlay, 2, 1, "gradient_overlay", &settings).unwrap();
        assert_eq!(&overlay[..4], &source[..4]);
        assert_eq!(overlay[7], 40001);
        settings["use_gradient_alpha"] = json!(1);
        assert!(validate("gradient_overlay", &settings).is_err());
    }

    fn source(w: u32, h: u32, depth: u16) -> Vec<u16> {
        (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let inside = x >= 11 && x < w - 12 && y >= 9 && y < h - 9;
                    let a = if !inside || (x * 3 + y * 5) % 17 == 0 {
                        0
                    } else {
                        27001 + (x * 317 + y * 173) % 38535
                    };
                    let pixel = [
                        10001 + (x * 739 + y * 127) % 54001,
                        21113 + (x * 137) % 40003,
                        32213 + (y * 211) % 30011,
                        a,
                    ];
                    pixel.map(|v| {
                        if depth == 8 {
                            (v / 257 * 257) as u16
                        } else {
                            v as u16
                        }
                    })
                })
            })
            .collect()
    }
    fn crop(source: &[u16], width: u32, rect: [u32; 4]) -> Vec<u16> {
        let mut result = Vec::new();
        for y in rect[1]..rect[3] {
            let begin = (y as usize * width as usize + rect[0] as usize) * 4;
            result.extend_from_slice(&source[begin..begin + (rect[2] - rect[0]) as usize * 4]);
        }
        result
    }
    fn prepare_field(
        source: &[u16],
        w: u32,
        h: u32,
        depth: u16,
        work: [u32; 4],
        kind: &str,
        settings: &Value,
        budget: &crate::large_document::ResidentBudget,
    ) -> Option<super::super::field::PreparedField> {
        let radii = smoothing_radii(kind, settings);
        if !radii.iter().any(|r| *r > 0) {
            return None;
        }
        let pad = field_padding(kind, settings);
        let bounds = [
            (work[0] as i32 - pad).max(0),
            (work[1] as i32 - pad).max(0),
            (work[2] as i32 + pad).min(w as i32),
            (work[3] as i32 + pad).min(h as i32),
        ];
        Some(
            super::super::field::prepare(
                [w, h],
                bounds,
                &radii,
                depth,
                budget,
                &mut || Ok(()),
                |region| {
                    let length = region.pixels() * if depth == 16 { 8 } else { 4 };
                    let mut bytes =
                        crate::large_document::ScratchBytes::new(budget, length, length)
                            .map_err(|e| e.to_string())?;
                    let b = region.bounds();
                    let mut at = 0;
                    for y in b[1]..b[3] {
                        for x in b[0]..b[2] {
                            let start = (y as usize * w as usize + x as usize) * 4;
                            let p = &source[start..start + 4];
                            if depth == 16 {
                                for v in p {
                                    bytes[at..at + 2].copy_from_slice(&v.to_be_bytes());
                                    at += 2;
                                }
                            } else {
                                for v in p {
                                    bytes[at] = crate::raster::project16(*v);
                                    at += 1;
                                }
                            }
                        }
                    }
                    super::super::regional::RegionalImage::from_native(
                        region,
                        depth,
                        bytes.freeze(),
                    )
                },
            )
            .unwrap(),
        )
    }

    #[test]
    fn regional_styles_keep_full_frame_phase_alpha_and_native_samples() {
        let (w, h) = (129, 123);
        let output = [49, 43, 80, 76];
        for depth in [8, 16] {
            let source = source(w, h, depth);
            for kind in KINDS {
                let mut settings = defaults(kind);
                if *kind == "gradient_overlay" {
                    settings["pixel_grid"] = json!(true);
                    settings["align_layer"] = json!(true);
                    settings["angle"] = json!(37.);
                    settings["offset_x"] = json!(0.13);
                    settings["use_gradient_alpha"] = json!(true);
                    settings["alpha_stops"] = json!([
                        {"position":0.,"opacity":0.}, {"position":1.,"opacity":0.73}
                    ]);
                }
                let mut full = source.clone();
                apply(&mut full, w, h, kind, &settings).unwrap();
                let work = [17, 11, 112, 108];
                let mut local = crop(&source, w, work);
                let budget = crate::large_document::ResidentBudget::new(32 * 1024 * 1024).unwrap();
                let field = prepare_field(&source, w, h, depth, work, kind, &settings, &budget);
                apply_window_field(
                    &mut local,
                    work[2] - work[0],
                    work[3] - work[1],
                    kind,
                    &settings,
                    Window {
                        origin: [work[0], work[1]],
                        frame: [w, h],
                        alpha_bounds: Some(AlphaBounds::Nonzero([11, 9, w - 12, h - 9])),
                    },
                    field.as_ref(),
                )
                .unwrap();
                assert_eq!(
                    crop(
                        &local,
                        work[2] - work[0],
                        [
                            output[0] - work[0],
                            output[1] - work[1],
                            output[2] - work[0],
                            output[3] - work[1]
                        ]
                    ),
                    crop(&full, w, output),
                    "depth{depth} {kind}"
                );
            }
        }
    }

    #[test]
    fn legacy_gradient_geometry_and_true_canvas_edges_match_full_frame() {
        let (w, h) = (81, 79);
        let source = source(w, h, 16);
        for kind in ["gradient_overlay", "pattern_overlay", "texture", "stroke"] {
            let settings = defaults(kind);
            let mut full = source.clone();
            apply(&mut full, w, h, kind, &settings).unwrap();
            for work in [[0, 0, 50, 52], [29, 27, w, h]] {
                let output = if work[0] == 0 {
                    [0, 0, 18, 18]
                } else {
                    [61, 59, w, h]
                };
                let mut local = crop(&source, w, work);
                apply_window(
                    &mut local,
                    work[2] - work[0],
                    work[3] - work[1],
                    kind,
                    &settings,
                    Window {
                        origin: [work[0], work[1]],
                        frame: [w, h],
                        alpha_bounds: None,
                    },
                )
                .unwrap();
                assert_eq!(
                    crop(
                        &local,
                        work[2] - work[0],
                        [
                            output[0] - work[0],
                            output[1] - work[1],
                            output[2] - work[0],
                            output[3] - work[1]
                        ]
                    ),
                    crop(&full, w, output),
                    "{kind} {work:?}"
                );
            }
        }
    }

    #[test]
    fn missing_or_invalid_global_geometry_refuses_without_changing_pixels() {
        let mut settings = defaults("gradient_overlay");
        settings["pixel_grid"] = json!(true);
        settings["align_layer"] = json!(true);
        let original = source(23, 29, 16);
        for window in [
            Window {
                origin: [10, 11],
                frame: [100, 100],
                alpha_bounds: None,
            },
            Window {
                origin: [90, 11],
                frame: [100, 100],
                alpha_bounds: Some(AlphaBounds::Empty),
            },
            Window {
                origin: [10, 11],
                frame: [100, 100],
                alpha_bounds: Some(AlphaBounds::Nonzero([0, 0, 101, 100])),
            },
        ] {
            let mut pixels = original.clone();
            assert!(
                apply_window(&mut pixels, 23, 29, "gradient_overlay", &settings, window).is_err()
            );
            assert_eq!(pixels, original);
        }
    }

    #[test]
    fn bitmap_patterns_preserve_global_phase_and_native_low_bits() {
        let words = [
            12347, 33559, 51237, 65535, 54321, 12345, 45679, 27139, 19001, 27003, 37007, 49151,
            61001, 51003, 41009, 65535,
        ];
        let png = crate::raster::png16(2, 2, &words).unwrap();
        let (w, h) = (73, 69);
        let original = source(w, h, 16);
        let area = [19, 17, 57, 53];
        for kind in ["pattern_overlay", "texture"] {
            let mut settings = defaults(kind);
            settings["pattern"] = json!("bitmap");
            settings["bitmap_png"] = json!(STANDARD.encode(&png));
            settings["bitmap_scale"] = json!(1.7);
            settings["bitmap_angle"] = json!(23.);
            settings["phase_x"] = json!(-1.3);
            settings["phase_y"] = json!(2.7);
            settings["sampling"] = json!("bilinear");
            let mut full = original.clone();
            apply(&mut full, w, h, kind, &settings).unwrap();
            let mut local = crop(&original, w, area);
            apply_window(
                &mut local,
                area[2] - area[0],
                area[3] - area[1],
                kind,
                &settings,
                Window {
                    origin: [area[0], area[1]],
                    frame: [w, h],
                    alpha_bounds: None,
                },
            )
            .unwrap();
            assert_eq!(local, crop(&full, w, area), "{kind}");
        }
    }

    #[test]
    fn impossible_style_buffer_request_returns_an_allocation_error() {
        assert!(filled::<u64>(usize::MAX, 0).is_err());
    }
    #[test]
    fn cropped_float_smoothing_without_a_global_field_refuses_unchanged_pixels() {
        let original = source(23, 29, 16);
        let mut pixels = original.clone();
        assert!(apply_window(
            &mut pixels,
            23,
            29,
            "outer_glow",
            &defaults("outer_glow"),
            Window {
                origin: [10, 11],
                frame: [100, 100],
                alpha_bounds: None
            }
        )
        .is_err());
        assert_eq!(pixels, original);
    }
}
