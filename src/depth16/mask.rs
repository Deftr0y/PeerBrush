//! Lazy local masks and cached nonlocal native-16 masks.
use super::{color, Image16, MASK_BUDGET};
use crate::engine::{Layer, Mask};
use std::{
    collections::{BTreeSet, HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

#[derive(Clone, Debug)]
pub struct Gray16 {
    pub width: u32,
    pub height: u32,
    pub words: Vec<u16>,
    constant: Option<u16>,
}

pub(crate) fn regional(
    layer: &Layer,
    previous: Option<Arc<Gray16>>,
    area: [i32; 4],
) -> Result<Option<Arc<Gray16>>, String> {
    let Some(mask) = layer.mask.as_ref().filter(|mask| mask.needs_cache()) else {
        return Ok(None);
    };
    let (width, height) = (layer.pixels.width, layer.pixels.height);
    if uniform(mask) {
        return Ok(Some(Arc::new(evaluate(mask, width, height)?)));
    }
    let output = crate::preview::regions::intersect(area, width, height);
    let mut result = match previous {
        Some(image) => image,
        None => Arc::new(evaluate(mask, width, height)?),
    };
    if output[0] >= output[2] || output[1] >= output[3] {
        return Ok(Some(result));
    }
    let work = crate::preview::regions::window(
        output,
        crate::preview::regions::mask_reach(mask),
        width,
        height,
    );
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
    )?;
    let destination = Arc::make_mut(&mut result);
    if let Some(constant) = destination.constant.take() {
        destination.words = vec![constant; width as usize * height as usize];
    }
    for y in output[1]..output[3] {
        for x in output[0]..output[2] {
            destination.words[y as usize * width as usize + x as usize] =
                image.get(x - work[0], y - work[1]);
        }
    }
    Ok(Some(result))
}
impl Gray16 {
    pub fn get(&self, x: i32, y: i32) -> u16 {
        if let Some(value) = self.constant {
            return value;
        }
        if self.width == 0 || self.height == 0 {
            return 65535;
        }
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.words[y * self.width as usize + x]
    }
    pub fn value(&self, x: i32, y: i32) -> f64 {
        self.get(x, y) as f64 / 65535.0
    }
}
#[derive(Default)]
struct Cache {
    items: HashMap<String, Arc<Gray16>>,
    order: VecDeque<String>,
    bytes: usize,
}
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
fn uniform(mask: &Mask) -> bool {
    !mask.steps.iter().any(|s| {
        s.enabled
            && s.weight > 0.0
            && s.kind == "paint"
            && (!s.pixels.tiles.is_empty() || !s.pixels.samples16.is_empty())
    })
}
pub(super) fn buffer_bytes(layer: &Layer) -> u64 {
    layer
        .mask
        .as_ref()
        .filter(|m| m.needs_cache() && !uniform(m))
        .map_or(0, |_| {
            u64::from(layer.pixels.width) * u64::from(layer.pixels.height) * 2
        })
}
pub(crate) fn prepare(layer: &Layer) -> Result<Option<Arc<Gray16>>, String> {
    let Some(mask) = layer.mask.as_ref().filter(|m| m.needs_cache()) else {
        return Ok(None);
    };
    let (width, height) = (layer.pixels.width, layer.pixels.height);
    let key = format!("{}:16:{width}:{height}", mask.cache_key);
    let cache = CACHE.get_or_init(|| Mutex::new(Cache::default()));
    if let Some(image) = cache
        .lock()
        .map_err(|_| "16-bit mask cache unavailable")?
        .items
        .get(&key)
        .cloned()
    {
        return Ok(Some(image));
    }
    let image = Arc::new(evaluate(mask, width, height)?);
    let mut cache = cache.lock().map_err(|_| "16-bit mask cache unavailable")?;
    if let Some(existing) = cache.items.get(&key) {
        return Ok(Some(existing.clone()));
    }
    let bytes = image.words.len() * 2;
    while cache.bytes + bytes > MASK_BUDGET as usize {
        let Some(old) = cache.order.pop_front() else {
            break;
        };
        if let Some(old) = cache.items.remove(&old) {
            cache.bytes -= old.words.len() * 2;
        }
    }
    cache.bytes += bytes;
    cache.order.push_back(key.clone());
    cache.items.insert(key, image.clone());
    Ok(Some(image))
}
pub fn value(layer: &Layer, x: i32, y: i32, prepared: Option<&Gray16>, raw: bool) -> f64 {
    let Some(mask) = &layer.mask else {
        return 1.0;
    };
    if !raw && !mask.enabled {
        return 1.0;
    }
    if let Some(image) = prepared {
        return image.value(x, y);
    }
    let mut value = 65535.0;
    for step in mask.steps.iter().filter(|s| s.enabled && s.weight > 0.0) {
        let before = value;
        match step.kind.as_str() {
            "fill" => value = f64::from(step.value) * 257.0,
            "paint" => {
                let p = step.pixels.get16(x, y);
                let alpha = p[3] as f64 / 65535.0;
                value = value * (1.0 - alpha) + p[0] as f64 * alpha;
            }
            "invert" => value = 65535.0 - value,
            "levels" => {
                value = (value / 65535.0).powf(f64::from(step.value.clamp(0.1, 5.0))) * 65535.0
            }
            "curves" | "adjust" => {
                value = color::map(&step.kind, &step.settings, value / 65535.0) * 65535.0
            }
            _ => {}
        }
        value = before * (1.0 - f64::from(step.weight)) + value * f64::from(step.weight);
    }
    value.clamp(0.0, 65535.0) / 65535.0
}
fn evaluate(mask: &Mask, width: u32, height: u32) -> Result<Gray16, String> {
    let count = width as usize * height as usize;
    if count as u64 * 2 > MASK_BUDGET {
        return Err("Native16 spatial mask exceeds the128 MiB mask budget".into());
    }
    let constant = uniform(mask);
    let mut values = vec![65535u16; if constant { 1 } else { count }];
    for step in mask.steps.iter().filter(|s| s.enabled && s.weight > 0.0) {
        let before = (step.weight < 1.0).then(|| values.clone());
        match step.kind.as_str() {
            "fill" => {
                values.fill((f64::from(step.value) * 257.0).round().clamp(0.0, 65535.0) as u16)
            }
            "paint" if !constant => {
                let keys: BTreeSet<_> = step
                    .pixels
                    .tiles
                    .keys()
                    .chain(step.pixels.samples16.keys())
                    .copied()
                    .collect();
                for (tx, ty) in keys {
                    let source16 = step.pixels.samples16.get(&(tx, ty));
                    let source8 = step.pixels.tiles.get(&(tx, ty));
                    let left = tx * crate::raster::TILE;
                    let top = ty * crate::raster::TILE;
                    for y in top..(top + crate::raster::TILE)
                        .min(height)
                        .min(step.pixels.height)
                    {
                        for x in left..(left + crate::raster::TILE)
                            .min(width)
                            .min(step.pixels.width)
                        {
                            let at = (((y - top) * crate::raster::TILE + x - left) * 4) as usize;
                            let (red, alpha) = if let Some(tile) = source16 {
                                (tile[at], tile[at + 3])
                            } else if let Some(tile) = source8 {
                                (u16::from(tile[at]) * 257, u16::from(tile[at + 3]) * 257)
                            } else {
                                continue;
                            };
                            if alpha == 0 {
                                continue;
                            }
                            let target = (y * width + x) as usize;
                            values[target] = ((u64::from(values[target])
                                * (65535 - u64::from(alpha))
                                + u64::from(red) * u64::from(alpha)
                                + 32767)
                                / 65535) as u16;
                        }
                    }
                }
            }
            "invert" => {
                for v in &mut values {
                    *v = 65535 - *v;
                }
            }
            "levels" => {
                for v in &mut values {
                    *v = ((*v as f64 / 65535.0).powf(f64::from(step.value.clamp(0.1, 5.0)))
                        * 65535.0)
                        .round()
                        .clamp(0.0, 65535.0) as u16;
                }
            }
            "curves" | "adjust" => {
                let table: Vec<u16> = (0..65536)
                    .map(|v| {
                        (color::map(&step.kind, &step.settings, v as f64 / 65535.0) * 65535.0)
                            .round()
                            .clamp(0.0, 65535.0) as u16
                    })
                    .collect();
                for v in &mut values {
                    *v = table[*v as usize];
                }
            }
            "gaussian" if !constant => {
                for radius in crate::effects::gaussian_radii(crate::effects::number(
                    &step.settings,
                    "radius",
                    8.0,
                )) {
                    box_pass(&mut values, width as usize, height as usize, radius, false);
                    box_pass(&mut values, width as usize, height as usize, radius, true);
                }
            }
            "blur" if !constant && step.value >= 0.5 => {
                let radius = (step.value.round().clamp(1.0, 64.0) as usize).div_ceil(3);
                for _ in 0..3 {
                    box_pass(&mut values, width as usize, height as usize, radius, false);
                    box_pass(&mut values, width as usize, height as usize, radius, true);
                }
            }
            _ => {}
        }
        if let Some(before) = before {
            for (v, old) in values.iter_mut().zip(before) {
                *v = (old as f64 * (1.0 - step.weight as f64) + *v as f64 * step.weight as f64)
                    .round()
                    .clamp(0.0, 65535.0) as u16;
            }
        }
    }
    Ok(Gray16 {
        width,
        height,
        constant: constant.then(|| values[0]),
        words: if constant { vec![] } else { values },
    })
}
fn box_pass(values: &mut [u16], width: usize, height: usize, radius: usize, vertical: bool) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }
    let mut ring = vec![0u16; radius + 1];
    let count = (radius * 2 + 1) as u64;
    let (lines, length, stride) = if vertical {
        (width, height, width)
    } else {
        (height, width, 1)
    };
    for line in 0..lines {
        let base = if vertical { line } else { line * width };
        let first = values[base];
        let last = values[base + (length - 1) * stride];
        let mut sum = (radius as u64 + 1) * u64::from(first);
        for offset in 1..=radius {
            sum += u64::from(values[base + offset.min(length - 1) * stride]);
        }
        for offset in 0..length {
            ring[offset % (radius + 1)] = values[base + offset * stride];
            values[base + offset * stride] = ((sum + count / 2) / count) as u16;
            let add = if offset + radius + 1 >= length {
                last
            } else {
                values[base + (offset + radius + 1) * stride]
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
pub(super) fn image(layer: &Layer) -> Result<Option<Arc<Image16>>, String> {
    if layer.mask.is_none() {
        return Ok(None);
    }
    let prepared = prepare(layer)?;
    let mut image = Image16::new(layer.pixels.width, layer.pixels.height)?;
    for y in 0..image.height {
        for x in 0..image.width {
            let v = (value(layer, x as i32, y as i32, prepared.as_deref(), true) * 65535.0).round()
                as u16;
            let at = ((y * image.width + x) * 4) as usize;
            image.words[at..at + 4].copy_from_slice(&[v, v, v, 65535]);
        }
    }
    Ok(Some(Arc::new(image)))
}
