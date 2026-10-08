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
const OUTPUT_BUDGET: usize = 16 * 1024 * 1024;
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
                    let mut bytes = vec![0; TILE_BYTES];
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
        let masks = vec![None; doc.layers.len()];
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
        assert!(max <= 1, "preview channel error {max}");
    }
    #[test]
    fn scene_preserves_groups_and_rejects_unsupported_structures() {
        let mut doc = fixture(271);
        let colors = vec![None; doc.layers.len()];
        let scene = Scene::new(&doc, &colors).unwrap();
        assert_eq!(
            scene.nodes.iter().map(|n| n[0]).collect::<Vec<_>>(),
            vec![0, 2, 0, 0, 3]
        );
        doc.layers[1].mask = Some(Mask {
            enabled: true,
            steps: vec![],
            cache_key: "mask".into(),
        });
        assert!(Scene::new(&doc, &colors).is_err());
        doc.layers[1].mask.as_mut().unwrap().enabled = false;
        assert!(Scene::new(&doc, &colors).is_ok());
        doc.layers[1].blend = "multiply".into();
        assert!(Scene::new(&doc, &colors).is_err());
        doc.layers[1].blend = "normal".into();
        doc.bit_depth = 16;
        assert!(Scene::new(&doc, &colors).is_err());
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
        slot.as_mut().unwrap().render(doc, colors, xs, ys)
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
    Raw(usize),
    Derived(String, u32, u32, u32, u32),
}
struct Entry {
    slot: u32,
    stamp: u64,
    _source: Option<Weak<Vec<u8>>>,
}
enum Source<'a> {
    Raster(&'a Raster),
    Image(&'a Image, &'a str),
}
impl Source<'_> {
    fn dims(&self) -> (u32, u32) {
        match self {
            Self::Raster(r) => (r.width, r.height),
            Self::Image(i, _) => (i.width, i.height),
        }
    }
    fn key(&self, x: u32, y: u32) -> Option<Key> {
        match self {
            Self::Raster(r) => r
                .tiles
                .get(&(x, y))
                .map(|p| Key::Raw(Arc::as_ptr(p) as usize)),
            Self::Image(i, key) => Some(Key::Derived((*key).into(), i.width, i.height, x, y)),
        }
    }
}
struct Scene<'a> {
    nodes: Vec<[u32; 12]>,
    sources: Vec<Source<'a>>,
    offsets: Vec<usize>,
    lookup_len: usize,
}
impl<'a> Scene<'a> {
    fn new(doc: &'a Document, colors: &'a [Option<Arc<Image>>]) -> Result<Self, String> {
        if cfg!(target_endian = "big") || doc.bit_depth != 8 || colors.len() != doc.layers.len() {
            return Err("Native precision requires CPU compositing".into());
        }
        let mut scene = Self {
            nodes: vec![],
            sources: vec![],
            offsets: vec![],
            lookup_len: 0,
        };
        scene.children(doc, colors, None, 0)?;
        if scene.nodes.is_empty() {
            let mut empty = [0; 12];
            empty[0] = 1;
            scene.nodes.push(empty);
        }
        Ok(scene)
    }
    fn children(
        &mut self,
        doc: &'a Document,
        colors: &'a [Option<Arc<Image>>],
        parent: Option<&str>,
        depth: usize,
    ) -> Result<(), String> {
        if depth > 16 {
            return Err("Folder depth requires CPU compositing".into());
        }
        for (i, layer) in doc.layers.iter().enumerate().rev() {
            if layer.parent.as_deref() != parent || !layer.visible {
                continue;
            }
            if layer.blend != "normal"
                || layer.clip_to.is_some()
                || layer.mask.as_ref().is_some_and(|m| m.enabled)
                || layer.kind == "adjustment"
                || layer.pixels.depth != 8
            {
                return Err("Blend, clipping, adjustment or mask requires CPU compositing".into());
            }
            let mut node = [0; 12];
            node[1] = layer.pixels.width;
            node[2] = layer.pixels.height;
            node[4] = layer.x as u32;
            node[5] = layer.y as u32;
            node[6] = layer.opacity.to_bits();
            if let Some(image) = colors[i].as_deref() {
                if layer.kind == "group" {
                    node[4] = 0;
                    node[5] = 0;
                }
                node[1] = image.width;
                node[2] = image.height;
                self.source(&mut node, Source::Image(image, &layer.effect_key));
            } else if layer.kind == "group" {
                node[0] = 2;
                self.nodes.push(node);
                self.children(doc, colors, Some(&layer.id), depth + 1)?;
                node[0] = 3;
            } else if layer.kind == "fill" {
                node[0] = 1;
                node[3] = u32::from_le_bytes(layer.color);
            } else {
                self.source(&mut node, Source::Raster(&layer.pixels));
            }
            self.nodes.push(node);
            if self.nodes.len() > 400 {
                return Err("Layer count requires CPU compositing".into());
            }
        }
        Ok(())
    }
    fn source(&mut self, node: &mut [u32; 12], source: Source<'a>) {
        let (w, h) = source.dims();
        node[7] = self.lookup_len as u32;
        node[8] = w.div_ceil(TILE);
        self.offsets.push(self.lookup_len);
        self.lookup_len += (w.div_ceil(TILE) * h.div_ceil(TILE)) as usize;
        // CPU-only source index in an unused shader field.
        node[9] = self.sources.len() as u32;
        self.sources.push(source);
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
                label: Some("PeerBrush tiled normal compositor"),
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
            })
        })
    }
    pub fn resident_bytes(&self) -> usize {
        self.cache.len() * TILE_BYTES
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
        let slot = if self.cache.len() < SLOTS {
            self.cache.len() as u32
        } else {
            let old = self
                .cache
                .iter()
                .filter(|(key, _)| !protected.contains(*key))
                .min_by_key(|(_, e)| e.stamp)
                .map(|(key, _)| key.clone())
                .ok_or("Preview tile working set exceeds the GPU budget")?;
            self.cache.remove(&old).unwrap().slot
        };
        let (bytes, weak) = match source {
            Source::Raster(r) => {
                let p = r.tiles.get(&(x, y)).unwrap();
                (p.as_slice().to_vec(), Some(Arc::downgrade(p)))
            }
            Source::Image(image, _) => {
                let mut bytes = vec![0; TILE_BYTES];
                let width = (image.width - x * TILE).min(TILE) as usize;
                for row in 0..(image.height - y * TILE).min(TILE) as usize {
                    let start = (((y * TILE) as usize + row) * image.width as usize
                        + (x * TILE) as usize)
                        * 4;
                    bytes[row * TILE as usize * 4..row * TILE as usize * 4 + width * 4]
                        .copy_from_slice(&image.bytes[start..start + width * 4]);
                }
                (bytes, None)
            }
        };
        if bytes.len() != TILE_BYTES {
            return Err("Invalid preview tile length".into());
        }
        self.queue
            .write_buffer(&self.atlas, u64::from(slot) * TILE_BYTES as u64, &bytes);
        self.uploaded += TILE_BYTES as u64;
        self.cache.insert(
            key,
            Entry {
                slot,
                stamp: self.stamp,
                _source: weak,
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
        let gate = self.gate.clone();
        let _gate = gate.try_lock().map_err(|_| "GPU busy; using CPU")?;
        let scene = Scene::new(doc, colors)?;
        let len = xs
            .len()
            .checked_mul(ys.len())
            .and_then(|n| n.checked_mul(4))
            .filter(|n| *n > 0 && *n <= OUTPUT_BUDGET)
            .ok_or("Preview output exceeds the GPU budget")?;
        let tiles = xs.len().div_ceil(TILE as usize) * ys.len().div_ceil(TILE as usize);
        if (scene.lookup_len + 512) * 4 * tiles > METADATA_BUDGET {
            return Err("Preview metadata exceeds the GPU budget".into());
        }
        if xs.windows(2).any(|v| v[0] > v[1]) || ys.windows(2).any(|v| v[0] > v[1]) {
            return Err("Preview sampling grid is not ordered".into());
        }
        let result = scoped(&self.device.clone(), || {
            self.dispatch(&scene, xs, ys, len, before_tile)
        });
        // A failed queue upload must not leave apparently valid resident entries.
        if result.is_err() {
            self.cache.clear();
        }
        result
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
                for node in &scene.nodes {
                    if node[0] != 0 {
                        continue;
                    }
                    let source = &scene.sources[node[9] as usize];
                    let (w, h) = source.dims();
                    let x0 =
                        (i64::from(xcoords[0]) - i64::from(node[4] as i32)).clamp(0, i64::from(w));
                    let x1 = (i64::from(*xcoords.last().unwrap()) - i64::from(node[4] as i32) + 1)
                        .clamp(0, i64::from(w));
                    let y0 =
                        (i64::from(ycoords[0]) - i64::from(node[5] as i32)).clamp(0, i64::from(h));
                    let y1 = (i64::from(*ycoords.last().unwrap()) - i64::from(node[5] as i32) + 1)
                        .clamp(0, i64::from(h));
                    if x0 >= x1 || y0 >= y1 {
                        continue;
                    }
                    for y in y0 as u32 / TILE..(y1 as u32).div_ceil(TILE) {
                        for x in x0 as u32 / TILE..(x1 as u32).div_ceil(TILE) {
                            if let Some(key) = source.key(x, y) {
                                protected.insert(key);
                                needed.push((node[9] as usize, x, y));
                            }
                        }
                    }
                }
                if protected.len() > SLOTS {
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
