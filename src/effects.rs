//! Editable effect sources and bounded derived color images; neither cache nor baked pixels enter history.
use crate::engine::{id, Document, Layer};
use crate::raster::Pixel;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};
pub const BUDGET: usize = 256 * 1024 * 1024;
// Caches and transient effect buffers have separate, bounded budgets.
pub const WORKING_BUDGET: usize = 256 * 1024 * 1024;
pub mod catalog;
pub const KINDS: &[&str] = &[
    "levels",
    "curves",
    "blur",
    "adjust",
    "color_balance",
    "hsl",
    "bloom",
    "liquify",
    "invert",
    "grayscale",
];
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    #[serde(default = "full_weight")]
    pub weight: f32,
    pub settings: Value,
}
pub fn full_weight() -> f32 {
    1.0
}
pub fn validate_weight(weight: f32) -> Result<(), String> {
    if !weight.is_finite() || !(0.0..=1.0).contains(&weight) {
        return Err("Effect weight must be between 0 and 1".into());
    }
    Ok(())
}
pub fn command_weight(command: &Value) -> Result<f32, String> {
    let weight = match command.get("weight") {
        None => 1.0,
        Some(value) => value.as_f64().ok_or("Effect weight must be a number")?,
    };
    if !weight.is_finite() || !(0.0..=1.0).contains(&weight) {
        return Err("Effect weight must be between 0 and 1".into());
    }
    Ok(weight as f32)
}
/// Interpolate straight samples in premultiplied space. Alpha-changing spatial
/// effects fade continuously without halos; hidden RGB remains native too.
pub fn weighted_pixel(before: [u16; 4], after: [u16; 4], weight: f32) -> [u16; 4] {
    if weight <= 0.0 {
        return before;
    }
    if weight >= 1.0 {
        return after;
    }
    let t = f64::from(weight);
    let a = f64::from(before[3]) * (1.0 - t);
    let b = f64::from(after[3]) * t;
    let alpha = a + b;
    let mut result = [0; 4];
    for c in 0..3 {
        result[c] = (if alpha == 0.0 {
            f64::from(before[c]) * (1.0 - t) + f64::from(after[c]) * t
        } else {
            (f64::from(before[c]) * a + f64::from(after[c]) * b) / alpha
        })
        .round()
        .clamp(0.0, 65535.0) as u16;
    }
    result[3] = alpha.round().clamp(0.0, 65535.0) as u16;
    result
}
pub fn weighted_working_bytes(effect: &Effect, w: u32, h: u32) -> Result<u64, String> {
    Ok(working_bytes(&effect.kind, w, h, &effect.settings)?
        + if effect.weight > 0.0 && effect.weight < 1.0 {
            u64::from(w) * u64::from(h) * 4
        } else {
            0
        })
}
pub(crate) fn apply_effect(
    image: &mut Image,
    effect: &Effect,
    region_source: Option<u64>,
) -> Result<(), String> {
    validate_weight(effect.weight)?;
    if !effect.enabled || effect.weight == 0.0 {
        return Ok(());
    }
    if weighted_working_bytes(effect, image.width, image.height)? > WORKING_BUDGET as u64 {
        return Err("Weighted effect working buffers exceed the 256 MiB budget".into());
    }
    let before = (effect.weight < 1.0).then(|| image.bytes.clone());
    if let Some(source_pixels) = region_source {
        apply_region(image, &effect.kind, &effect.settings, source_pixels)?;
    } else {
        apply(image, &effect.kind, &effect.settings)?;
    }
    if let Some(before) = before {
        for (source, target) in before.chunks_exact(4).zip(image.bytes.chunks_exact_mut(4)) {
            let result = weighted_pixel(
                source
                    .try_into()
                    .map(|p: [u8; 4]| p.map(u16::from))
                    .unwrap(),
                (&*target)
                    .try_into()
                    .map(|p: [u8; 4]| p.map(u16::from))
                    .unwrap(),
                effect.weight,
            );
            target.copy_from_slice(&result.map(|v| v as u8));
        }
    }
    Ok(())
}
pub fn defaults(kind: &str) -> Value {
    match kind {
        "levels" => json!({"black":0.0,"white":1.0,"gamma":1.0}),
        "curves" => json!({"points":[[0.0,0.0],[0.5,0.5],[1.0,1.0]],"interpolation":"smooth"}),
        "blur" => json!({"radius":8.0}),
        "adjust" => json!({"brightness":0.0,"contrast":1.0,"saturation":1.0}),
        "color_balance" => {
            json!({"shadows":[0.0,0.0,0.0],"midtones":[0.0,0.0,0.0],"highlights":[0.0,0.0,0.0],"preserve_luminosity":true})
        }
        "hsl" => json!({"hue":0.0,"saturation":0.0,"lightness":0.0}),
        "bloom" => json!({"threshold":0.75,"spread":12.0,"strength":0.5}),
        "liquify" => crate::liquify::defaults(),
        _ => json!({}),
    }
}
pub fn number(v: &Value, key: &str, default: f32) -> f32 {
    v[key]
        .as_f64()
        .filter(|n| n.is_finite() && n.abs() <= f32::MAX as f64)
        .map(|n| n as f32)
        .unwrap_or(default)
}
pub fn validate(kind: &str, v: &Value) -> Result<(), String> {
    if !KINDS.contains(&kind) {
        return Err("Unsupported effect".into());
    }
    if !v.is_object() {
        return Err("Effect settings must be an object".into());
    }
    let check = |key: &str, min: f32, max: f32| {
        if let Some(value) = v.get(key) {
            let n = value.as_f64().ok_or("Invalid effect setting")?;
            if !n.is_finite() || n < min as f64 || n > max as f64 {
                return Err(format!("{key} must be {min}–{max}"));
            }
        }
        Ok(())
    };
    match kind {
        "levels" => {
            check("black", 0.0, 0.99)?;
            check("white", 0.01, 1.0)?;
            check("gamma", 0.1, 5.0)?;
            if number(v, "black", 0.0) >= number(v, "white", 1.0) {
                return Err("White point must be greater than black point".into());
            }
        }
        "blur" => check("radius", 0.0, 64.0)?,
        "adjust" => {
            check("brightness", -1.0, 1.0)?;
            check("contrast", 0.0, 4.0)?;
            check("saturation", 0.0, 3.0)?;
        }
        "color_balance" => {
            for band in ["shadows", "midtones", "highlights"] {
                if let Some(values) = v.get(band) {
                    let values = values
                        .as_array()
                        .ok_or("Color balance tones must contain three values")?;
                    if values.len() != 3 {
                        return Err("Color balance tones must contain three values".into());
                    }
                    for value in values {
                        let n = value.as_f64().ok_or("Invalid color balance value")?;
                        if !n.is_finite() || !(-1.0..=1.0).contains(&n) {
                            return Err("Color balance values must be between -1 and 1".into());
                        }
                    }
                }
            }
            if v.get("preserve_luminosity")
                .is_some_and(|x| !x.is_boolean())
            {
                return Err("Preserve luminosity must be true or false".into());
            }
        }
        "hsl" => {
            check("hue", -180.0, 180.0)?;
            check("saturation", -1.0, 1.0)?;
            check("lightness", -1.0, 1.0)?;
        }
        "bloom" => {
            check("threshold", 0.0, 1.0)?;
            check("spread", 0.0, 64.0)?;
            check("strength", 0.0, 3.0)?;
        }
        "liquify" => crate::liquify::validate(v)?,
        "curves" => {
            if let Some(interpolation) = v.get("interpolation") {
                if !matches!(interpolation.as_str(), Some("linear" | "smooth")) {
                    return Err("Curve interpolation must be linear or smooth".into());
                }
            }
            let points = v["points"].as_array().ok_or("Missing curve points")?;
            if points.len() < 2 || points.len() > 16 {
                return Err("Use 2–16 curve points".into());
            }
            let mut previous = -1.0;
            for p in points {
                let p = p.as_array().ok_or("Invalid curve point")?;
                if p.len() != 2 {
                    return Err("Curve points must be [x,y]".into());
                }
                let x = p[0].as_f64().ok_or("Invalid curve x")?;
                let y = p[1].as_f64().ok_or("Invalid curve y")?;
                if !x.is_finite()
                    || !y.is_finite()
                    || !(0.0..=1.0).contains(&x)
                    || !(0.0..=1.0).contains(&y)
                    || x <= previous
                {
                    return Err("Curve x positions must increase within 0–1".into());
                }
                previous = x;
            }
        }
        _ => {}
    }
    Ok(())
}
/// Resolve omitted parameters once; UI, saved sources, and agents use the same defaults.
pub fn normalized(kind: &str, settings: &Value) -> Result<Value, String> {
    validate(kind, settings)?;
    let mut result = defaults(kind);
    if kind == "curves" && settings.get("interpolation").is_none() {
        result["interpolation"] = json!("linear");
    }
    let target = result.as_object_mut().ok_or("Invalid effect defaults")?;
    for (key, value) in settings.as_object().unwrap() {
        target.insert(key.clone(), value.clone());
    }
    Ok(result)
}
pub fn color_only(kind: &str) -> bool {
    ["color_balance", "hsl", "bloom"].contains(&kind)
}
/// Largest scratch buffer used by a single effect, excluding the cached output.
pub fn working_bytes(kind: &str, width: u32, height: u32, settings: &Value) -> Result<u64, String> {
    let pixels = width as u64 * height as u64;
    match kind {
        "blur" | "bloom" => Ok(pixels * 8 + width.max(height) as u64 * 8),
        // Liquify enforces its own field/stroke work limits as well.
        "liquify" => crate::liquify::working_bytes(width, height, settings).map(|n| n as u64),
        _ => Ok(0),
    }
}
pub fn map_value(kind: &str, settings: &Value, v: f32) -> f32 {
    match kind {
        "levels" => {
            let b = number(settings, "black", 0.0);
            let w = number(settings, "white", 1.0);
            ((v - b) / (w - b).max(0.01))
                .clamp(0.0, 1.0)
                .powf(1.0 / number(settings, "gamma", 1.0))
        }
        "curves" => curve_value(settings, v),
        "invert" => 1.0 - v,
        "adjust" => ((v - 0.5) * number(settings, "contrast", 1.0)
            + 0.5
            + number(settings, "brightness", 0.0))
        .clamp(0.0, 1.0),
        _ => v,
    }
}
/// Shape-preserving cubic Hermite interpolation shared by the graph and both pixel depths.
pub fn curve_value(settings: &Value, v: f32) -> f32 {
    curve_value64(settings, v as f64) as f32
}
pub fn curve_value64(settings: &Value, v: f64) -> f64 {
    let points = settings["points"].as_array().unwrap();
    if settings["interpolation"] == "linear" {
        for pair in points.windows(2) {
            let x = pair[0][0].as_f64().unwrap();
            let nx = pair[1][0].as_f64().unwrap();
            if v <= nx {
                let y = pair[0][1].as_f64().unwrap();
                let ny = pair[1][1].as_f64().unwrap();
                return y + (ny - y) * ((v - x) / (nx - x)).clamp(0., 1.);
            }
        }
        return points.last().unwrap()[1].as_f64().unwrap();
    }
    let point = |i: usize| {
        (
            points[i][0].as_f64().unwrap(),
            points[i][1].as_f64().unwrap(),
        )
    };
    let secant = |i: usize| {
        let (x, y) = point(i);
        let (nx, ny) = point(i + 1);
        (ny - y) / (nx - x)
    };
    let tangent = |i: usize| {
        if i == 0 {
            return secant(0);
        }
        if i + 1 == points.len() {
            return secant(i - 1);
        }
        let a = secant(i - 1);
        let b = secant(i);
        if a * b <= 0.0 {
            return 0.0;
        }
        let h0 = point(i).0 - point(i - 1).0;
        let h1 = point(i + 1).0 - point(i).0;
        let w0 = 2.0 * h1 + h0;
        let w1 = h1 + 2.0 * h0;
        (w0 + w1) / (w0 / a + w1 / b)
    };
    if v <= point(0).0 {
        return point(0).1;
    }
    for i in 0..points.len() - 1 {
        let (x, y) = point(i);
        let (nx, ny) = point(i + 1);
        if v <= nx {
            let h = nx - x;
            let t = ((v - x) / h).clamp(0.0, 1.0);
            let t2 = t * t;
            let t3 = t2 * t;
            return ((2.0 * t3 - 3.0 * t2 + 1.0) * y
                + (t3 - 2.0 * t2 + t) * h * tangent(i)
                + (-2.0 * t3 + 3.0 * t2) * ny
                + (t3 - t2) * h * tangent(i + 1))
            .clamp(0.0, 1.0);
        }
    }
    point(points.len() - 1).1
}
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub bytes: Vec<u8>,
}
impl Image {
    pub fn get(&self, x: i32, y: i32) -> Pixel {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0; 4];
        }
        let i = ((y as u32 * self.width + x as u32) * 4) as usize;
        self.bytes[i..i + 4].try_into().unwrap()
    }
}
#[derive(Default)]
struct Cache {
    items: HashMap<String, Arc<Image>>,
    order: VecDeque<String>,
    bytes: usize,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
impl Cache {
    fn get(&mut self, key: &str) -> Option<Arc<Image>> {
        let out = self.items.get(key)?.clone();
        self.order.retain(|k| k != key);
        self.order.push_back(key.into());
        Some(out)
    }
    fn insert(&mut self, key: String, image: Arc<Image>) {
        if image.bytes.len() > BUDGET {
            return;
        }
        if let Some(previous) = self.items.remove(&key) {
            self.bytes -= previous.bytes.len();
        }
        self.order.retain(|k| k != &key);
        while self.bytes + image.bytes.len() > BUDGET {
            let Some(k) = self.order.pop_front() else {
                break;
            };
            if let Some(v) = self.items.remove(&k) {
                self.bytes -= v.bytes.len();
                crate::disk_cache::spill(&k, v.width, v.height, 8, &v.bytes);
            }
        }
        self.bytes += image.bytes.len();
        self.order.push_back(key.clone());
        self.items.insert(key, image);
    }
}
pub fn active(l: &Layer) -> bool {
    l.kind == "adjustment" || l.effects.iter().any(|e| e.enabled && e.weight > 0.0)
}
pub fn validate_budget(doc: &Document) -> Result<(), String> {
    let bytes: u64 = doc
        .layers
        .iter()
        .filter(|l| active(l))
        .map(|l| {
            if ["group", "adjustment"].contains(&l.kind.as_str()) {
                doc.width as u64 * doc.height as u64 * 4
            } else {
                l.pixels.width as u64 * l.pixels.height as u64 * 4
            }
        })
        .sum();
    for layer in doc.layers.iter().filter(|l| active(l)) {
        let (w, h) = if ["group", "adjustment"].contains(&layer.kind.as_str()) {
            (doc.width, doc.height)
        } else {
            (layer.pixels.width, layer.pixels.height)
        };
        for effect in layer.effects.iter().filter(|e| e.enabled && e.weight > 0.0) {
            if weighted_working_bytes(effect, w, h)? > WORKING_BUDGET as u64 {
                return Err("Effect working buffers exceed the 256 MiB budget; reduce the affected layer dimensions".into());
            }
        }
    }
    if bytes > BUDGET as u64 {
        Err("Color effects exceed the 256 MiB derived-image budget".into())
    } else {
        Ok(())
    }
}
pub fn invalidate(doc: &mut Document, commands: &[Value]) {
    let mut changed = HashSet::new();
    for c in commands {
        if c["op"] == "selection" {
            continue;
        }
        // Adjustment inputs depend on preceding composited siblings, including clipped units.
        changed.extend(
            doc.layers
                .iter()
                .filter(|l| l.kind == "adjustment")
                .map(|l| l.id.clone()),
        );
        if let Some(target) = c["layer"].as_str() {
            changed.insert(target.to_owned());
            let mut parent = doc
                .layers
                .iter()
                .find(|l| l.id == target)
                .and_then(|l| l.parent.clone());
            while let Some(p) = parent {
                changed.insert(p.clone());
                parent = doc
                    .layers
                    .iter()
                    .find(|l| l.id == p)
                    .and_then(|l| l.parent.clone());
            }
        } else {
            changed.extend(doc.layers.iter().map(|l| l.id.clone()));
        }
        if [
            "layer.reorder",
            "layer.parent",
            "layer.delete",
            "layer.merge",
            "layer.duplicate",
            "layer.duplicate_selection",
            "layer.paste",
            "group.create_selected",
            "image.place",
        ]
        .contains(&c["op"].as_str().unwrap_or(""))
        {
            changed.extend(
                doc.layers
                    .iter()
                    .filter(|l| l.kind == "group")
                    .map(|l| l.id.clone()),
            );
        }
    }
    for l in &mut doc.layers {
        if changed.contains(&l.id) {
            l.effect_key = id();
        }
    }
}
pub fn prepare(
    doc: &Document,
    masks: &[Option<Arc<crate::mask::GrayMask>>],
) -> Result<Vec<Option<Arc<Image>>>, String> {
    validate_budget(doc)?;
    let mut out = vec![None; doc.layers.len()];
    for i in crate::compositor::effect_order(doc) {
        let l = &doc.layers[i];
        if !active(l) {
            continue;
        }
        let (w, h) = if ["group", "adjustment"].contains(&l.kind.as_str()) {
            (doc.width, doc.height)
        } else {
            (l.pixels.width, l.pixels.height)
        };
        let key = format!("{}:{w}:{h}", l.effect_key);
        let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
        if let Some(image) = cache.lock().unwrap().get(&key) {
            out[i] = Some(image);
            continue;
        }
        if let Some(bytes) = crate::disk_cache::read(&key, w, h, 8) {
            let image = Arc::new(Image {
                width: w,
                height: h,
                bytes,
            });
            cache.lock().unwrap().insert(key, image.clone());
            out[i] = Some(image);
            continue;
        }
        let bytes = match l.kind.as_str() {
            "fill" => {
                let mut bytes = vec![0; w as usize * h as usize * 4];
                for pixel in bytes.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&l.color);
                }
                bytes
            }
            "group" => {
                let plan = crate::compositor::Plan::new(doc, masks, &out);
                let group = plan.group(Some(&l.id));
                let mut bytes = vec![0; w as usize * h as usize * 4];
                for (index, pixel) in bytes.chunks_exact_mut(4).enumerate() {
                    let x = (index % w as usize) as i32;
                    let y = (index / w as usize) as i32;
                    pixel.copy_from_slice(&plan.sample(group, x, y));
                }
                bytes
            }
            "adjustment" => {
                let mut bytes = vec![0; w as usize * h as usize * 4];
                for (index, pixel) in bytes.chunks_exact_mut(4).enumerate() {
                    let x = (index % w as usize) as i32;
                    let y = (index / w as usize) as i32;
                    pixel.copy_from_slice(&crate::compositor::adjustment_input(
                        doc, i, x, y, masks, &out,
                    ));
                }
                bytes
            }
            _ => l.pixels.rgba(),
        };
        let mut image = Image {
            width: w,
            height: h,
            bytes,
        };
        for effect in l.effects.iter().filter(|e| e.enabled && e.weight > 0.0) {
            apply_effect(&mut image, effect, None)?;
        }
        let image = Arc::new(image);
        let mut cache = cache.lock().unwrap();
        if let Some(existing) = cache.get(&key) {
            out[i] = Some(existing);
        } else {
            cache.insert(key, image.clone());
            out[i] = Some(image);
        }
    }
    Ok(out)
}
fn validated_settings(image: &Image, kind: &str, settings: &Value) -> Result<Value, String> {
    let settings = normalized(kind, settings)?;
    let (w, h) = (image.width as usize, image.height as usize);
    if w.checked_mul(h).and_then(|n| n.checked_mul(4)) != Some(image.bytes.len()) {
        return Err("Effect image dimensions do not match its pixels".into());
    }
    if working_bytes(kind, image.width, image.height, &settings)? > WORKING_BUDGET as u64 {
        return Err("Effect working buffers exceed the 256 MiB budget".into());
    }
    Ok(settings)
}
/// Apply the same editable source used by canvas previews, export, and adjustment layers.
pub fn apply(image: &mut Image, kind: &str, settings: &Value) -> Result<(), String> {
    let settings = validated_settings(image, kind, settings)?;
    if crate::gpu::try_apply(image, kind, &settings) {
        return Ok(());
    }
    apply_cpu_prepared(image, kind, settings)
}
pub fn apply_cpu(image: &mut Image, kind: &str, settings: &Value) -> Result<(), String> {
    let settings = validated_settings(image, kind, settings)?;
    apply_cpu_prepared(image, kind, settings)
}
pub(crate) fn apply_region(
    image: &mut Image,
    kind: &str,
    settings: &Value,
    source_pixels: u64,
) -> Result<(), String> {
    let settings = validated_settings(image, kind, settings)?;
    if crate::gpu::try_apply_region(image, kind, &settings, source_pixels) {
        return Ok(());
    }
    apply_cpu_prepared(image, kind, settings)
}
fn apply_cpu_prepared(image: &mut Image, kind: &str, settings: Value) -> Result<(), String> {
    let (w, h) = (image.width as usize, image.height as usize);
    match kind {
        "blur" => {
            blur_rgba(&mut image.bytes, w, h, number(&settings, "radius", 8.0));
            return Ok(());
        }
        "bloom" => {
            bloom(image, &settings);
            return Ok(());
        }
        "liquify" => return crate::liquify::apply(image, &settings),
        _ => {}
    }
    let table: [u8; 256] = std::array::from_fn(|i| {
        (map_value(kind, &settings, i as f32 / 255.0) * 255.0).round() as u8
    });
    let balance = ["shadows", "midtones", "highlights"].map(|band| {
        std::array::from_fn::<_, 3, _>(|i| settings[band][i].as_f64().unwrap_or(0.0) as f32)
    });
    let preserve = settings["preserve_luminosity"].as_bool().unwrap_or(true);
    let hue = number(&settings, "hue", 0.0);
    let saturation = number(
        &settings,
        "saturation",
        if kind == "hsl" { 0.0 } else { 1.0 },
    );
    let lightness = number(&settings, "lightness", 0.0);
    for p in image.bytes.chunks_exact_mut(4) {
        if p[3] == 0 {
            continue;
        }
        match kind {
            "color_balance" => {
                let rgb = [
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ];
                let luma = luminance(rgb);
                let shadow = 1.0 - smoothstep(0.08, 0.48, luma);
                let highlight = smoothstep(0.52, 0.92, luma);
                let weights = [shadow, 1.0 - shadow - highlight, highlight];
                let mut adjusted: [f32; 3] = std::array::from_fn(|c| {
                    rgb[c] + 0.5 * (0..3).map(|b| balance[b][c] * weights[b]).sum::<f32>()
                });
                if preserve {
                    let correction = luma - luminance(adjusted);
                    for c in &mut adjusted {
                        *c += correction;
                    }
                    // Compress chroma around the original luminance instead of clipping it away.
                    let mut scale = 1.0_f32;
                    for c in adjusted {
                        let chroma = c - luma;
                        if chroma > 0.0 {
                            scale = scale.min((1.0 - luma) / chroma);
                        } else if chroma < 0.0 {
                            scale = scale.min(-luma / chroma);
                        }
                    }
                    for c in &mut adjusted {
                        *c = luma + (*c - luma) * scale;
                    }
                }
                for c in 0..3 {
                    p[c] = (adjusted[c] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            "hsl" => {
                if hue == 0.0 && saturation == 0.0 && lightness == 0.0 {
                    continue;
                }
                let rgb = hsl_adjust(
                    [
                        p[0] as f32 / 255.0,
                        p[1] as f32 / 255.0,
                        p[2] as f32 / 255.0,
                    ],
                    hue,
                    saturation,
                    lightness,
                );
                for c in 0..3 {
                    p[c] = (rgb[c] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
            "grayscale" => {
                let v = (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722).round()
                    as u8;
                p[..3].fill(v);
            }
            _ => {
                for c in 0..3 {
                    p[c] = table[p[c] as usize];
                }
                if kind == "adjust" {
                    let gray = p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722;
                    for c in 0..3 {
                        p[c] = (gray + (p[c] as f32 - gray) * saturation)
                            .round()
                            .clamp(0.0, 255.0) as u8;
                    }
                }
            }
        }
    }
    Ok(())
}
fn luminance(rgb: [f32; 3]) -> f32 {
    rgb[0] * 0.2126 + rgb[1] * 0.7152 + rgb[2] * 0.0722
}
fn smoothstep(low: f32, high: f32, value: f32) -> f32 {
    let t = ((value - low) / (high - low)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn hsl_adjust(rgb: [f32; 3], hue: f32, saturation: f32, lightness: f32) -> [f32; 3] {
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let delta = max - min;
    let mut l = (max + min) * 0.5;
    let mut h = if delta <= f32::EPSILON {
        0.0
    } else if max == rgb[0] {
        ((rgb[1] - rgb[2]) / delta).rem_euclid(6.0)
    } else if max == rgb[1] {
        (rgb[2] - rgb[0]) / delta + 2.0
    } else {
        (rgb[0] - rgb[1]) / delta + 4.0
    };
    let mut s = if delta <= f32::EPSILON {
        0.0
    } else {
        delta / (1.0 - (2.0 * l - 1.0).abs()).max(f32::EPSILON)
    };
    h = (h + hue / 60.0).rem_euclid(6.0);
    s = (s * (1.0 + saturation)).clamp(0.0, 1.0);
    l = if lightness < 0.0 {
        l * (1.0 + lightness)
    } else {
        l + (1.0 - l) * lightness
    };
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
    let m = l - c * 0.5;
    let out = match h as i32 {
        0 => [c, x, 0.0],
        1 => [x, c, 0.0],
        2 => [0.0, c, x],
        3 => [0.0, x, c],
        4 => [x, 0.0, c],
        _ => [c, 0.0, x],
    };
    out.map(|v| v + m)
}
fn bloom(image: &mut Image, settings: &Value) {
    let threshold = number(settings, "threshold", 0.75);
    let strength = number(settings, "strength", 0.5);
    if strength == 0.0
        || threshold == 1.0
        || !image.bytes.chunks_exact(4).any(|p| {
            p[3] > 0
                && luminance([
                    p[0] as f32 / 255.0,
                    p[1] as f32 / 255.0,
                    p[2] as f32 / 255.0,
                ]) > threshold
        })
    {
        return;
    }
    let mut glow = Vec::with_capacity(image.bytes.len());
    for p in image.bytes.chunks_exact(4) {
        let luma = luminance([
            p[0] as f32 / 255.0,
            p[1] as f32 / 255.0,
            p[2] as f32 / 255.0,
        ]);
        let emission = ((luma - threshold) / (1.0 - threshold).max(f32::EPSILON)).clamp(0.0, 1.0);
        let alpha = p[3] as f32 * 255.0 * emission;
        glow.extend(
            [
                p[0] as f32 * alpha / 255.0,
                p[1] as f32 * alpha / 255.0,
                p[2] as f32 * alpha / 255.0,
                alpha,
            ]
            .map(|v| v.round() as u16),
        );
    }
    blur_premultiplied(
        &mut glow,
        image.width as usize,
        image.height as usize,
        number(settings, "spread", 12.0),
    );
    for (p, g) in image.bytes.chunks_exact_mut(4).zip(glow.chunks_exact(4)) {
        let base_alpha = p[3] as f32 / 255.0;
        let glow_alpha = (g[3] as f32 / 65025.0 * strength).clamp(0.0, 1.0);
        let alpha = base_alpha + (1.0 - base_alpha) * glow_alpha;
        if alpha <= 0.0 {
            continue;
        }
        let out_alpha = (alpha * 255.0).round() as u8;
        for c in 0..3 {
            p[c] = if out_alpha == 0 {
                0
            } else {
                ((p[c] as f32 / 255.0 * base_alpha + g[c] as f32 / 65025.0 * strength).min(alpha)
                    / alpha
                    * 255.0)
                    .round()
                    .clamp(0.0, 255.0) as u8
            };
        }
        p[3] = out_alpha;
    }
}
/// Three linear-time box passes approximate a Gaussian. Premultiplied RGBA prevents dark fringes.
pub fn gaussian_radii(sigma: f32) -> [usize; 3] {
    if sigma < 0.5 {
        return [0; 3];
    }
    let ideal = (4.0 * sigma * sigma + 1.0).sqrt();
    let mut low = ideal.floor() as i32;
    if low % 2 == 0 {
        low -= 1;
    }
    low = low.max(1);
    let high = low + 2;
    let m = ((12.0 * sigma * sigma - 3.0 * (low * low) as f32 - 12.0 * low as f32 - 9.0)
        / (-4.0 * low as f32 - 4.0))
        .round()
        .clamp(0.0, 3.0) as usize;
    std::array::from_fn(|i| ((if i < m { low } else { high }) - 1) as usize / 2)
}
pub fn box_channel(
    src: &[u8],
    dst: &mut [u8],
    w: usize,
    h: usize,
    channels: usize,
    r: usize,
    vertical: bool,
) {
    let (lines, len, stride) = if vertical {
        (w, h, w * channels)
    } else {
        (h, w, channels)
    };
    let n = (r * 2 + 1) as i64;
    for line in 0..lines {
        let base = if vertical {
            line * channels
        } else {
            line * w * channels
        };
        for c in 0..channels {
            let at =
                |i: isize| src[base + i.clamp(0, len as isize - 1) as usize * stride + c] as i64;
            let mut sum: i64 = (-(r as isize)..=r as isize).map(at).sum();
            for i in 0..len {
                dst[base + i * stride + c] = ((sum + n / 2) / n) as u8;
                sum += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
            }
        }
    }
}
pub fn blur_rgba(bytes: &mut Vec<u8>, w: usize, h: usize, sigma: f32) {
    if !sigma.is_finite() || sigma < 0.5 || w == 0 || h == 0 {
        return;
    }
    if w.checked_mul(h).and_then(|v| v.checked_mul(4)) != Some(bytes.len()) {
        return;
    }
    // Keep 16-bit premultiplied values through every pass: faint colored edges retain their hue.
    let mut pixels = Vec::with_capacity(bytes.len());
    for p in bytes.chunks_exact(4) {
        pixels.extend([
            p[0] as u16 * p[3] as u16,
            p[1] as u16 * p[3] as u16,
            p[2] as u16 * p[3] as u16,
            p[3] as u16 * 255,
        ]);
    }
    blur_premultiplied(&mut pixels, w, h, sigma.clamp(0.0, 64.0));
    for (p, v) in bytes.chunks_exact_mut(4).zip(pixels.chunks_exact(4)) {
        p[3] = ((v[3] as u32 + 127) / 255).min(255) as u8;
        for c in 0..3 {
            p[c] = if p[3] == 0 || v[3] == 0 {
                0
            } else {
                ((v[c] as u32 * 255 + v[3] as u32 / 2) / v[3] as u32).min(255) as u8
            };
        }
    }
}
fn blur_premultiplied(pixels: &mut [u16], w: usize, h: usize, sigma: f32) {
    if w == 0 || h == 0 {
        return;
    }
    let mut line = vec![0u16; w.max(h) * 4];
    for radius in gaussian_radii(sigma) {
        if radius == 0 {
            continue;
        }
        for vertical in [false, true] {
            let (lines, len, stride) = if vertical { (w, h, w * 4) } else { (h, w, 4) };
            for index in 0..lines {
                let base = if vertical { index * 4 } else { index * w * 4 };
                for i in 0..len {
                    line[i * 4..i * 4 + 4]
                        .copy_from_slice(&pixels[base + i * stride..base + i * stride + 4]);
                }
                let n = (radius * 2 + 1) as u64;
                for c in 0..4 {
                    let at = |i: isize| line[i.clamp(0, len as isize - 1) as usize * 4 + c] as u64;
                    let mut sum: u64 = (-(radius as isize)..=radius as isize).map(at).sum();
                    for i in 0..len {
                        pixels[base + i * stride + c] = ((sum + n / 2) / n) as u16;
                        sum = sum + at(i as isize + radius as isize + 1)
                            - at(i as isize - radius as isize);
                    }
                }
            }
        }
    }
}
