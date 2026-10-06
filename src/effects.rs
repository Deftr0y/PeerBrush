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
pub const KINDS: &[&str] = &["levels", "curves", "blur", "adjust", "invert", "grayscale"];
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Effect {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    pub settings: Value,
}
pub fn defaults(kind: &str) -> Value {
    match kind {
        "levels" => json!({"black":0.0,"white":1.0,"gamma":1.0}),
        "curves" => json!({"points":[[0.0,0.0],[0.5,0.5],[1.0,1.0]]}),
        "blur" => json!({"radius":8.0}),
        "adjust" => json!({"brightness":0.0,"contrast":1.0,"saturation":1.0}),
        _ => json!({}),
    }
}
pub fn number(v: &Value, key: &str, default: f32) -> f32 {
    v[key].as_f64().unwrap_or(default as f64) as f32
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
        "curves" => {
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
pub fn map_value(kind: &str, settings: &Value, v: f32) -> f32 {
    match kind {
        "levels" => {
            let b = number(settings, "black", 0.0);
            let w = number(settings, "white", 1.0);
            ((v - b) / (w - b).max(0.01))
                .clamp(0.0, 1.0)
                .powf(1.0 / number(settings, "gamma", 1.0))
        }
        "curves" => {
            let points = settings["points"].as_array().unwrap();
            for pair in points.windows(2) {
                let x = pair[0][0].as_f64().unwrap() as f32;
                let nx = pair[1][0].as_f64().unwrap() as f32;
                if v <= nx {
                    let y = pair[0][1].as_f64().unwrap() as f32;
                    let ny = pair[1][1].as_f64().unwrap() as f32;
                    return y + (ny - y) * ((v - x) / (nx - x)).clamp(0.0, 1.0);
                }
            }
            points.last().unwrap()[1].as_f64().unwrap() as f32
        }
        "invert" => 1.0 - v,
        "adjust" => ((v - 0.5) * number(settings, "contrast", 1.0)
            + 0.5
            + number(settings, "brightness", 0.0))
        .clamp(0.0, 1.0),
        _ => v,
    }
}
#[derive(Debug)]
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
        while self.bytes + image.bytes.len() > BUDGET {
            let Some(k) = self.order.pop_front() else {
                break;
            };
            if let Some(v) = self.items.remove(&k) {
                self.bytes -= v.bytes.len();
            }
        }
        self.bytes += image.bytes.len();
        self.order.push_back(key.clone());
        self.items.insert(key, image);
    }
}
pub fn active(l: &Layer) -> bool {
    l.effects.iter().any(|e| e.enabled)
}
pub fn validate_budget(doc: &Document) -> Result<(), String> {
    let bytes: u64 = doc
        .layers
        .iter()
        .filter(|l| active(l))
        .map(|l| {
            if l.kind == "group" {
                doc.width as u64 * doc.height as u64 * 4
            } else {
                l.pixels.width as u64 * l.pixels.height as u64 * 4
            }
        })
        .sum();
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
    let depth = |l: &Layer| {
        let mut d = 0;
        let mut p = l.parent.as_deref();
        while let Some(parent) = p {
            d += 1;
            p = doc
                .layers
                .iter()
                .find(|n| n.id == parent)
                .and_then(|n| n.parent.as_deref());
            if d > 16 {
                break;
            }
        }
        d
    };
    let mut order = (0..doc.layers.len()).collect::<Vec<_>>();
    order.sort_by_key(|&i| std::cmp::Reverse(depth(&doc.layers[i])));
    for i in order {
        let l = &doc.layers[i];
        if !active(l) {
            continue;
        }
        let (w, h) = if l.kind == "group" {
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
        let mut image = Image {
            width: w,
            height: h,
            bytes: vec![0; w as usize * h as usize * 4],
        };
        for y in 0..h {
            for x in 0..w {
                let p = if l.kind == "group" {
                    doc.sample_group(Some(&l.id), x as i32, y as i32, masks, &out)
                } else if l.kind == "fill" {
                    l.color
                } else {
                    l.pixels.get(x as i32, y as i32)
                };
                let at = ((y * w + x) * 4) as usize;
                image.bytes[at..at + 4].copy_from_slice(&p);
            }
        }
        for effect in l.effects.iter().filter(|e| e.enabled) {
            apply(&mut image, &effect.kind, &effect.settings);
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
pub fn apply(image: &mut Image, kind: &str, settings: &Value) {
    if kind == "blur" {
        blur_rgba(
            &mut image.bytes,
            image.width as usize,
            image.height as usize,
            number(settings, "radius", 8.0),
        );
        return;
    }
    let table: [u8; 256] = std::array::from_fn(|i| {
        (map_value(kind, settings, i as f32 / 255.0) * 255.0).round() as u8
    });
    for p in image.bytes.chunks_exact_mut(4) {
        if kind == "grayscale" {
            let v =
                (p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722).round() as u8;
            p[..3].fill(v);
        } else {
            for c in 0..3 {
                p[c] = table[p[c] as usize];
            }
            if kind == "adjust" {
                let gray = p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722;
                let sat = number(settings, "saturation", 1.0);
                for c in 0..3 {
                    p[c] = (gray + (p[c] as f32 - gray) * sat)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            }
        }
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
    if sigma < 0.5 || w == 0 || h == 0 {
        return;
    }
    // u8 premultiplication is sufficient for the initial 8-bit editing pipeline.
    for p in bytes.chunks_exact_mut(4) {
        for c in 0..3 {
            p[c] = ((p[c] as u32 * p[3] as u32 + 127) / 255) as u8;
        }
    }
    let mut scratch = vec![0; bytes.len()];
    for radius in gaussian_radii(sigma) {
        if radius == 0 {
            continue;
        }
        box_channel(bytes, &mut scratch, w, h, 4, radius, false);
        box_channel(&scratch, bytes, w, h, 4, radius, true);
    }
    for p in bytes.chunks_exact_mut(4) {
        if p[3] == 0 {
            p[..3].fill(0);
        } else {
            for c in 0..3 {
                p[c] = ((p[c] as u32 * 255 + p[3] as u32 / 2) / p[3] as u32).min(255) as u8;
            }
        }
    }
}
