//! Bounded, derived preview tiles. Never used for authoritative save/export pixels.
use super::{scoped, Backend, BACKEND};
use crate::{
    effects::Image,
    engine::Document,
    raster::{Raster, TILE},
};
use eframe::wgpu::{self, util::DeviceExt};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::{mpsc, Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};

const TILE_BYTES: usize = (TILE * TILE * 4) as usize;
const SLOTS: usize = 256;
pub const ATLAS_BYTES: u64 = (SLOTS * TILE_BYTES) as u64;
const OUTPUT_BUDGET: usize = 32 * 1024 * 1024;
const METADATA_BUDGET: usize = 32 * 1024 * 1024;
static COMPOSITOR: OnceLock<Mutex<Option<Compositor>>> = OnceLock::new();
static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();

#[derive(Clone, Default, Debug, Serialize)]
pub struct Status {
    pub available: bool,
    pub atlas_budget_bytes: u64,
    pub resident_tiles: usize,
    pub cache_hits: u64,
    pub uploaded_bytes: u64,
    pub successful_frames: u64,
    pub repaired_samples: u64,
    pub last_ms: Option<f64>,
    pub fallback_reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{Layer, Mask};
    fn fixture(size: u32) -> Document {
        let mut doc = Document::new(size, size - 31).unwrap();
        for index in 0..3 {
            let mut layer = Layer::new("GPU parity", "paint", size, size - 31);
            layer.opacity = [0.731, 1.0, 0.417][index];
            layer.x = index as i32 * 19 - 27;
            layer.y = 11 - index as i32 * 17;
            for ty in 0..layer.pixels.height.div_ceil(TILE) {
                for tx in 0..size.div_ceil(TILE) {
                    let mut bytes = vec![0; (TILE * TILE * 4) as usize];
                    for (i, p) in bytes.chunks_exact_mut(4).enumerate() {
                        let x = tx * TILE + i as u32 % TILE;
                        let y = ty * TILE + i as u32 / TILE;
                        p.copy_from_slice(&[
                            (x.wrapping_mul(37) + y * 11 + index as u32 * 53) as u8,
                            (x * 13 + y * 23) as u8,
                            (x * 7 + y * 43) as u8,
                            ((x * 3 + y * 5 + index as u32 * 31) % 256) as u8,
                        ]);
                    }
                    layer.pixels.tiles.insert((tx, ty), Arc::new(bytes));
                }
            }
            doc.layers.insert(0, layer);
        }
        doc.layers.pop();
        let mut group = Layer::new("Isolated folder", "group", size, size - 31);
        group.opacity = 0.613;
        doc.layers[0].parent = Some(group.id.clone());
        doc.layers[1].parent = Some(group.id.clone());
        doc.layers.insert(0, group);
        doc
    }
    fn reference(doc: &Document, colors: &[Option<Arc<Image>>], xs: &[i32], ys: &[i32]) -> Vec<u8> {
        let masks: Vec<_> = doc
            .layers
            .iter()
            .map(|l| {
                l.mask
                    .as_ref()
                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
            })
            .collect();
        let plan = crate::compositor::Plan::new(doc, &masks, colors);
        ys.iter()
            .flat_map(|y| xs.iter().flat_map(|x| plan.sample(0, *x, *y)))
            .collect()
    }
    fn parity(
        compositor: &mut Compositor,
        doc: &Document,
        colors: &[Option<Arc<Image>>],
        xs: &[i32],
        ys: &[i32],
    ) {
        let actual = compositor.render(doc, colors, xs, ys).unwrap();
        let expected = reference(doc, colors, xs, ys);
        let max = actual
            .iter()
            .zip(&expected)
            .map(|(a, b)| a.abs_diff(*b))
            .max()
            .unwrap_or(0);
        let worst = actual
            .iter()
            .zip(&expected)
            .position(|(a, b)| a.abs_diff(*b) == max)
            .unwrap_or(0);
        assert!(
            max <= 1,
            "preview channel error {max} at {:?}: {:?} vs {:?}; blends {:?}",
            [xs[(worst / 4) % xs.len()], ys[(worst / 4) / xs.len()]],
            &actual[(worst / 4) * 4..(worst / 4) * 4 + 4],
            &expected[(worst / 4) * 4..(worst / 4) * 4 + 4],
            doc.layers
                .iter()
                .map(|l| l.blend.as_str())
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn scene_preserves_groups_masks_blends_and_rejects_precision_mismatch() {
        let mut doc = fixture(271);
        let colors = vec![None; doc.layers.len()];
        let scene = Scene::new(&doc, &colors).unwrap();
        assert!(scene.source_bytes() <= ATLAS_BYTES as usize);
        assert_eq!(
            scene.nodes.iter().map(|n| n[0]).collect::<Vec<_>>(),
            vec![0, 2, 0, 0, 3]
        );
        doc.layers[1].mask = Some(Mask {
            enabled: true,
            steps: vec![],
            cache_key: "mask".into(),
        });
        assert!(Scene::new(&doc, &colors).is_ok());
        doc.layers[1].mask.as_mut().unwrap().enabled = false;
        assert!(Scene::new(&doc, &colors).is_ok());
        doc.layers[1].blend = "multiply".into();
        assert!(Scene::new(&doc, &colors).is_ok());
        doc.layers[1].blend = "unknown".into();
        assert!(Scene::new(&doc, &colors).is_err());
        doc.layers[1].blend = "normal".into();
        doc.bit_depth = 16;
        assert!(Scene::new(&doc, &colors).is_err());
    }
    #[test]
    fn source_budget_and_native_slot_allocation_use_the_original_depth() {
        let mut doc = fixture(1033);
        let colors = vec![None; doc.layers.len()];
        let bytes = Scene::new(&doc, &colors).unwrap().source_bytes();
        doc.bit_depth = 16;
        for l in &mut doc.layers {
            l.pixels.promote16();
        }
        let words = vec![None; doc.layers.len()];
        assert_eq!(
            Scene::build(&doc, Colors::Words(&words))
                .unwrap()
                .source_bytes(),
            bytes * 2
        );
        assert!(
            Scene::new(&fixture(3073), &vec![None; 4])
                .unwrap()
                .source_bytes()
                > ATLAS_BYTES as usize
        );
    }
    #[test]
    #[ignore = "requires a GPU device"]
    fn gpu_all_blends_native16_masks_clipping_pass_through_and_adjustments_match_cpu() {
        use crate::{effects::Effect, engine::MaskStep};
        let backend = crate::gpu::headless().unwrap();
        let mut gpu = Compositor::new(&backend).unwrap();
        let xs: Vec<_> = (-7..119).collect();
        let ys: Vec<_> = (-11..99).collect();
        for depth in [8, 16] {
            for pass in [false, true] {
                for &(mode, _) in crate::raster::BLENDS {
                    let mut doc = fixture(113);
                    doc.bit_depth = depth;
                    for l in &mut doc.layers {
                        l.pixels.convert_depth(depth);
                        if depth == 16 {
                            for tile in l.pixels.samples16.values_mut() {
                                for (i, v) in Arc::make_mut(tile).iter_mut().enumerate() {
                                    *v = v.saturating_add((i % 127) as u16);
                                }
                            }
                        }
                    }
                    doc.layers[0].blend = if pass { "pass_through" } else { mode }.into();
                    doc.layers[1].blend = mode.into();
                    doc.layers[2].blend = mode.into();
                    doc.layers[1].clip_to = Some(doc.layers[2].id.clone());
                    for i in [0, 1, 2] {
                        let l = &mut doc.layers[i];
                        let mut paint = Raster::new_depth(l.pixels.width, l.pixels.height, depth);
                        paint.set16(29, 31, [17773, 17773, 17773, 48261]);
                        l.mask = Some(Mask {
                            enabled: true,
                            cache_key: crate::engine::id(),
                            steps: vec![
                                MaskStep {
                                    id: crate::engine::id(),
                                    kind: "fill".into(),
                                    enabled: true,
                                    weight: 0.733,
                                    value: 177.1,
                                    pixels: Raster::new_depth(
                                        l.pixels.width,
                                        l.pixels.height,
                                        depth,
                                    ),
                                    settings: serde_json::json!({}),
                                },
                                MaskStep {
                                    id: crate::engine::id(),
                                    kind: "paint".into(),
                                    enabled: true,
                                    weight: 0.611,
                                    value: 0.0,
                                    pixels: paint,
                                    settings: serde_json::json!({}),
                                },
                            ],
                        });
                    }
                    let mut adjustment =
                        Layer::new("Native adjustment", "adjustment", doc.width, doc.height);
                    adjustment.pixels.convert_depth(depth);
                    adjustment.blend = mode.into();
                    adjustment.opacity = 0.571;
                    adjustment.effects.push(Effect {
                        id: crate::engine::id(),
                        kind: "invert".into(),
                        enabled: true,
                        weight: 0.417,
                        settings: serde_json::json!({}),
                    });
                    doc.layers.insert(0, adjustment);
                    let before = serde_json::to_vec(&doc).unwrap();
                    if depth == 8 {
                        let masks: Vec<_> = doc
                            .layers
                            .iter()
                            .map(|l| {
                                l.mask
                                    .as_ref()
                                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
                            })
                            .collect();
                        let colors = crate::effects::prepare(&doc, &masks).unwrap();
                        parity(&mut gpu, &doc, &colors, &xs, &ys);
                    } else {
                        let masks = crate::depth16::prepare_masks(&doc).unwrap();
                        let colors = crate::depth16::prepare_with_masks(&doc, &masks).unwrap();
                        let plan = crate::depth16::Plan16::with_prepared(&doc, &masks, &colors);
                        let plan = &plan;
                        let expected: Vec<_> = ys
                            .iter()
                            .flat_map(|&y| xs.iter().flat_map(move |&x| plan.pixel(x, y)))
                            .collect();
                        let actual = gpu.render16(&doc, &colors, &xs, &ys).unwrap();
                        let error = actual
                            .iter()
                            .zip(&expected)
                            .map(|(a, b)| a.abs_diff(*b))
                            .max()
                            .unwrap();
                        assert!(
                            error <= 8,
                            "native preview error {error}: {mode}, pass {pass}"
                        );
                        let projected = actual
                            .iter()
                            .zip(&expected)
                            .map(|(a, b)| {
                                crate::raster::project16(*a).abs_diff(crate::raster::project16(*b))
                            })
                            .max()
                            .unwrap();
                        assert!(
                            projected <= 1,
                            "display preview error {projected}: {mode}, pass {pass}"
                        );
                        assert!(actual.iter().any(|v| v % 257 != 0));
                    }
                    assert_eq!(serde_json::to_vec(&doc).unwrap(), before);
                }
            }
        }
    }
    #[test]
    #[ignore = "requires a GPU device"]
    fn gpu_tiles_match_cpu_cow_undo_groups_crops_and_bounded_eviction() {
        let backend = crate::gpu::headless().unwrap();
        let mut compositor = Compositor::new(&backend).unwrap();
        let mut doc = fixture(3073);
        let original = serde_json::to_vec(&doc).unwrap();
        let mut colors = vec![None; doc.layers.len()];
        let xs: Vec<_> = (0..769).map(|x| 13 + (x as f32 / 0.251) as i32).collect();
        let ys: Vec<_> = (0..733).map(|y| 7 + (y as f32 / 0.251) as i32).collect();
        parity(&mut compositor, &doc, &colors, &xs, &ys);
        assert!(compositor.uploaded > ATLAS_BYTES);
        assert_eq!(compositor.resident_bytes(), ATLAS_BYTES as usize);
        // COW identity changes even with only a weak cache reference. Undo reuses the original tile safely.
        let before = doc.clone();
        doc.layers[1].pixels.set(26, 37, [241, 17, 89, 223]);
        parity(&mut compositor, &doc, &colors, &xs[..117], &ys[..91]);
        parity(&mut compositor, &before, &colors, &xs[..117], &ys[..91]);
        let hits = compositor.hits;
        parity(&mut compositor, &before, &colors, &xs[..117], &ys[..91]);
        assert!(compositor.hits > hits);
        // A baked folder effect uses its image at the folder origin, without replaying children.
        colors[0] = Some(Arc::new(Image {
            width: 271,
            height: 263,
            bytes: (0..271 * 263)
                .flat_map(|i| [i as u8, 71, 229, (i % 256) as u8])
                .collect(),
        }));
        doc.layers[0].x = 29;
        doc.layers[0].y = -9;
        parity(&mut compositor, &doc, &colors, &xs[..117], &ys[..91]);
        doc = before;
        assert_eq!(serde_json::to_vec(&doc).unwrap(), original);
        let native = Document::new_depth(8, 8, 16).unwrap();
        assert!(compositor.render(&native, &[None], &[0], &[0]).is_err());
        let mut empty = Document::new(1, 1).unwrap();
        empty.layers.clear();
        assert_eq!(
            compositor.render(&empty, &[], &[0], &[0]).unwrap(),
            vec![0; 4]
        );
        assert!(compositor
            .render(&doc, &vec![None; doc.layers.len()], &[2, 1], &[0])
            .is_err());
        let mut completed = 0;
        let failure =
            compositor.render_impl(&doc, &vec![None; doc.layers.len()], &xs, &ys, |tile| {
                if tile == 1 {
                    return Err("simulated later dispatch failure".into());
                }
                completed += 1;
                Ok(())
            });
        assert_eq!(failure.unwrap_err(), "simulated later dispatch failure");
        assert_eq!(completed, 1);
        assert_eq!(compositor.resident_bytes(), 0);
        assert_eq!(serde_json::to_vec(&doc).unwrap(), original);
        parity(
            &mut compositor,
            &doc,
            &vec![None; doc.layers.len()],
            &xs[..117],
            &ys[..91],
        );
        let _busy = backend.gate.lock().unwrap();
        assert!(compositor
            .render(&doc, &vec![None; doc.layers.len()], &xs, &ys)
            .unwrap_err()
            .contains("busy"));
        let mut image = Image {
            width: 1,
            height: 1,
            bytes: vec![17, 29, 43, 89],
        };
        assert!(!backend
            .apply(&mut image, "invert", &serde_json::json!({}))
            .unwrap());
        assert_eq!(image.bytes, vec![17, 29, 43, 89]);
    }
}
pub fn status() -> Status {
    STATUS
        .get_or_init(|| {
            Mutex::new(Status {
                atlas_budget_bytes: ATLAS_BYTES,
                ..Status::default()
            })
        })
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

/// Returns a complete region or leaves the CPU caller responsible for it. No partial frame is exposed.
pub(crate) fn try_render(
    doc: &Document,
    colors: &[Option<Arc<Image>>],
    xs: &[i32],
    ys: &[i32],
) -> Option<Vec<u8>> {
    try_native(doc, Colors::Bytes(colors), xs, ys)
        .map(|bytes| bytes.chunks_exact(2).map(|v| v[0]).collect())
}
pub(crate) fn try_render16(
    doc: &Document,
    colors: &[Option<Arc<crate::depth16::Image16>>],
    xs: &[i32],
    ys: &[i32],
) -> Option<Vec<u16>> {
    try_native(doc, Colors::Words(colors), xs, ys).map(|bytes| {
        bytes
            .chunks_exact(2)
            .map(|v| u16::from_ne_bytes([v[0], v[1]]))
            .collect()
    })
}
fn try_native(doc: &Document, colors: Colors<'_>, xs: &[i32], ys: &[i32]) -> Option<Vec<u8>> {
    if xs.len().checked_mul(ys.len())? < 128 * 1024 || doc.layers.len() < 3 {
        return None;
    }
    let backend = BACKEND.get()?;
    let mut slot = COMPOSITOR
        .get_or_init(|| Mutex::new(None))
        .try_lock()
        .ok()?;
    let start = Instant::now();
    let result = (|| {
        if slot.is_none() {
            *slot = Some(Compositor::new(backend)?);
        }
        let scene = Scene::build(doc, colors)?;
        if scene.source_bytes() > ATLAS_BYTES as usize {
            return Err(
                "Source tile working set exceeds the measured GPU cache threshold; using CPU"
                    .into(),
            );
        }
        slot.as_mut()
            .unwrap()
            .render_scene(&scene, xs, ys, |_| Ok(()))
    })();
    let mut status = STATUS
        .get_or_init(|| Mutex::new(Status::default()))
        .lock()
        .ok()?;
    status.atlas_budget_bytes = ATLAS_BYTES;
    if let Some(compositor) = slot.as_ref() {
        status.available = true;
        status.resident_tiles = compositor.cache.len();
        status.cache_hits = compositor.hits;
        status.uploaded_bytes = compositor.uploaded;
        status.repaired_samples = compositor.repaired;
    }
    match result {
        Ok(bytes) => {
            status.successful_frames += 1;
            status.last_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
            status.fallback_reason = None;
            Some(bytes)
        }
        Err(error) => {
            status.fallback_reason = Some(error);
            None
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
enum Key {
    Raw8(usize),
    Raw16(usize),
    Image8(usize, u32, u32),
    Image16(usize, u32, u32),
}
// Weak ownership prevents allocator address reuse without retaining image buffers.
#[allow(dead_code)]
enum Owner {
    Bytes(Weak<Vec<u8>>),
    Words(Weak<Vec<u16>>),
    Image8(Weak<Image>),
    Image16(Weak<crate::depth16::Image16>),
}
struct Entry {
    slot: u32,
    blocks: usize,
    stamp: u64,
    _source: Owner,
}
enum Source<'a> {
    Raster(&'a Raster),
    Image8(&'a Arc<Image>),
    Image16(&'a Arc<crate::depth16::Image16>),
}
impl Source<'_> {
    fn blocks(&self) -> usize {
        match self {
            Self::Raster(r) if r.depth == 16 => 2,
            Self::Image16(_) => 2,
            _ => 1,
        }
    }
    fn dims(&self) -> (u32, u32) {
        match self {
            Self::Raster(r) => (r.width, r.height),
            Self::Image8(i) => (i.width, i.height),
            Self::Image16(i) => (i.width, i.height),
        }
    }
    fn key(&self, x: u32, y: u32) -> Option<Key> {
        match self {
            Self::Raster(r) if r.depth == 16 => r
                .samples16
                .get(&(x, y))
                .map(|p| Key::Raw16(Arc::as_ptr(p) as usize)),
            Self::Raster(r) => r
                .tiles
                .get(&(x, y))
                .map(|p| Key::Raw8(Arc::as_ptr(p) as usize)),
            Self::Image8(i) => Some(Key::Image8(Arc::as_ptr(i) as usize, x, y)),
            Self::Image16(i) => Some(Key::Image16(Arc::as_ptr(i) as usize, x, y)),
        }
    }
    fn tile(&self, x: u32, y: u32) -> Result<(Vec<u8>, Owner), String> {
        if let Self::Raster(r) = self {
            if r.depth == 16 {
                let p = r
                    .samples16
                    .get(&(x, y))
                    .ok_or("Missing native preview tile")?;
                return Ok((
                    bytemuck::cast_slice(p.as_slice()).to_vec(),
                    Owner::Words(Arc::downgrade(p)),
                ));
            }
            let p = r.tiles.get(&(x, y)).ok_or("Missing preview tile")?;
            return Ok((p.as_ref().clone(), Owner::Bytes(Arc::downgrade(p))));
        }
        let mut words = vec![0; (TILE * TILE * 4) as usize];
        let (w, h) = self.dims();
        let valid = match self {
            Self::Image8(i) => (w as u64) * (h as u64) * 4 == i.bytes.len() as u64,
            Self::Image16(i) => (w as u64) * (h as u64) * 4 == i.words.len() as u64,
            _ => true,
        };
        if !valid {
            return Err("Preview image dimensions do not match its samples".into());
        }
        let width = (w - x * TILE).min(TILE) as usize;
        for row in 0..(h - y * TILE).min(TILE) as usize {
            let start = (((y * TILE) as usize + row) * w as usize + (x * TILE) as usize) * 4;
            let dest = &mut words[row * TILE as usize * 4..row * TILE as usize * 4 + width * 4];
            match self {
                Self::Image8(i) => {
                    for (d, s) in dest.iter_mut().zip(&i.bytes[start..start + width * 4]) {
                        *d = u16::from(*s);
                    }
                }
                Self::Image16(i) => dest.copy_from_slice(&i.words[start..start + width * 4]),
                _ => unreachable!(),
            }
        }
        let owner = match self {
            Self::Image8(i) => Owner::Image8(Arc::downgrade(i)),
            Self::Image16(i) => Owner::Image16(Arc::downgrade(i)),
            _ => unreachable!(),
        };
        let bytes = match self {
            Self::Image8(_) => words.into_iter().map(|v| v as u8).collect(),
            _ => bytemuck::cast_slice(&words).to_vec(),
        };
        Ok((bytes, owner))
    }
}
#[derive(Clone, Copy)]
enum Colors<'a> {
    Bytes(&'a [Option<Arc<Image>>]),
    Words(&'a [Option<Arc<crate::depth16::Image16>>]),
}
impl<'a> Colors<'a> {
    fn len(self) -> usize {
        match self {
            Self::Bytes(c) => c.len(),
            Self::Words(c) => c.len(),
        }
    }
    fn source(self, i: usize) -> Option<Source<'a>> {
        match self {
            Self::Bytes(c) => c[i].as_ref().map(Source::Image8),
            Self::Words(c) => c[i].as_ref().map(Source::Image16),
        }
    }
}
enum PreparedMask {
    Bytes(Option<Arc<crate::mask::GrayMask>>),
    Words(Option<Arc<crate::depth16::Gray16>>),
}
struct Scene<'a> {
    doc: &'a Document,
    colors: Colors<'a>,
    // op, dimensions/fill; origin, opacity, lookup; columns, source index,
    // mask index, blend; native fill high word, sample maximum, reserved, action.
    nodes: Vec<[u32; 16]>,
    sources: Vec<Source<'a>>,
    origins: Vec<[i32; 2]>,
    offsets: Vec<usize>,
    masks: Vec<(&'a crate::engine::Layer, PreparedMask)>,
    lookup_len: usize,
}
impl<'a> Scene<'a> {
    fn source_bytes(&self) -> usize {
        self.sources
            .iter()
            .map(|source| {
                let tiles = match source {
                    Source::Raster(r) if r.depth == 16 => r.samples16.len(),
                    Source::Raster(r) => r.tiles.len(),
                    _ => {
                        let (w, h) = source.dims();
                        (w.div_ceil(TILE) * h.div_ceil(TILE)) as usize
                    }
                };
                tiles * TILE_BYTES * source.blocks()
            })
            .sum()
    }
    fn new(doc: &'a Document, colors: &'a [Option<Arc<Image>>]) -> Result<Self, String> {
        Self::build(doc, Colors::Bytes(colors))
    }
    fn build(doc: &'a Document, colors: Colors<'a>) -> Result<Self, String> {
        if cfg!(target_endian = "big")
            || colors.len() != doc.layers.len()
            || !matches!(
                (doc.bit_depth, colors),
                (8, Colors::Bytes(_)) | (16, Colors::Words(_))
            )
        {
            return Err("Preview source precision does not match the document".into());
        }
        if doc.bit_depth == 16 {
            crate::depth16::validate_budget(doc)?;
        } else {
            crate::mask::validate_budget(&doc.layers)?;
        }
        let mut scene = Self {
            doc,
            colors,
            nodes: vec![],
            sources: vec![],
            origins: vec![],
            offsets: vec![],
            masks: vec![],
            lookup_len: 0,
        };
        scene.children(doc, colors, None, 0)?;
        if scene.nodes.is_empty() {
            let mut node = [0; 16];
            node[0] = 1;
            node[10] = u32::MAX;
            node[13] = if doc.bit_depth == 16 { 65535 } else { 255 };
            scene.nodes.push(node);
        }
        Ok(scene)
    }
    fn children(
        &mut self,
        doc: &'a Document,
        colors: Colors<'a>,
        parent: Option<&str>,
        depth: usize,
    ) -> Result<(), String> {
        for (i, layer) in doc.layers.iter().enumerate().rev() {
            if layer.parent.as_deref() != parent || !layer.visible || layer.clip_to.is_some() {
                continue;
            }
            let clips: Vec<_> = (0..i)
                .rev()
                .filter(|&c| {
                    let clip = &doc.layers[c];
                    clip.visible
                        && clip.parent == layer.parent
                        && clip.clip_to.as_deref() == Some(layer.id.as_str())
                })
                .collect();
            if clips.is_empty() || layer.kind == "adjustment" || layer.blend == "pass_through" {
                self.layer(doc, colors, i, 0, false, depth)?;
            } else {
                self.push(
                    [4, 0, 0, 0, 0, 0, 0, 0, 0, 0, u32::MAX, 0, 0, 0, 0, 0],
                    depth,
                )?;
                self.layer(doc, colors, i, 0, true, depth + 1)?;
                for clip in clips {
                    self.layer(doc, colors, clip, 1, false, depth + 1)?;
                }
                let mut end = self.node(doc, i, false)?;
                end[0] = 5;
                self.push(end, depth + 1)?;
            }
        }
        Ok(())
    }
    fn node(&mut self, doc: &'a Document, i: usize, raw: bool) -> Result<[u32; 16], String> {
        let l = &doc.layers[i];
        if l.pixels.depth != doc.bit_depth {
            return Err("Preview layer precision mismatch".into());
        }
        let mut n = [0; 16];
        n[1] = l.pixels.width;
        n[2] = l.pixels.height;
        n[4] = l.x as u32;
        n[5] = l.y as u32;
        n[6] = if raw { 1.0f32 } else { l.opacity }.to_bits();
        n[10] = u32::MAX;
        n[11] = if raw || l.blend == "pass_through" {
            0
        } else {
            crate::raster::BLENDS
                .iter()
                .position(|(k, _)| *k == l.blend)
                .ok_or("Unsupported preview blend")? as u32
        };
        n[13] = if doc.bit_depth == 16 { 65535 } else { 255 };
        if !raw && l.mask.as_ref().is_some_and(|m| m.enabled) {
            n[10] = self.masks.len() as u32;
            let prepared = if doc.bit_depth == 16 {
                PreparedMask::Words(crate::depth16::mask::prepare(l)?)
            } else {
                PreparedMask::Bytes(
                    l.mask
                        .as_ref()
                        .and_then(|m| m.prepare(l.pixels.width, l.pixels.height)),
                )
            };
            self.masks.push((l, prepared));
        }
        Ok(n)
    }
    fn layer(
        &mut self,
        doc: &'a Document,
        colors: Colors<'a>,
        i: usize,
        action: u32,
        raw: bool,
        depth: usize,
    ) -> Result<(), String> {
        let l = &doc.layers[i];
        let mut n = self.node(doc, i, raw)?;
        n[15] = action;
        let pass = l.kind == "group" && l.blend == "pass_through" && !raw && action == 0;
        let source = if pass { None } else { colors.source(i) };
        if l.kind == "group" && source.is_none() {
            let mut begin = n;
            begin[0] = 2;
            begin[15] = if pass { 4 } else { 0 };
            begin[10] = u32::MAX;
            self.push(begin, depth)?;
            self.children(doc, colors, Some(&l.id), depth + 1)?;
            n[0] = 3;
            if pass {
                n[15] = 4;
            }
        } else if let Some(source) = source {
            let (w, h) = source.dims();
            n[1] = w;
            n[2] = h;
            if l.kind == "group" || l.kind == "adjustment" {
                n[4] = 0;
                n[5] = 0;
            }
            self.source(&mut n, source)?;
            if l.kind == "adjustment" {
                n[15] = if action == 1 { 3 } else { 2 };
            }
        } else if l.kind == "adjustment" {
            return Ok(());
        } else if l.kind == "fill" {
            n[0] = 1;
            let c = l
                .color
                .map(|v| u16::from(v) * if doc.bit_depth == 16 { 257 } else { 1 });
            n[3] = u32::from(c[0]) | (u32::from(c[1]) << 16);
            n[12] = u32::from(c[2]) | (u32::from(c[3]) << 16);
        } else {
            self.source(&mut n, Source::Raster(&l.pixels))?;
        }
        self.push(n, depth)
    }
    fn push(&mut self, node: [u32; 16], depth: usize) -> Result<(), String> {
        if depth > 32 || (matches!(node[0], 2 | 4) && depth >= 32) || self.nodes.len() >= 800 {
            return Err("Preview scene exceeds the bounded topology budget".into());
        }
        self.nodes.push(node);
        Ok(())
    }
    fn source(&mut self, node: &mut [u32; 16], source: Source<'a>) -> Result<(), String> {
        let (w, h) = source.dims();
        crate::raster::check_size(w, h)?;
        node[7] = self.lookup_len as u32;
        node[8] = w.div_ceil(TILE);
        node[9] = self.sources.len() as u32;
        self.offsets.push(self.lookup_len);
        self.origins.push([node[4] as i32, node[5] as i32]);
        self.lookup_len += (w.div_ceil(TILE) * h.div_ceil(TILE)) as usize;
        self.sources.push(source);
        Ok(())
    }
}

pub struct Compositor {
    gate: Arc<Mutex<()>>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipeline: wgpu::ComputePipeline,
    atlas: wgpu::Buffer,
    cache: HashMap<Key, Entry>,
    stamp: u64,
    pub hits: u64,
    pub uploaded: u64,
    pub repaired: u64,
}
impl Compositor {
    pub fn new(backend: &Backend) -> Result<Self, String> {
        let _gate = backend.gate.try_lock().map_err(|_| "GPU busy; using CPU")?;
        let device = &backend.device;
        if u64::from(device.limits().max_storage_buffer_binding_size) < ATLAS_BYTES {
            return Err("GPU storage limits require CPU compositing".into());
        }
        scoped(device, || {
            let entries: Vec<_> = (0..5)
                .map(|binding| wgpu::BindGroupLayoutEntry {
                    binding,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    count: None,
                    ty: wgpu::BindingType::Buffer {
                        ty: if binding == 3 {
                            wgpu::BufferBindingType::Uniform
                        } else {
                            wgpu::BufferBindingType::Storage {
                                read_only: binding != 2,
                            }
                        },
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                })
                .collect();
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("PeerBrush tile compositor"),
                entries: &entries,
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: None,
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("PeerBrush native-depth tile compositor"),
                source: wgpu::ShaderSource::Wgsl(include_str!("../shaders/composite.wgsl").into()),
            });
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: None,
                layout: Some(&pipeline_layout),
                module: &shader,
                entry_point: Some("composite"),
                compilation_options: Default::default(),
                cache: None,
            });
            let atlas = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("PeerBrush bounded preview atlas"),
                size: ATLAS_BYTES,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Ok(Self {
                gate: backend.gate.clone(),
                device: device.clone(),
                queue: backend.queue.clone(),
                layout,
                pipeline,
                atlas,
                cache: HashMap::new(),
                stamp: 0,
                hits: 0,
                uploaded: 0,
                repaired: 0,
            })
        })
    }
    pub fn resident_bytes(&self) -> usize {
        self.cache.values().map(|e| e.blocks * TILE_BYTES).sum()
    }
    fn tile(
        &mut self,
        source: &Source<'_>,
        x: u32,
        y: u32,
        protected: &HashSet<Key>,
    ) -> Result<u32, String> {
        let Some(key) = source.key(x, y) else {
            return Ok(u32::MAX);
        };
        self.stamp += 1;
        if let Some(entry) = self.cache.get_mut(&key) {
            entry.stamp = self.stamp;
            self.hits += 1;
            return Ok(entry.slot);
        }
        let blocks = source.blocks();
        let slot = loop {
            let mut used = [false; SLOTS];
            for e in self.cache.values() {
                used[e.slot as usize..e.slot as usize + e.blocks].fill(true);
            }
            if let Some(slot) = used.windows(blocks).position(|v| v.iter().all(|b| !*b)) {
                break slot as u32;
            }
            let old = self
                .cache
                .iter()
                .filter(|(key, _)| !protected.contains(*key))
                .min_by_key(|(_, e)| e.stamp)
                .map(|(key, _)| key.clone())
                .ok_or("Preview tile working set exceeds the GPU budget")?;
            self.cache.remove(&old);
        };
        let (bytes, owner) = source.tile(x, y)?;
        if bytes.len() != TILE_BYTES * blocks {
            return Err("Invalid preview tile length".into());
        }
        self.queue
            .write_buffer(&self.atlas, u64::from(slot) * TILE_BYTES as u64, &bytes);
        self.uploaded += bytes.len() as u64;
        self.cache.insert(
            key,
            Entry {
                slot,
                blocks,
                stamp: self.stamp,
                _source: owner,
            },
        );
        Ok(slot)
    }
    /// Coordinates come from the shared CPU sampling grid, including fractional zoom and crops.
    pub fn render(
        &mut self,
        doc: &Document,
        colors: &[Option<Arc<Image>>],
        xs: &[i32],
        ys: &[i32],
    ) -> Result<Vec<u8>, String> {
        self.render_impl(doc, colors, xs, ys, |_| Ok(()))
    }
    fn render_impl(
        &mut self,
        doc: &Document,
        colors: &[Option<Arc<Image>>],
        xs: &[i32],
        ys: &[i32],
        before_tile: impl FnMut(usize) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        let scene = Scene::new(doc, colors)?;
        let bytes = self.render_scene(&scene, xs, ys, before_tile)?;
        Ok(bytes.chunks_exact(2).map(|v| v[0]).collect())
    }
    /// Derived native-depth preview samples; authoritative renders never call this.
    pub fn render16(
        &mut self,
        doc: &Document,
        colors: &[Option<Arc<crate::depth16::Image16>>],
        xs: &[i32],
        ys: &[i32],
    ) -> Result<Vec<u16>, String> {
        let scene = Scene::build(doc, Colors::Words(colors))?;
        let bytes = self.render_scene(&scene, xs, ys, |_| Ok(()))?;
        Ok(bytes
            .chunks_exact(2)
            .map(|v| u16::from_ne_bytes([v[0], v[1]]))
            .collect())
    }
    fn render_scene(
        &mut self,
        scene: &Scene<'_>,
        xs: &[i32],
        ys: &[i32],
        before_tile: impl FnMut(usize) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        let gate = self.gate.clone();
        let _gate = gate.try_lock().map_err(|_| "GPU busy; using CPU")?;
        let len = xs
            .len()
            .checked_mul(ys.len())
            .and_then(|n| n.checked_mul(12))
            .filter(|n| *n > 0 && *n <= OUTPUT_BUDGET)
            .ok_or("Preview output exceeds the GPU budget")?;
        let tiles = xs.len().div_ceil(TILE as usize) * ys.len().div_ceil(TILE as usize);
        if (scene.lookup_len + 512) * 4 * tiles + scene.masks.len() * len / 3 > METADATA_BUDGET {
            return Err("Preview metadata exceeds the GPU budget".into());
        }
        if xs.windows(2).any(|v| v[0] > v[1]) || ys.windows(2).any(|v| v[0] > v[1]) {
            return Err("Preview sampling grid is not ordered".into());
        }
        let result = scoped(&self.device.clone(), || {
            self.dispatch(scene, xs, ys, len, before_tile)
        });
        // A failed queue upload must not leave apparently valid resident entries.
        if result.is_err() {
            self.cache.clear();
        }
        let bytes = result?;
        self.repaired += bytes
            .chunks_exact(12)
            .filter(|p| p[8..12] != [0; 4])
            .count() as u64;
        Self::resolve_samples(scene, xs, ys, bytes)
    }
    fn resolve_samples(
        scene: &Scene<'_>,
        xs: &[i32],
        ys: &[i32],
        bytes: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        if !bytes.chunks_exact(12).any(|p| p[8..12] != [0; 4]) {
            return Ok(bytes
                .chunks_exact(12)
                .flat_map(|p| p[..8].iter().copied())
                .collect());
        }
        let gpu_sample = |i: usize| -> [u16; 4] {
            std::array::from_fn(|c| {
                u16::from_ne_bytes([bytes[i * 12 + c * 2], bytes[i * 12 + c * 2 + 1]])
            })
        };
        let words = match scene.colors {
            Colors::Bytes(colors) => {
                let masks: Vec<_> = scene
                    .doc
                    .layers
                    .iter()
                    .map(|l| {
                        l.mask
                            .as_ref()
                            .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
                    })
                    .collect();
                let plan = crate::compositor::Plan::new(scene.doc, &masks, colors);
                crate::render::rgba16(xs.len() as u32, ys.len() as u32, |x, y| {
                    let i = y as usize * xs.len() + x as usize;
                    if bytes[i * 12 + 8..i * 12 + 12] == [0; 4] {
                        gpu_sample(i)
                    } else {
                        plan.sample(0, xs[x as usize], ys[y as usize])
                            .map(u16::from)
                    }
                })
            }
            Colors::Words(colors) => {
                let masks = crate::depth16::prepare_masks(scene.doc)?;
                let plan = crate::depth16::Plan16::with_prepared(scene.doc, &masks, colors);
                crate::render::rgba16(xs.len() as u32, ys.len() as u32, |x, y| {
                    let i = y as usize * xs.len() + x as usize;
                    if bytes[i * 12 + 8..i * 12 + 12] == [0; 4] {
                        gpu_sample(i)
                    } else {
                        plan.pixel(xs[x as usize], ys[y as usize])
                    }
                })
            }
        };
        Ok(bytemuck::cast_slice(&words).to_vec())
    }

    fn dispatch(
        &mut self,
        scene: &Scene<'_>,
        xs: &[i32],
        ys: &[i32],
        len: usize,
        mut before_tile: impl FnMut(usize) -> Result<(), String>,
    ) -> Result<Vec<u8>, String> {
        let words: Vec<u32> = scene.nodes.iter().flat_map(|n| n.iter().copied()).collect();
        let nodes = self
            .device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(&words),
                usage: wgpu::BufferUsages::STORAGE,
            });
        let output = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: len as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: len as u64,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let mut staged = 0;
        for (ty, ycoords) in ys.chunks(TILE as usize).enumerate() {
            for (tx, xcoords) in xs.chunks(TILE as usize).enumerate() {
                before_tile(ty * xs.len().div_ceil(TILE as usize) + tx)?;
                let mut lookup = vec![u32::MAX; scene.lookup_len];
                let mut needed = Vec::new();
                let mut protected = HashSet::new();
                for (source_id, source) in scene.sources.iter().enumerate() {
                    let origin = scene.origins[source_id];
                    let (w, h) = source.dims();
                    let x0 = (i64::from(xcoords[0]) - i64::from(origin[0])).clamp(0, i64::from(w));
                    let x1 = (i64::from(*xcoords.last().unwrap()) - i64::from(origin[0]) + 1)
                        .clamp(0, i64::from(w));
                    let y0 = (i64::from(ycoords[0]) - i64::from(origin[1])).clamp(0, i64::from(h));
                    let y1 = (i64::from(*ycoords.last().unwrap()) - i64::from(origin[1]) + 1)
                        .clamp(0, i64::from(h));
                    if x0 >= x1 || y0 >= y1 {
                        continue;
                    }
                    for y in y0 as u32 / TILE..(y1 as u32).div_ceil(TILE) {
                        for x in x0 as u32 / TILE..(x1 as u32).div_ceil(TILE) {
                            if let Some(key) = source.key(x, y) {
                                protected.insert(key);
                                needed.push((source_id, x, y));
                            }
                        }
                    }
                }
                if protected.len() * if scene.doc.bit_depth == 16 { 2 } else { 1 } > SLOTS {
                    return Err("Preview tile working set exceeds the GPU budget".into());
                }
                let before = self.uploaded;
                for (source_id, x, y) in needed {
                    let slot = self.tile(&scene.sources[source_id], x, y, &protected)?;
                    let cols = scene.sources[source_id].dims().0.div_ceil(TILE);
                    lookup[scene.offsets[source_id] + (y * cols + x) as usize] = slot;
                }
                let coordinate_offset = lookup.len() as u32;
                lookup.extend(xcoords.iter().chain(ycoords).map(|n| *n as u32));
                let mask_offset = lookup.len() as u32;
                for (layer, prepared) in &scene.masks {
                    for &y in ycoords {
                        for &x in xcoords {
                            let value = match prepared {
                                PreparedMask::Bytes(m) => layer.mask_value_prepared(
                                    x - layer.x,
                                    y - layer.y,
                                    m.as_deref(),
                                    false,
                                ),
                                PreparedMask::Words(m) => crate::depth16::mask::value(
                                    layer,
                                    x - layer.x,
                                    y - layer.y,
                                    m.as_deref(),
                                    false,
                                ) as f32,
                            };
                            lookup.push(value.to_bits());
                        }
                    }
                }
                let table = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: bytemuck::cast_slice(&lookup),
                        usage: wgpu::BufferUsages::STORAGE,
                    });
                let params = [
                    xcoords.len() as u32,
                    ycoords.len() as u32,
                    xs.len() as u32,
                    ys.len() as u32,
                    (tx * TILE as usize) as u32,
                    (ty * TILE as usize) as u32,
                    scene.nodes.len() as u32,
                    coordinate_offset,
                    mask_offset,
                    0,
                    0,
                    0,
                ];
                let uniform = self
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: None,
                        contents: bytemuck::cast_slice(&params),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                let buffers = [&self.atlas, &nodes, &output, &uniform, &table];
                let entries: Vec<_> = buffers
                    .iter()
                    .enumerate()
                    .map(|(binding, buffer)| wgpu::BindGroupEntry {
                        binding: binding as u32,
                        resource: buffer.as_entire_binding(),
                    })
                    .collect();
                let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &self.layout,
                    entries: &entries,
                });
                let mut encoder = self
                    .device
                    .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
                {
                    let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                        label: None,
                        timestamp_writes: None,
                    });
                    pass.set_pipeline(&self.pipeline);
                    pass.set_bind_group(0, &bind, &[]);
                    pass.dispatch_workgroups(
                        (xcoords.len() as u32).div_ceil(8),
                        (ycoords.len() as u32).div_ceil(8),
                        1,
                    );
                }
                // Submit before reusing atlas slots; later queue writes cannot overwrite this dispatch's inputs.
                let submission = self.queue.submit(Some(encoder.finish()));
                staged += self.uploaded - before;
                if staged >= 16 * 1024 * 1024 {
                    self.device
                        .poll(wgpu::Maintain::WaitForSubmissionIndex(submission));
                    staged = 0;
                }
            }
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, len as u64);
        let submission = self.queue.submit(Some(encoder.finish()));
        let (sender, receiver) = mpsc::channel();
        readback.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = sender.send(r);
        });
        self.device
            .poll(wgpu::Maintain::WaitForSubmissionIndex(submission));
        let mapped = receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()));
        if let Err(error) = mapped {
            readback.unmap();
            return Err(error);
        }
        let bytes = readback.slice(..).get_mapped_range().to_vec();
        readback.unmap();
        Ok(bytes)
    }
}
