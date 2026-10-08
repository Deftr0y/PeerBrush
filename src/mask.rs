//! Derived grayscale masks stay out of PSD source data and undo snapshots.
//! Three sliding-window box passes feather edges in linear time.
use crate::engine::{Layer, Mask};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

pub const CACHE_BUDGET: usize = 128 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct GrayMask {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

/// Update only affected output samples; include the complete filter support in the input crop.
pub(crate) fn regional(
    mask: &Mask,
    width: u32,
    height: u32,
    previous: Option<Arc<GrayMask>>,
    area: [i32; 4],
) -> Option<Arc<GrayMask>> {
    if !mask.needs_cache() {
        return None;
    }
    let output = crate::preview::regions::intersect(area, width, height);
    let mut result = previous.unwrap_or_else(|| Arc::new(evaluate(mask, width, height)));
    if output[0] >= output[2] || output[1] >= output[3] {
        return Some(result);
    }
    let reach = crate::preview::regions::mask_reach(mask);
    let work = crate::preview::regions::window(output, reach, width, height);
    let mut cropped = mask.clone();
    for step in &mut cropped.steps {
        if step.kind == "paint" {
            step.pixels = crate::preview::regions::crop(&step.pixels, work);
        }
    }
    let image = evaluate(
        &cropped,
        (work[2] - work[0]) as u32,
        (work[3] - work[1]) as u32,
    );
    let destination = Arc::make_mut(&mut result);
    for y in output[1]..output[3] {
        for x in output[0]..output[2] {
            destination.pixels[y as usize * width as usize + x as usize] = image.pixels
                [(y - work[1]) as usize * image.width as usize + (x - work[0]) as usize];
        }
    }
    Some(result)
}
impl GrayMask {
    pub fn value(&self, x: i32, y: i32) -> f32 {
        if self.width == 0 || self.height == 0 {
            return 1.0;
        }
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.pixels[y * self.width as usize + x] as f32 / 255.0
    }
}

#[derive(Default)]
struct Cache {
    items: HashMap<String, Arc<GrayMask>>,
    order: VecDeque<String>,
    bytes: usize,
}
impl Cache {
    fn get(&mut self, key: &str) -> Option<Arc<GrayMask>> {
        let result = self.items.get(key)?.clone();
        self.order.retain(|k| k != key);
        self.order.push_back(key.into());
        Some(result)
    }
    fn insert(&mut self, key: String, image: Arc<GrayMask>) {
        let size = image.pixels.len();
        while self.bytes + size > CACHE_BUDGET {
            let Some(old) = self.order.pop_front() else {
                break;
            };
            if let Some(item) = self.items.remove(&old) {
                self.bytes -= item.pixels.len();
            }
        }
        self.bytes += size;
        self.order.push_back(key.clone());
        self.items.insert(key, image);
    }
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

impl Mask {
    pub fn needs_cache(&self) -> bool {
        self.steps.iter().any(|s| {
            s.enabled
                && (["curves", "adjust", "gaussian"].contains(&s.kind.as_str())
                    || (s.kind == "blur" && s.value >= 0.5))
        })
    }
    pub fn prepare(&self, width: u32, height: u32) -> Option<Arc<GrayMask>> {
        if !self.needs_cache() {
            return None;
        }
        let key = format!("{}:{width}:{height}", self.cache_key);
        let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
        if let Some(image) = cache.lock().unwrap().get(&key) {
            return Some(image);
        }
        let image = Arc::new(evaluate(self, width, height));
        let mut cache = cache.lock().unwrap();
        if let Some(existing) = cache.get(&key) {
            return Some(existing);
        }
        cache.insert(key, image.clone());
        Some(image)
    }
}

pub fn validate_budget(layers: &[Layer]) -> Result<(), String> {
    let bytes: u64 = layers
        .iter()
        .filter(|l| l.mask.as_ref().is_some_and(Mask::needs_cache))
        .map(|l| l.pixels.width as u64 * l.pixels.height as u64)
        .sum();
    if bytes > CACHE_BUDGET as u64 {
        Err("Feathered masks exceed the initial 128 MiB effect budget. Reduce the number of feathered masks or the canvas size.".into())
    } else {
        Ok(())
    }
}

fn evaluate(mask: &Mask, width: u32, height: u32) -> GrayMask {
    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![255; w * h];
    let mut scratch = Vec::new();
    for step in mask.steps.iter().filter(|s| s.enabled) {
        match step.kind.as_str() {
            "fill" => pixels.fill(step.value.round().clamp(0., 255.) as u8),
            "paint" => {
                for y in 0..h {
                    for x in 0..w {
                        let p = step.pixels.get(x as i32, y as i32);
                        let at = y * w + x;
                        pixels[at] = ((pixels[at] as u32 * (255 - p[3] as u32)
                            + p[0] as u32 * p[3] as u32
                            + 127)
                            / 255) as u8;
                    }
                }
            }
            "invert" => {
                for value in &mut pixels {
                    *value = 255 - *value;
                }
            }
            "levels" => {
                let table: [u8; 256] = std::array::from_fn(|i| {
                    ((i as f32 / 255.).powf(step.value.clamp(0.1, 5.)) * 255.).round() as u8
                });
                for value in &mut pixels {
                    *value = table[*value as usize];
                }
            }
            "curves" | "adjust" => {
                let table: [u8; 256] = std::array::from_fn(|i| {
                    (crate::effects::map_value(&step.kind, &step.settings, i as f32 / 255.0)
                        * 255.0)
                        .round() as u8
                });
                for value in &mut pixels {
                    *value = table[*value as usize];
                }
            }
            "gaussian" if w > 0 && h > 0 => {
                scratch.resize(w * h, 0);
                for radius in crate::effects::gaussian_radii(crate::effects::number(
                    &step.settings,
                    "radius",
                    8.0,
                )) {
                    if radius == 0 {
                        continue;
                    }
                    crate::effects::box_channel(&pixels, &mut scratch, w, h, 1, radius, false);
                    crate::effects::box_channel(&scratch, &mut pixels, w, h, 1, radius, true);
                }
            }
            "blur" if step.value >= 0.5 && w > 0 && h > 0 => {
                scratch.resize(w * h, 0);
                let radius = (step.value.round().clamp(1., 64.) as usize).div_ceil(3);
                for _ in 0..3 {
                    box_pass(&pixels, &mut scratch, w, h, radius, false);
                    box_pass(&scratch, &mut pixels, w, h, radius, true);
                }
            }
            _ => {}
        }
    }
    GrayMask {
        width,
        height,
        pixels,
    }
}

fn box_pass(source: &[u8], target: &mut [u8], w: usize, h: usize, radius: usize, vertical: bool) {
    let (lines, length, stride) = if vertical { (w, h, w) } else { (h, w, 1) };
    let diameter = (radius * 2 + 1) as u32;
    for line in 0..lines {
        let base = if vertical { line } else { line * w };
        let at = |i: isize| source[base + i.clamp(0, length as isize - 1) as usize * stride] as u32;
        let mut sum: u32 = (-(radius as isize)..=radius as isize).map(at).sum();
        for i in 0..length {
            target[base + i * stride] = ((sum + diameter / 2) / diameter) as u8;
            sum = sum + at(i as isize + radius as isize + 1) - at(i as isize - radius as isize);
        }
    }
}

/// A cumulative effect thumbnail, including the preceding stack steps.
pub fn step_preview(layer: &Layer, index: usize, edge: u32) -> Result<(u32, u32, Vec<u8>), String> {
    let mut layer = layer.clone();
    let mask = layer.mask.as_mut().ok_or("Layer has no mask")?;
    if index >= mask.steps.len() {
        return Err("Unknown mask step".into());
    }
    if index + 1 < mask.steps.len() {
        mask.cache_key = format!("{}:prefix:{index}", mask.cache_key);
    }
    mask.steps.truncate(index + 1);
    mask.enabled = true;
    let prepared = mask.prepare(layer.pixels.width, layer.pixels.height);
    let scale = (edge as f32 / layer.pixels.width.max(layer.pixels.height).max(1) as f32).min(1.);
    let w = (layer.pixels.width as f32 * scale).round().max(1.) as u32;
    let h = (layer.pixels.height as f32 * scale).round().max(1.) as u32;
    let mut rgba = vec![0; w as usize * h as usize * 4];
    for y in 0..h {
        for x in 0..w {
            let value = (layer.mask_value_prepared(
                (x as f32 / scale) as i32,
                (y as f32 / scale) as i32,
                prepared.as_deref(),
                true,
            ) * 255.)
                .round() as u8;
            let at = ((y * w + x) * 4) as usize;
            rgba[at..at + 4].copy_from_slice(&[value, value, value, 255]);
        }
    }
    Ok((w, h, rgba))
}
