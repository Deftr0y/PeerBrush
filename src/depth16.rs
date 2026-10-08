//! Native 16-bit document rendering. Only display previews project to bytes.
//! Editable sources, masks, effects, clipboard crops and PSD composites retain
//! all 65,536 sample values; the existing fast 8-bit renderer stays independent.
pub mod color;
mod compositor;
pub mod mask;
pub use crate::raster::Pixel16;
use crate::{effects, engine::Document, raster::check_size};
pub use compositor::Plan16;
pub use mask::Gray16;
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};
pub const COLOR_BUDGET: u64 = 128 * 1024 * 1024;
pub const MASK_BUDGET: u64 = 128 * 1024 * 1024;
// A 256 MiB premultiplied working image plus at most a bounded radius ring.
pub const WORKING_BUDGET: u64 = 256 * 1024 * 1024 + 4096;

#[derive(Clone, Debug)]
pub struct Image16 {
    pub width: u32,
    pub height: u32,
    pub words: Vec<u16>,
}
impl Image16 {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        check_size(width, height)?;
        let count = u64::from(width) * u64::from(height) * 4;
        if count * 2 > WORKING_BUDGET {
            return Err("16-bit image exceeds the bounded image budget".into());
        }
        Ok(Self {
            width,
            height,
            words: vec![0; count as usize],
        })
    }
    pub fn get(&self, x: i32, y: i32) -> Pixel16 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return [0; 4];
        }
        let at = ((y as u32 * self.width + x as u32) * 4) as usize;
        self.words[at..at + 4].try_into().unwrap_or([0; 4])
    }
    pub fn validate(&self) -> Result<(), String> {
        if u64::from(self.width) * u64::from(self.height) * 4 != self.words.len() as u64 {
            return Err("16-bit image dimensions do not match its samples".into());
        }
        Ok(())
    }
}
pub fn display_pixel(pixel: Pixel16) -> [u8; 4] {
    let mut bytes = pixel.map(crate::raster::project16);
    if bytes[3] == 0 {
        bytes = [0; 4];
    }
    bytes
}
pub fn validate_budget(doc: &Document) -> Result<(), String> {
    check_size(doc.width, doc.height)?;
    let mut colors = 0u64;
    let mut masks = 0u64;
    for layer in &doc.layers {
        masks = masks
            .checked_add(mask::buffer_bytes(layer))
            .ok_or("16-bit mask budget overflow")?;
        if layer.effects.iter().any(|e| e.enabled) {
            let (width, height) = if ["group", "adjustment"].contains(&layer.kind.as_str()) {
                (doc.width, doc.height)
            } else {
                (layer.pixels.width, layer.pixels.height)
            };
            colors = colors
                .checked_add(u64::from(width) * u64::from(height) * 8)
                .ok_or("16-bit color budget overflow")?;
            for effect in layer.effects.iter().filter(|e| e.enabled) {
                let settings = effects::normalized(&effect.kind, &effect.settings)?;
                if color::working_bytes(width, height, &effect.kind, &settings)? > WORKING_BUDGET {
                    return Err("16-bit effect temporary buffers exceed the bounded budget".into());
                }
            }
        }
    }
    if colors > COLOR_BUDGET {
        return Err("Native16 derived color images exceed the128 MiB effect budget".into());
    }
    if masks > MASK_BUDGET {
        return Err("Native16 spatial masks exceed the128 MiB mask budget".into());
    }
    Ok(())
}
pub fn prepare_masks(doc: &Document) -> Result<Vec<Option<Arc<Gray16>>>, String> {
    validate_budget(doc)?;
    doc.layers.iter().map(mask::prepare).collect()
}
pub fn mask_value_prepared(
    layer: &crate::engine::Layer,
    x: i32,
    y: i32,
    prepared: Option<&Gray16>,
    raw: bool,
) -> f64 {
    mask::value(layer, x, y, prepared, raw)
}
pub fn mask_image(doc: &Document, index: usize) -> Result<Option<Arc<Image16>>, String> {
    validate_budget(doc)?;
    mask::image(doc.layers.get(index).ok_or("Unknown16-bit mask layer")?)
}
#[derive(Default)]
struct ColorCache {
    items: HashMap<String, Arc<Image16>>,
    order: VecDeque<String>,
    bytes: usize,
}
static COLORS: OnceLock<Mutex<ColorCache>> = OnceLock::new();
pub fn prepare(doc: &Document) -> Result<Vec<Option<Arc<Image16>>>, String> {
    let masks = prepare_masks(doc)?;
    prepare_with_masks(doc, &masks)
}
pub(crate) fn prepare_with_masks(
    doc: &Document,
    masks: &[Option<Arc<Gray16>>],
) -> Result<Vec<Option<Arc<Image16>>>, String> {
    validate_budget(doc)?;
    let mut out = vec![None; doc.layers.len()];
    let depth = |index: usize| {
        let mut value = 0;
        let mut parent = doc.layers[index].parent.as_deref();
        while let Some(id) = parent {
            value += 1;
            parent = doc
                .layers
                .iter()
                .find(|l| l.id == id)
                .and_then(|l| l.parent.as_deref());
            if value > 16 {
                break;
            }
        }
        value
    };
    let mut order: Vec<_> = (0..doc.layers.len()).collect();
    order.sort_by_key(|&i| (std::cmp::Reverse(depth(i)), std::cmp::Reverse(i)));
    let cache = COLORS.get_or_init(|| Mutex::new(ColorCache::default()));
    for index in order {
        let layer = &doc.layers[index];
        if !layer.effects.iter().any(|e| e.enabled) {
            continue;
        }
        let (width, height) = if ["group", "adjustment"].contains(&layer.kind.as_str()) {
            (doc.width, doc.height)
        } else {
            (layer.pixels.width, layer.pixels.height)
        };
        // Engine invalidation regenerates effect keys for source, ancestor and
        // adjustment changes, including drafts whose revision has not committed.
        let key = format!(
            "{}:{}:16:{}:{width}:{height}",
            doc.id, doc.revision, layer.effect_key
        );
        {
            let mut cache = cache.lock().map_err(|_| "16-bit color cache unavailable")?;
            if let Some(image) = cache.items.get(&key).cloned() {
                cache.order.retain(|k| k != &key);
                cache.order.push_back(key.clone());
                out[index] = Some(image);
                continue;
            }
        }
        let mut image = match layer.kind.as_str() {
            "fill" => {
                let mut image = Image16::new(width, height)?;
                for pixel in image.words.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&layer.color.map(|v| u16::from(v) * 257));
                }
                image
            }
            "group" | "adjustment" => {
                let plan = Plan16::with_prepared(doc, masks, &out);
                check_size(width, height)?;
                let group = plan.group(Some(&layer.id));
                Image16 {
                    width,
                    height,
                    words: crate::render::rgba16(width, height, |x, y| {
                        if layer.kind == "group" {
                            plan.sample(group, x as i32, y as i32)
                        } else {
                            plan.adjustment_input(index, x as i32, y as i32)
                        }
                    }),
                }
            }
            _ => Image16 {
                width,
                height,
                words: layer.pixels.rgba16(),
            },
        };
        for effect in layer.effects.iter().filter(|e| e.enabled) {
            color::apply(&mut image, &effect.kind, &effect.settings)?;
        }
        let image = Arc::new(image);
        let bytes = image.words.len() * 2;
        let mut cache = cache.lock().map_err(|_| "16-bit color cache unavailable")?;
        if let Some(existing) = cache.items.get(&key) {
            out[index] = Some(existing.clone());
            continue;
        }
        while cache.bytes + bytes > COLOR_BUDGET as usize {
            let Some(old) = cache.order.pop_front() else {
                break;
            };
            if let Some(image) = cache.items.remove(&old) {
                cache.bytes -= image.words.len() * 2;
            }
        }
        cache.bytes += bytes;
        cache.order.push_back(key.clone());
        cache.items.insert(key, image.clone());
        out[index] = Some(image);
    }
    Ok(out)
}
pub fn render_crop(doc: &Document, rect: [i32; 4]) -> Result<Image16, String> {
    let width = u32::try_from(i64::from(rect[2]) - i64::from(rect[0]))
        .map_err(|_| "Invalid16-bit render rectangle")?;
    let height = u32::try_from(i64::from(rect[3]) - i64::from(rect[1]))
        .map_err(|_| "Invalid16-bit render rectangle")?;
    check_size(width, height)?;
    let plan = Plan16::new(doc)?;
    Ok(Image16 {
        width,
        height,
        words: crate::render::rgba16(width, height, |x, y| {
            plan.pixel(rect[0] + x as i32, rect[1] + y as i32)
        }),
    })
}
pub fn render(doc: &Document) -> Result<Image16, String> {
    render_crop(doc, [0, 0, doc.width as i32, doc.height as i32])
}
pub fn full_composite(doc: &Document) -> Result<Image16, String> {
    render(doc)
}
pub fn layer_image(doc: &Document, index: usize) -> Result<Image16, String> {
    let layer = doc.layers.get(index).ok_or("Unknown16-bit color layer")?;
    let plan = Plan16::new(doc)?;
    let (width, height) = (layer.pixels.width, layer.pixels.height);
    check_size(width, height)?;
    Ok(Image16 {
        width,
        height,
        words: crate::render::rgba16(width, height, |x, y| {
            plan.layer(index, x as i32 + layer.x, y as i32 + layer.y)
        }),
    })
}
pub fn preview16(
    doc: &Document,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
) -> Result<(u32, u32, Vec<u16>, [i32; 4]), String> {
    validate_budget(doc)?;
    let mut rect = rect.unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
    rect[0] = rect[0].clamp(0, doc.width as i32 - 1);
    rect[1] = rect[1].clamp(0, doc.height as i32 - 1);
    rect[2] = rect[2].clamp(rect[0] + 1, doc.width as i32);
    rect[3] = rect[3].clamp(rect[1] + 1, doc.height as i32);
    let rw = (rect[2] - rect[0]) as u32;
    let rh = (rect[3] - rect[1]) as u32;
    let scale = (edge.clamp(1, 8192) as f64 / rw.max(rh) as f64).min(1.0);
    let width = (rw as f64 * scale).round().max(1.0) as u32;
    let height = (rh as f64 * scale).round().max(1.0) as u32;
    let index = target
        .map(|id| {
            doc.layers
                .iter()
                .position(|l| l.id == id)
                .ok_or("Layer no longer exists")
        })
        .transpose()?;
    let plan = Plan16::new(doc)?;
    check_size(width, height)?;
    let words = crate::render::rgba16(width, height, |x, y| {
        let sx = rect[0] + (x as f64 / scale) as i32;
        let sy = rect[1] + (y as f64 / scale) as i32;
        let pixel = if let Some(index) = index {
            if mask {
                let v = (plan.mask(index, sx, sy, true) * 65535.0).round() as u16;
                [v, v, v, 65535]
            } else {
                plan.layer(index, sx, sy)
            }
        } else {
            plan.pixel(sx, sy)
        };
        pixel
    });
    Ok((width, height, words, rect))
}
pub fn preview(
    doc: &Document,
    rect: Option<[i32; 4]>,
    edge: u32,
    target: Option<&str>,
    mask: bool,
) -> Result<(u32, u32, Vec<u8>, [i32; 4]), String> {
    let (width, height, words, rect) = preview16(doc, rect, edge, target, mask)?;
    let bytes = crate::render::rgba8(width, height, |x, y| {
        let at = ((y * width + x) * 4) as usize;
        display_pixel(words[at..at + 4].try_into().unwrap())
    });
    Ok((width, height, bytes, rect))
}
