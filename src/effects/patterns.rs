//! Embedded PNG pattern sources. Decoded samples are derived, bounded and never serialized.
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::Value;
use std::{
    collections::VecDeque,
    io::Read,
    path::Path,
    sync::{Arc, Mutex, OnceLock},
};

pub const ENCODED_LIMIT: usize = 4 * 1024 * 1024;
pub const EDGE_LIMIT: u32 = 512;
const CACHE_LIMIT: usize = 16 * 1024 * 1024;
const DOCUMENT_LIMIT: usize = 32 * 1024 * 1024;
type Cache = VecDeque<(String, Arc<Bitmap>)>;

pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    pub depth: u16,
    words: Vec<u16>,
}
impl Bitmap {
    pub fn bytes(&self) -> usize {
        self.words.len() * 2
    }
    fn pixel(&self, x: i64, y: i64) -> [u16; 4] {
        let at = (y.rem_euclid(i64::from(self.height)) as usize * self.width as usize
            + x.rem_euclid(i64::from(self.width)) as usize)
            * 4;
        self.words[at..at + 4].try_into().unwrap()
    }
    /// Inverse mapping of document pixel centers; positive angles rotate clockwise.
    pub fn sample(&self, x: f64, y: f64, settings: &Value) -> [u16; 4] {
        let scale = settings["bitmap_scale"].as_f64().unwrap_or(1.);
        let angle = settings["bitmap_angle"].as_f64().unwrap_or(0.).to_radians();
        let x = x + 0.5 - settings["phase_x"].as_f64().unwrap_or(0.);
        let y = y + 0.5 - settings["phase_y"].as_f64().unwrap_or(0.);
        let (x, y) = (
            (x * angle.cos() + y * angle.sin()) / scale - 0.5,
            (-x * angle.sin() + y * angle.cos()) / scale - 0.5,
        );
        if settings["sampling"] == "nearest" {
            return self.pixel((x + 0.5).floor() as i64, (y + 0.5).floor() as i64);
        }
        let (ix, iy) = (x.floor() as i64, y.floor() as i64);
        let (fx, fy) = (x - x.floor(), y - y.floor());
        let samples = [
            self.pixel(ix, iy),
            self.pixel(ix + 1, iy),
            self.pixel(ix, iy + 1),
            self.pixel(ix + 1, iy + 1),
        ];
        let weights = [
            (1. - fx) * (1. - fy),
            fx * (1. - fy),
            (1. - fx) * fy,
            fx * fy,
        ];
        let alpha: f64 = samples
            .iter()
            .zip(weights)
            .map(|(p, w)| f64::from(p[3]) * w)
            .sum();
        if alpha <= 0. {
            return [0; 4];
        }
        let mut result = [0; 4];
        for c in 0..3 {
            result[c] = (samples
                .iter()
                .zip(weights)
                .map(|(p, w)| f64::from(p[c]) * f64::from(p[3]) * w)
                .sum::<f64>()
                / alpha)
                .round()
                .clamp(0., 65535.) as u16;
        }
        result[3] = alpha.round().clamp(0., 65535.) as u16;
        result
    }
}
pub fn active(kind: &str, settings: &Value) -> bool {
    matches!(kind, "pattern_overlay" | "texture") && settings["pattern"] == "bitmap"
}
pub fn get(source: &str) -> Result<Arc<Bitmap>, String> {
    if source.is_empty() || source.len() > ENCODED_LIMIT.div_ceil(3) * 4 {
        return Err("Bitmap patterns require a base64 PNG of at most 4 MiB".into());
    }
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    {
        let mut cache = cache.lock().unwrap();
        if let Some(at) = cache.iter().position(|(key, _)| key == source) {
            let item = cache.remove(at).unwrap();
            let bitmap = item.1.clone();
            cache.push_back(item);
            return Ok(bitmap);
        }
    }
    let bytes = STANDARD
        .decode(source)
        .map_err(|_| "Bitmap pattern is not valid base64")?;
    if bytes.len() > ENCODED_LIMIT {
        return Err("Bitmap PNG exceeds 4 MiB".into());
    }
    let image = crate::image_import::Encoded::png(bytes)?;
    if image.info.choice.is_some() {
        return Err(
            "Bitmap patterns require a static PNG; export an explicit animation frame first".into(),
        );
    }
    if image.info.width > EDGE_LIMIT || image.info.height > EDGE_LIMIT {
        return Err("Bitmap patterns are limited to 512 × 512 pixels".into());
    }
    let pixels = image.decode(&serde_json::json!({}), 4 * 1024 * 1024)?;
    let bitmap = Arc::new(Bitmap {
        width: pixels.width,
        height: pixels.height,
        depth: pixels.depth,
        words: pixels.rgba16(),
    });
    let mut cache = cache.lock().unwrap();
    if let Some((_, prior)) = cache.iter().find(|(key, _)| key == source) {
        return Ok(prior.clone());
    }
    let cost = source.len() + bitmap.bytes();
    let mut bytes: usize = cache
        .iter()
        .map(|(key, value)| key.len() + value.bytes())
        .sum();
    while bytes + cost > CACHE_LIMIT {
        if let Some((key, value)) = cache.pop_front() {
            bytes -= key.len() + value.bytes();
        } else {
            break;
        }
    }
    cache.push_back((source.into(), bitmap.clone()));
    Ok(bitmap)
}
pub fn from_settings(kind: &str, settings: &Value) -> Result<Option<Arc<Bitmap>>, String> {
    if active(kind, settings) {
        Ok(Some(get(settings["bitmap_png"]
            .as_str()
            .ok_or("Bitmap pattern requires bitmap_png")?)?))
    } else {
        Ok(None)
    }
}
pub fn validate_document(doc: &crate::engine::Document) -> Result<(), String> {
    let mut total = 0usize;
    for effect in doc.layers.iter().flat_map(|l| &l.effects) {
        if let Some(bitmap) = from_settings(&effect.kind, &effect.settings)? {
            if bitmap.depth == 16 && doc.bit_depth != 16 {
                return Err("A 16-bit bitmap pattern requires a 16-bit document".into());
            }
            total += effect.settings["bitmap_png"].as_str().unwrap().len() + bitmap.bytes();
            if total > DOCUMENT_LIMIT {
                return Err("Bitmap pattern sources exceed the 32 MiB document budget".into());
            }
        }
    }
    Ok(())
}
/// Embed bytes, never a filesystem path. The shared decoder normalizes supported ICC at native depth.
pub fn read_file(path: &Path) -> Result<String, String> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .map_err(|e| e.to_string())?
        .take(ENCODED_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > ENCODED_LIMIT {
        return Err("Bitmap PNG exceeds 4 MiB".into());
    }
    let source = STANDARD.encode(bytes);
    get(&source)?;
    Ok(source)
}
