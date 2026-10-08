//! Optional shared-device compute effects. CPU document pixels remain authoritative.
//!
//! Compute runs on effect workers, never waits on the window thread, and commits
//! readback only after validation and mapping succeed. Unsupported sizes, driver
//! errors, and busy devices leave the source intact for the CPU implementation.
use crate::effects::{self, Image};
pub mod composite;
use eframe::wgpu;
use serde::Serialize;
use serde_json::Value;
use std::{
    future::Future,
    num::NonZeroU64,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex, OnceLock,
    },
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};

pub const SUPPORTED: &[&str] = &[
    "levels",
    "curves",
    "adjust",
    "invert",
    "grayscale",
    "hsl",
    "color_balance",
    "blur",
    "bloom",
];
// Full transfer timings favor CPU byte LUTs. Acceleration must improve latency,
// rather than moving cheap work to the GPU merely because it is available.
pub const AUTOMATIC: &[&str] = &["adjust", "hsl", "color_balance", "blur", "bloom"];
pub const NATIVE16: &[&str] = &["adjust", "hsl", "color_balance"];
const WORKING_BUDGET: u64 = 256 * 1024 * 1024;
const PARAM_BYTES: u64 = 80;
const MAX_PASSES: u64 = 8;
const GROUP_SIZE: u32 = 256;
const BOX_CORE: u32 = 128;
static BACKEND: OnceLock<Backend> = OnceLock::new();
static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();

#[derive(Clone, Debug, Default, Serialize)]
pub struct Status {
    pub available: bool,
    pub adapter: Option<String>,
    pub effects: Vec<String>,
    pub automatic_effects: Vec<String>,
    pub effects16: Vec<String>,
    pub successful_dispatches: u64,
    pub last_ms: Option<f64>,
    pub fallback_reason: Option<String>,
}
fn status_store() -> &'static Mutex<Status> {
    STATUS.get_or_init(|| Mutex::new(Status::default()))
}
pub fn status() -> Status {
    status_store().lock().map(|s| s.clone()).unwrap_or_default()
}

/// Reuses the native renderer's existing device; does not create another GPU.
pub fn install(state: &eframe::egui_wgpu::RenderState) -> Result<(), String> {
    if BACKEND.get().is_some() {
        return Ok(());
    }
    let info = state.adapter.get_info();
    let adapter = format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type);
    let backend = Backend::new(state.device.clone(), state.queue.clone(), adapter.clone());
    match backend {
        Ok(backend) => {
            let _ = BACKEND.set(backend);
            if let Ok(mut status) = status_store().lock() {
                status.available = true;
                status.adapter = Some(adapter);
                status.effects = SUPPORTED.iter().map(|kind| (*kind).into()).collect();
                status.automatic_effects = AUTOMATIC.iter().map(|kind| (*kind).into()).collect();
                status.effects16 = NATIVE16.iter().map(|kind| (*kind).into()).collect();
                status.fallback_reason = None;
            }
            Ok(())
        }
        Err(error) => {
            if let Ok(mut status) = status_store().lock() {
                status.fallback_reason = Some(error.clone());
            }
            Err(error)
        }
    }
}

/// False requests the existing CPU path. Failed compute never modifies pixels.
pub fn try_apply(image: &mut Image, kind: &str, settings: &Value) -> bool {
    try_apply_region(
        image,
        kind,
        settings,
        u64::from(image.width) * u64::from(image.height),
    )
}
/// Keep the full layer's CPU/GPU decision when evaluating a smaller padded window.
pub(crate) fn try_apply_region(
    image: &mut Image,
    kind: &str,
    settings: &Value,
    source_pixels: u64,
) -> bool {
    let Some(backend) = BACKEND.get() else {
        return false;
    };
    // Transfers and dispatch cost more than byte loops on small previews.
    let minimum = if matches!(kind, "blur" | "bloom") {
        128 * 1024
    } else {
        512 * 1024
    };
    if source_pixels < minimum || !AUTOMATIC.contains(&kind) {
        return false;
    }
    let start = Instant::now();
    match backend.apply(image, kind, settings) {
        Ok(true) => {
            if let Ok(mut status) = status_store().lock() {
                status.successful_dispatches += 1;
                status.last_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                status.fallback_reason = None;
            }
            true
        }
        Ok(false) => false,
        Err(error) => {
            if let Ok(mut status) = status_store().lock() {
                status.fallback_reason = Some(error);
            }
            false
        }
    }
}

pub fn try_apply16(image: &mut crate::depth16::Image16, kind: &str, settings: &Value) -> bool {
    try_apply16_region(
        image,
        kind,
        settings,
        u64::from(image.width) * u64::from(image.height),
    )
}
pub(crate) fn try_apply16_region(
    image: &mut crate::depth16::Image16,
    kind: &str,
    settings: &Value,
    source_pixels: u64,
) -> bool {
    let Some(backend) = BACKEND.get() else {
        return false;
    };
    if !NATIVE16.contains(&kind) || source_pixels < 128 * 1024 {
        return false;
    }
    let start = Instant::now();
    match backend.apply16(image, kind, settings) {
        Ok(true) => {
            if let Ok(mut status) = status_store().lock() {
                status.successful_dispatches += 1;
                status.last_ms = Some(start.elapsed().as_secs_f64() * 1000.0);
                status.fallback_reason = None;
            }
            true
        }
        Ok(false) => false,
        Err(error) => {
            if let Ok(mut status) = status_store().lock() {
                status.fallback_reason = Some(error);
            }
            false
        }
    }
}

struct Pipelines {
    color: wgpu::ComputePipeline,
    color16: wgpu::ComputePipeline,
    init_blur: wgpu::ComputePipeline,
    init_bloom: wgpu::ComputePipeline,
    box_blur: wgpu::ComputePipeline,
    finish_blur: wgpu::ComputePipeline,
    finish_bloom: wgpu::ComputePipeline,
}
struct Buffers {
    capacity: u64,
    original: wgpu::Buffer,
    output: wgpu::Buffer,
    readback: wgpu::Buffer,
    scratch: Option<[wgpu::Buffer; 2]>,
    mapped: AtomicBool,
}
#[derive(Default)]
struct Workspace {
    buffers: Option<Buffers>,
}

/// Independent instances are useful for device parity tests without changing
/// the process-wide backend or its CPU reference implementation.
pub struct Backend {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipelines: Pipelines,
    parameters: wgpu::Buffer,
    table: wgpu::Buffer,
    parameter_stride: u64,
    limits: wgpu::Limits,
    workspace: Mutex<Workspace>,
    // Error scopes are a device stack; effect and compositor workers must share this gate.
    gate: Arc<Mutex<()>>,
    pub adapter: String,
}
impl Backend {
    pub fn new(device: wgpu::Device, queue: wgpu::Queue, adapter: String) -> Result<Self, String> {
        let limits = device.limits();
        if limits.max_compute_invocations_per_workgroup < GROUP_SIZE
            || limits.max_compute_workgroup_size_x < GROUP_SIZE
            || limits.max_compute_workgroup_storage_size < 4096
            || limits.max_storage_buffers_per_shader_stage < 4
        {
            return Err("GPU compute limits require CPU effects".into());
        }
        let parameter_stride = PARAM_BYTES
            .div_ceil(u64::from(limits.min_uniform_buffer_offset_alignment))
            * u64::from(limits.min_uniform_buffer_offset_alignment);
        let result = scoped(&device, || {
            let entry = |binding, ty| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty,
                count: None,
            };
            let storage = |read_only| wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            };
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("PeerBrush compute effects"),
                entries: &[
                    entry(0, storage(true)),
                    entry(1, storage(false)),
                    entry(
                        2,
                        wgpu::BindingType::Buffer {
                            ty: wgpu::BufferBindingType::Uniform,
                            has_dynamic_offset: true,
                            min_binding_size: NonZeroU64::new(PARAM_BYTES),
                        },
                    ),
                    entry(3, storage(true)),
                    entry(4, storage(true)),
                ],
            });
            let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("PeerBrush effect pipeline layout"),
                bind_group_layouts: &[&layout],
                push_constant_ranges: &[],
            });
            let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                label: Some("PeerBrush CPU-compatible effects"),
                source: wgpu::ShaderSource::Wgsl(include_str!("shaders/effects.wgsl").into()),
            });
            let pipeline = |name| {
                device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(name),
                    layout: Some(&pipeline_layout),
                    module: &shader,
                    entry_point: Some(name),
                    compilation_options: Default::default(),
                    cache: None,
                })
            };
            let pipelines = Pipelines {
                color: pipeline("color"),
                color16: pipeline("color16"),
                init_blur: pipeline("init_blur"),
                init_bloom: pipeline("init_bloom"),
                box_blur: pipeline("box_blur"),
                finish_blur: pipeline("finish_blur"),
                finish_bloom: pipeline("finish_bloom"),
            };
            let parameters = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("PeerBrush effect parameters"),
                size: parameter_stride * MAX_PASSES,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            let table = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("PeerBrush byte LUT"),
                size: 65536 * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            Ok((layout, pipelines, parameters, table))
        })?;
        Ok(Self {
            device,
            queue,
            layout: result.0,
            pipelines: result.1,
            parameters: result.2,
            table: result.3,
            parameter_stride,
            limits,
            workspace: Mutex::new(Workspace::default()),
            gate: Arc::new(Mutex::new(())),
            adapter,
        })
    }

    pub fn apply(&self, image: &mut Image, kind: &str, settings: &Value) -> Result<bool, String> {
        let Ok(_gate) = self.gate.try_lock() else {
            return Ok(false);
        };
        if !SUPPORTED.contains(&kind) {
            return Ok(false);
        }
        let plan = Plan::new(image, kind, settings, &self.limits)?;
        let Some(plan) = plan else {
            return Ok(false);
        };
        // Another worker may be using this device. CPU fallback avoids a queue of
        // old previews holding up the latest human gesture.
        let Ok(mut workspace) = self.workspace.try_lock() else {
            return Ok(false);
        };
        let output = scoped(&self.device, || {
            self.run(
                image.width,
                image.height,
                &image.bytes,
                &plan,
                &mut workspace,
            )
        });
        if let Some(buffers) = &workspace.buffers {
            if buffers.mapped.swap(false, Ordering::Relaxed) {
                buffers.readback.unmap();
            }
        }
        let output = match output {
            Ok(output) => output,
            Err(error) => {
                // A failed mapping/allocation may leave pending buffer state;
                // discard it so a later edit can retry on healthy resources.
                workspace.buffers = None;
                return Err(error);
            }
        };
        if output.len() != image.bytes.len() {
            return Err("GPU effect returned an incomplete image".into());
        }
        image.bytes = output;
        Ok(true)
    }

    /// Pointwise native16 tiles preserve full source words. No tile is written
    /// into the image until every dispatch/readback has succeeded.
    pub fn apply16(
        &self,
        image: &mut crate::depth16::Image16,
        kind: &str,
        settings: &Value,
    ) -> Result<bool, String> {
        self.apply16_impl(image, kind, settings, |_| Ok(()))
    }
    fn apply16_impl(
        &self,
        image: &mut crate::depth16::Image16,
        kind: &str,
        settings: &Value,
        mut before_tile: impl FnMut(usize) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Ok(_gate) = self.gate.try_lock() else {
            return Ok(false);
        };
        if !NATIVE16.contains(&kind) || cfg!(target_endian = "big") {
            return Ok(false);
        }
        image.validate()?;
        let dummy = Image {
            width: 1,
            height: 1,
            bytes: vec![0; 4],
        };
        let Some(mut plan) = Plan::new(&dummy, kind, settings, &self.limits)? else {
            return Ok(false);
        };
        let settings = effects::normalized(kind, settings)?;
        plan.native = true;
        if kind == "adjust" {
            plan.table = (0..65536)
                .map(|v| {
                    (crate::depth16::color::map(kind, &settings, v as f64 / 65535.0) * 65535.0)
                        .round()
                        .clamp(0.0, 65535.0) as u32
                })
                .collect();
        }
        let tile_pixels = (1024 * 1024_u64)
            .min(u64::from(self.limits.max_storage_buffer_binding_size) / 8)
            .min(self.limits.max_buffer_size / 8)
            .min(WORKING_BUDGET / 24) as usize;
        if tile_pixels == 0 {
            return Err("Native16 GPU tile exceeds device buffers".into());
        }
        let Ok(mut workspace) = self.workspace.try_lock() else {
            return Ok(false);
        };
        let mut candidate = Vec::with_capacity(image.words.len());
        for (tile, words) in image.words.chunks(tile_pixels * 4).enumerate() {
            before_tile(tile)?;
            let pixels = (words.len() / 4) as u32;
            let total = pixels.div_ceil(GROUP_SIZE);
            let x = total.min(self.limits.max_compute_workgroups_per_dimension);
            let y = total.div_ceil(x);
            if y > self.limits.max_compute_workgroups_per_dimension {
                return Err("Native16 GPU tile exceeds dispatch limits".into());
            }
            plan.params.dims[0] = pixels;
            plan.params.dims[1] = 1;
            plan.params.dims[3] = x;
            plan.groups = (x, y);
            let output = scoped(&self.device, || {
                self.run(
                    pixels,
                    1,
                    bytemuck::cast_slice(words),
                    &plan,
                    &mut workspace,
                )
            });
            if let Some(buffers) = &workspace.buffers {
                if buffers.mapped.swap(false, Ordering::Relaxed) {
                    buffers.readback.unmap();
                }
            }
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    workspace.buffers = None;
                    return Err(error);
                }
            };
            if output.len() != words.len() * 2 {
                return Err("Native16 GPU tile returned incomplete samples".into());
            }
            candidate.extend(
                output
                    .chunks_exact(2)
                    .map(|p| u16::from_le_bytes([p[0], p[1]])),
            );
        }
        if candidate.len() != image.words.len() {
            return Err("Native16 GPU result is incomplete".into());
        }
        image.words = candidate;
        Ok(true)
    }

    fn run(
        &self,
        width: u32,
        height: u32,
        input: &[u8],
        plan: &Plan,
        workspace: &mut Workspace,
    ) -> Result<Vec<u8>, String> {
        let size = input.len() as u64;
        let replace = workspace
            .buffers
            .as_ref()
            .is_none_or(|b| b.capacity < size || (plan.blur && b.scratch.is_none()));
        if replace {
            // Drop previous completed buffers before allocating their replacement.
            workspace.buffers = None;
            let buffer = |label, bytes, usage| {
                self.device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: bytes,
                    usage,
                    mapped_at_creation: false,
                })
            };
            let storage = wgpu::BufferUsages::STORAGE
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::COPY_SRC;
            workspace.buffers = Some(Buffers {
                mapped: AtomicBool::new(false),
                capacity: size,
                original: buffer("PeerBrush effect source", size, storage),
                output: buffer("PeerBrush effect output", size, storage),
                readback: buffer(
                    "PeerBrush effect readback",
                    size,
                    wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                ),
                scratch: plan.blur.then(|| {
                    [
                        buffer("PeerBrush premultiplied A", size * 2, storage),
                        buffer("PeerBrush premultiplied B", size * 2, storage),
                    ]
                }),
            });
        }
        let buffers = workspace.buffers.as_ref().unwrap();
        self.queue.write_buffer(&buffers.original, 0, input);
        let table: Vec<u8> = plan.table.iter().flat_map(|v| v.to_le_bytes()).collect();
        self.queue.write_buffer(&self.table, 0, &table);
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("PeerBrush effect compute"),
            });
        let mut slot = 0u32;
        let mut dispatch = |pipeline: &wgpu::ComputePipeline,
                            source: &wgpu::Buffer,
                            destination: &wgpu::Buffer,
                            params: Params,
                            x,
                            y| {
            self.queue.write_buffer(
                &self.parameters,
                u64::from(slot) * self.parameter_stride,
                &params.bytes(),
            );
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("PeerBrush effect pass"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: source.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: destination.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &self.parameters,
                            offset: 0,
                            size: NonZeroU64::new(PARAM_BYTES),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: self.table.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 4,
                        resource: buffers.original.as_entire_binding(),
                    },
                ],
            });
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("PeerBrush effect pass"),
                timestamp_writes: None,
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(
                0,
                &group,
                &[(u64::from(slot) * self.parameter_stride) as u32],
            );
            pass.dispatch_workgroups(x, y, 1);
            slot += 1;
        };
        if plan.blur {
            let [a, b] = buffers.scratch.as_ref().unwrap();
            dispatch(
                if plan.bloom {
                    &self.pipelines.init_bloom
                } else {
                    &self.pipelines.init_blur
                },
                &buffers.original,
                a,
                plan.params.clone(),
                plan.groups.0,
                plan.groups.1,
            );
            let mut source = a;
            let mut destination = b;
            for radius in plan.radii {
                if radius == 0 {
                    continue;
                }
                for vertical in [false, true] {
                    let mut params = plan.params.clone();
                    params.dims[2] = u32::from(vertical);
                    params.dims[3] = radius;
                    let (length, lines) = if vertical {
                        (height, width)
                    } else {
                        (width, height)
                    };
                    dispatch(
                        &self.pipelines.box_blur,
                        source,
                        destination,
                        params,
                        length.div_ceil(BOX_CORE),
                        lines,
                    );
                    std::mem::swap(&mut source, &mut destination);
                }
            }
            dispatch(
                if plan.bloom {
                    &self.pipelines.finish_bloom
                } else {
                    &self.pipelines.finish_blur
                },
                source,
                &buffers.output,
                plan.params.clone(),
                plan.groups.0,
                plan.groups.1,
            );
        } else {
            dispatch(
                if plan.native {
                    &self.pipelines.color16
                } else {
                    &self.pipelines.color
                },
                &buffers.original,
                &buffers.output,
                plan.params.clone(),
                plan.groups.0,
                plan.groups.1,
            );
        }
        encoder.copy_buffer_to_buffer(&buffers.output, 0, &buffers.readback, 0, size);
        let submission = self.queue.submit(Some(encoder.finish()));
        let (sender, receiver) = mpsc::channel();
        buffers
            .readback
            .slice(..size)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        // Wait only for this compute submission, not continuously queued frames.
        self.device
            .poll(wgpu::Maintain::WaitForSubmissionIndex(submission));
        receiver
            .recv_timeout(Duration::from_secs(5))
            .map_err(|_| "GPU effect readback timed out")?
            .map_err(|error| format!("GPU effect readback failed: {error}"))?;
        buffers.mapped.store(true, Ordering::Relaxed);
        let result = buffers.readback.slice(..size).get_mapped_range().to_vec();
        Ok(result)
    }
}

#[derive(Clone)]
struct Params {
    dims: [u32; 4],
    factors: [f32; 4],
    balance: [[f32; 4]; 3],
}
impl Params {
    fn bytes(&self) -> Vec<u8> {
        self.dims
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .chain(
                self.factors
                    .iter()
                    .chain(self.balance.iter().flatten())
                    .flat_map(|v| v.to_le_bytes()),
            )
            .collect()
    }
}
struct Plan {
    params: Params,
    table: Vec<u32>,
    groups: (u32, u32),
    radii: [u32; 3],
    blur: bool,
    bloom: bool,
    native: bool,
}
impl Plan {
    fn new(
        image: &Image,
        kind: &str,
        settings: &Value,
        limits: &wgpu::Limits,
    ) -> Result<Option<Self>, String> {
        let settings = effects::normalized(kind, settings)?;
        let pixels = u64::from(image.width)
            .checked_mul(u64::from(image.height))
            .ok_or("GPU effect image size overflow")?;
        if pixels.checked_mul(4) != Some(image.bytes.len() as u64) {
            return Err("GPU effect image dimensions do not match its pixels".into());
        }
        if pixels == 0 {
            return Ok(None);
        }
        let blur = matches!(kind, "blur" | "bloom");
        let size = pixels * 4;
        let working = size
            .checked_mul(if blur { 7 } else { 3 })
            .ok_or("GPU effect buffer size overflow")?;
        if working > WORKING_BUDGET
            || size * if blur { 2 } else { 1 } > u64::from(limits.max_storage_buffer_binding_size)
            || size * if blur { 2 } else { 1 } > limits.max_buffer_size
        {
            return Err("GPU effect size exceeds bounded device buffers; using CPU".into());
        }
        let max_groups = limits.max_compute_workgroups_per_dimension;
        let group_count = pixels.div_ceil(u64::from(GROUP_SIZE));
        let x = group_count.min(u64::from(max_groups)) as u32;
        let y = group_count.div_ceil(u64::from(x)) as u32;
        if y > max_groups || (blur && (image.width > max_groups || image.height > max_groups)) {
            return Err("GPU effect dimensions exceed compute dispatch limits; using CPU".into());
        }
        let mut params = Params {
            dims: [image.width, image.height, 0, x],
            factors: [0.0; 4],
            balance: [[0.0; 4]; 3],
        };
        match kind {
            "adjust" => {
                params.dims[2] = 1;
                params.factors[0] = effects::number(&settings, "saturation", 1.0);
            }
            "grayscale" => params.dims[2] = 2,
            "hsl" => {
                params.dims[2] = 3;
                params.factors = [
                    effects::number(&settings, "hue", 0.0) / 60.0,
                    effects::number(&settings, "saturation", 0.0),
                    effects::number(&settings, "lightness", 0.0),
                    0.0,
                ];
                if params.factors[..3].iter().all(|v| *v == 0.0) {
                    return Ok(None);
                }
            }
            "color_balance" => {
                params.dims[2] = 4;
                params.factors[3] = if settings["preserve_luminosity"].as_bool().unwrap_or(true) {
                    1.0
                } else {
                    0.0
                };
                for (index, band) in ["shadows", "midtones", "highlights"].iter().enumerate() {
                    for channel in 0..3 {
                        params.balance[index][channel] =
                            settings[*band][channel].as_f64().unwrap_or(0.0) as f32;
                    }
                }
            }
            "bloom" => {
                params.factors[0] = effects::number(&settings, "threshold", 0.75);
                params.factors[1] = effects::number(&settings, "strength", 0.5);
                if params.factors[0] == 1.0 || params.factors[1] == 0.0 {
                    return Ok(None);
                }
            }
            _ => {}
        }
        let sigma = effects::number(
            &settings,
            if kind == "bloom" { "spread" } else { "radius" },
            if kind == "bloom" { 12.0 } else { 8.0 },
        );
        if kind == "blur" && sigma < 0.5 {
            return Ok(None);
        }
        let radii = if blur {
            effects::gaussian_radii(sigma).map(|v| v as u32)
        } else {
            [0; 3]
        };
        if radii.iter().any(|r| *r > 64) {
            return Err("GPU blur radius exceeds the shader halo".into());
        }
        let table = (0..256)
            .map(|i| {
                (effects::map_value(kind, &settings, i as f32 / 255.0) * 255.0).round() as u8 as u32
            })
            .collect();
        Ok(Some(Self {
            params,
            table,
            groups: (x, y),
            radii,
            blur,
            bloom: kind == "bloom",
            native: false,
        }))
    }
}

fn scoped<T>(
    device: &wgpu::Device,
    operation: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    device.push_error_scope(wgpu::ErrorFilter::Internal);
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let result = catch_unwind(AssertUnwindSafe(operation))
        .map_err(|_| "GPU effect failed; using CPU".to_string())
        .and_then(|result| result);
    let mut errors = Vec::new();
    for _ in 0..3 {
        if let Some(error) = block_on(device.pop_error_scope()) {
            errors.push(error.to_string());
        }
    }
    if errors.is_empty() {
        result
    } else {
        Err(errors.join("; "))
    }
}

/// Small native future executor, avoiding another runtime dependency.
pub fn block_on<F: Future>(future: F) -> F::Output {
    struct ThreadWake(std::thread::Thread);
    impl Wake for ThreadWake {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// Headless test/example device; normal application startup calls install().
pub fn headless() -> Result<Backend, String> {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .ok_or("No compute adapter available")?;
    let info = adapter.get_info();
    let (device, queue) = block_on(adapter.request_device(
        &wgpu::DeviceDescriptor {
            label: Some("PeerBrush GPU parity"),
            required_features: wgpu::Features::empty(),
            required_limits: wgpu::Limits::default(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
        },
        None,
    ))
    .map_err(|error| error.to_string())?;
    Backend::new(
        device,
        queue,
        format!("{} ({:?}, {:?})", info.name, info.backend, info.device_type),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn device_limits_reject_work_without_touching_source() {
        let image = Image {
            width: 1024,
            height: 1024,
            bytes: vec![17; 1024 * 1024 * 4],
        };
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffer_binding_size = 4096;
        assert!(Plan::new(&image, "blur", &json!({"radius":64}), &limits).is_err());
        assert!(image.bytes.iter().all(|v| *v == 17));
        assert!(Plan::new(
            &image,
            "blur",
            &json!({"radius":0}),
            &wgpu::Limits::default()
        )
        .unwrap()
        .is_none());
    }
    #[test]
    fn parameters_match_uniform_alignment_and_cpu_radii() {
        let image = Image {
            width: 2,
            height: 1,
            bytes: vec![255; 8],
        };
        let plan = Plan::new(
            &image,
            "blur",
            &json!({"radius":64}),
            &wgpu::Limits::default(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(plan.radii, effects::gaussian_radii(64.0).map(|r| r as u32));
        assert!(plan.radii.iter().all(|r| *r <= 64));
        assert_eq!(plan.params.bytes().len() as u64, PARAM_BYTES);
    }
    #[test]
    #[ignore = "requires native compute adapter"]
    fn native_tile_failure_keeps_every_original_word() {
        let backend = headless().unwrap();
        let mut image = crate::depth16::Image16 {
            width: 1024,
            height: 1025,
            words: vec![12345; 1024 * 1025 * 4],
        };
        for p in image.words.chunks_exact_mut(4) {
            p.copy_from_slice(&[12345, 23456, 34567, 65535]);
        }
        let source = image.words.clone();
        let mut completed = 0;
        let result = backend.apply16_impl(&mut image, "hsl", &json!({"hue":60}), |tile| {
            if tile == 1 {
                return Err("simulated later tile failure".into());
            }
            completed += 1;
            Ok(())
        });
        assert_eq!(result.unwrap_err(), "simulated later tile failure");
        assert_eq!(completed, 1, "first tile actually completed before failure");
        assert_eq!(
            image.words, source,
            "failed work cannot partially overwrite source"
        );
    }
}
