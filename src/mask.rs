//! Derived grayscale masks stay out of PSD source data and undo snapshots.
//! Three sliding-window box passes feather edges in linear time.
use crate::engine::{Layer, Mask};
pub(crate) mod commands;
pub(crate) mod region;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

pub const CACHE_BUDGET: usize = 128 * 1024 * 1024;

/// Density operates on native mask samples. Rounding here also makes a
/// standard raw mask followed by its density record reproduce this stage.
pub(crate) fn density_sample(sample: f64, density: f32, maximum: u16) -> u16 {
    let sample = sample.round().clamp(0., f64::from(maximum)) as u16;
    let inverse = u32::from(maximum - sample);
    let density = density.round().clamp(0., 255.) as u32;
    maximum - ((inverse * density + 127) / 255) as u16
}

pub(crate) fn paint_step_index(mask: &Mask, requested: Option<&str>) -> Result<usize, String> {
    let index = if let Some(id) = requested {
        mask.steps.iter().position(|step| step.id == id && step.kind == "paint")
    } else {
        mask.steps.iter().rposition(|step| step.kind == "paint")
    };
    index.ok_or_else(|| "Add a paint step to the mask".into())
}

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
    check: &crate::render::Check<'_>,
) -> Result<Option<Arc<GrayMask>>, String> {
    check()?;
    if !mask.needs_cache() {
        return Ok(None);
    }
    let Some(mut result) = previous else {
        return Ok(Some(Arc::new(evaluate_checked(mask, width, height, check)?)));
    };
    let output = crate::preview::regions::intersect(area, width, height);
    if output[0] >= output[2] || output[1] >= output[3] {
        return Ok(Some(result));
    }
    let reach = crate::preview::regions::mask_reach(mask);
    let work = crate::preview::regions::window(output, reach, width, height);
    let mut cropped = mask.clone();
    for step in &mut cropped.steps {
        if step.kind == "paint" {
            step.pixels = crate::preview::regions::crop(&step.pixels, work, check)?;
        }
    }
    let image = evaluate_checked(
        &cropped,
        (work[2] - work[0]) as u32,
        (work[3] - work[1]) as u32,
        check,
    )?;
    let destination = Arc::make_mut(&mut result);
    for y in output[1]..output[3] {
        check()?;
        for x in output[0]..output[2] {
            destination.pixels[y as usize * width as usize + x as usize] = image.pixels
                [(y - work[1]) as usize * image.width as usize + (x - work[0]) as usize];
        }
    }
    Ok(Some(result))
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
        self.steps
            .iter()
            .any(|s| needs_cache_step(&s.kind, s.enabled, s.weight, s.value))
    }
    pub fn prepare(&self, width: u32, height: u32) -> Option<Arc<GrayMask>> {
        self.prepare_checked(width, height, &|| Ok(()))
            .expect("Uncancelled mask preparation")
    }
    fn prepare_checked(
        &self,
        width: u32,
        height: u32,
        check: &crate::render::Check<'_>,
    ) -> Result<Option<Arc<GrayMask>>, String> {
        check()?;
        if !self.needs_cache() {
            return Ok(None);
        }
        let key = format!("{}:{width}:{height}", self.cache_key);
        let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
        if let Some(image) = cache.lock().unwrap().get(&key) {
            return Ok(Some(image));
        }
        let image = Arc::new(evaluate_checked(self, width, height, check)?);
        let mut cache = cache.lock().unwrap();
        if let Some(existing) = cache.get(&key) {
            return Ok(Some(existing));
        }
        cache.insert(key, image.clone());
        Ok(Some(image))
    }
}

pub(crate) fn prepare_layers(
    layers: &[Layer],
    check: &crate::render::Check<'_>,
) -> Result<Vec<Option<Arc<GrayMask>>>, String> {
    validate_budget(layers)?;
    layers.iter()
        .map(|layer| {
            let Some(mask) = &layer.mask else { return Ok(None) };
            mask.prepare_checked(layer.pixels.width, layer.pixels.height, check)
        })
        .collect()
}

pub fn validate_budget(layers: &[Layer]) -> Result<(), String> {
    let bytes: u64 = layers
        .iter()
        .filter(|l| l.mask.as_ref().is_some_and(Mask::needs_cache))
        .map(|l| l.pixels.width as u64 * l.pixels.height as u64)
        .sum();
    validate_cache_bytes(bytes)
}
pub(crate) fn needs_cache_step(kind: &str, enabled: bool, weight: f32, value: f32) -> bool {
    enabled
        && weight > 0.0
        && (["curves", "adjust", "gaussian"].contains(&kind) || (kind == "blur" && value >= 0.5))
}
pub(crate) fn validate_cache_bytes(bytes: u64) -> Result<(), String> {
    if bytes > CACHE_BUDGET as u64 {
        Err("Feathered masks exceed the initial 128 MiB effect budget. Reduce the number of feathered masks or the canvas size.".into())
    } else {
        Ok(())
    }
}

pub(crate) fn evaluate(mask: &Mask, width: u32, height: u32) -> GrayMask {
    evaluate_checked(mask, width, height, &|| Ok(())).expect("Uncancelled mask evaluation")
}

fn evaluate_checked(
    mask: &Mask,
    width: u32,
    height: u32,
    check: &crate::render::Check<'_>,
) -> Result<GrayMask, String> {
    check()?;
    let (w, h) = (width as usize, height as usize);
    let mut pixels = vec![255; w * h];
    let mut scratch = Vec::new();
    for step in mask.steps.iter().filter(|s| s.enabled && s.weight > 0.0) {
        check()?;
        let before = (step.weight < 1.0).then(|| pixels.clone());
        match step.kind.as_str() {
            "fill" => pixels.fill(step.value.round().clamp(0., 255.) as u8),
            "paint" => {
                for y in 0..h {
                    check()?;
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
            "density" => {
                for value in &mut pixels {
                    *value = density_sample(f64::from(*value), step.value, 255) as u8;
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
                    crate::effects::box_channel_checked(&pixels, &mut scratch, w, h, 1, radius, false, check)?;
                    crate::effects::box_channel_checked(&scratch, &mut pixels, w, h, 1, radius, true, check)?;
                }
            }
            "blur" if step.value >= 0.5 && w > 0 && h > 0 => {
                scratch.resize(w * h, 0);
                let radius = (step.value.round().clamp(1., 64.) as usize).div_ceil(3);
                for _ in 0..3 {
                    box_pass(&pixels, &mut scratch, w, h, radius, false, check)?;
                    box_pass(&scratch, &mut pixels, w, h, radius, true, check)?;
                }
            }
            _ => {}
        }
        if let Some(before) = before {
            for (v, old) in pixels.iter_mut().zip(before) {
                *v = (old as f64 * (1.0 - step.weight as f64) + *v as f64 * step.weight as f64)
                    .round()
                    .clamp(0.0, 255.0) as u8;
            }
        }
    }
    check()?;
    Ok(GrayMask {
        width,
        height,
        pixels,
    })
}

fn box_pass(
    source: &[u8],
    target: &mut [u8],
    w: usize,
    h: usize,
    radius: usize,
    vertical: bool,
    check: &crate::render::Check<'_>,
) -> Result<(), String> {
    let (lines, length, stride) = if vertical { (w, h, w) } else { (h, w, 1) };
    let diameter = (radius * 2 + 1) as u32;
    for line in 0..lines {
        check()?;
        let base = if vertical { line } else { line * w };
        let at = |i: isize| source[base + i.clamp(0, length as isize - 1) as usize * stride] as u32;
        let mut sum: u32 = (-(radius as isize)..=radius as isize).map(at).sum();
        for i in 0..length {
            target[base + i * stride] = ((sum + diameter / 2) / diameter) as u8;
            sum = sum + at(i as isize + radius as isize + 1) - at(i as isize - radius as isize);
        }
    }
    Ok(())
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
