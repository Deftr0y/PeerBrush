pub(crate) mod authority;
pub mod backed;
mod document_settings;
mod image_patch;
pub(crate) use image_patch::precision as image_patch_precision;
pub mod native;
pub(crate) mod primitive;
pub(crate) mod report;
pub(crate) mod reframe_scope;
pub(crate) mod runtime_value;
pub use runtime_value::RuntimeValue;
pub use authority::ResidentAuthority;
pub use report::StateCapture;
pub mod scene;
pub(crate) mod view;
use crate::raster::{blend, Pixel, Raster};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct MaskStep {
    pub id: String,
    pub kind: String,
    pub enabled: bool,
    #[serde(default = "crate::effects::full_weight")]
    pub weight: f32,
    pub value: f32,
    pub pixels: Raster,
    #[serde(default)]
    pub settings: Value,
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Mask {
    pub enabled: bool,
    #[serde(default = "mask_linked")]
    pub linked: bool,
    pub steps: Vec<MaskStep>,
    #[serde(skip, default = "id")]
    pub cache_key: String,
}
fn mask_linked() -> bool {
    true
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Layer {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub parent: Option<String>,
    #[serde(default)]
    pub clip_to: Option<String>,
    pub visible: bool,
    pub locked: bool,
    pub opacity: f32,
    pub blend: String,
    pub x: i32,
    pub y: i32,
    pub pixels: Raster,
    pub color: Pixel,
    pub mask: Option<Mask>,
    #[serde(default)]
    pub effects: Vec<crate::effects::Effect>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub psd_metadata: Vec<crate::psd::LayerMetadata>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<Arc<crate::source::Source>>,
    #[serde(skip, default = "id")]
    pub effect_key: String,
}
impl Layer {
    pub fn new(name: &str, kind: &str, w: u32, h: u32) -> Self {
        Self {
            id: id(),
            name: name.into(),
            kind: kind.into(),
            parent: None,
            clip_to: None,
            visible: true,
            locked: false,
            opacity: 1.0,
            blend: "normal".into(),
            x: 0,
            y: 0,
            pixels: Raster::new(w, h),
            color: [233, 84, 32, 255],
            mask: None,
            effects: vec![],
            psd_metadata: vec![],
            source: None,
            effect_key: id(),
        }
    }
    pub fn mask_value(&self, x: i32, y: i32) -> f32 {
        if self.mask.as_ref().is_none_or(|m| !m.enabled) {
            return 1.0;
        }
        self.mask_value_raw(x, y)
    }
    pub fn mask_value_raw(&self, x: i32, y: i32) -> f32 {
        let prepared = self
            .mask
            .as_ref()
            .and_then(|m| m.prepare(self.pixels.width, self.pixels.height));
        self.mask_value_prepared(x, y, prepared.as_deref(), true)
    }
    pub fn mask_value_prepared(
        &self,
        x: i32,
        y: i32,
        prepared: Option<&crate::mask::GrayMask>,
        raw: bool,
    ) -> f32 {
        let Some(m) = &self.mask else {
            return 1.0;
        };
        if !raw && !m.enabled {
            return 1.0;
        }
        if let Some(image) = prepared {
            return image.value(x, y);
        }
        let mut v = 255.0;
        for step in &m.steps {
            if !step.enabled || step.weight == 0.0 {
                continue;
            }
            let before = v;
            match step.kind.as_str() {
                "fill" => v = step.value,
                "paint" => {
                    let p = step.pixels.get(x, y);
                    let a = p[3] as f32 / 255.0;
                    v = v * (1.0 - a) + p[0] as f32 * a;
                }
                "invert" => v = 255.0 - v,
                "density" => v = f32::from(crate::mask::density_sample(f64::from(v), step.value, 255)),
                "levels" => {
                    v = ((v / 255.0).powf(step.value.clamp(0.1, 5.0)) * 255.0).clamp(0.0, 255.0)
                }
                _ => {}
            }
            v = before * (1.0 - step.weight) + v * step.weight;
        }
        v.clamp(0.0, 255.0) / 255.0
    }
}

fn default_bit_depth() -> u16 {
    8
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Document {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    #[serde(default = "default_bit_depth")]
    pub bit_depth: u16,
    /// Protected source profile; editable copies explicitly convert to sRGB.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icc_profile: Option<std::sync::Arc<Vec<u8>>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub srgb_tagged: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub psd_intentionally_untagged: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub psd_rgb8_multiply: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_resolution: Option<[u8; 16]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_print_scale: Option<[u8; 14]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_pixel_aspect: Option<[u8; 12]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_grid: Option<[u8; 16]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_layer_id_seed: Option<[u8; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_print_flags: Option<[u8; 9]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_print_flags_info: Option<[u8; 10]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_copyright: Option<[u8; 1]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_file_info: Option<std::sync::Arc<Vec<u8>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_global_angle: Option<[u8; 4]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_global_altitude: Option<[u8; 4]>,
    /// Preferred per-layer Photoshop mask settings, retained as standard data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub psd_global_mask: Option<crate::psd::GlobalMaskSettings>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub psd_global_metadata: Vec<crate::psd::GlobalMetadata>,
    pub layers: Vec<Layer>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<crate::effects::Effect>,
    pub selection: Option<[i32; 4]>,
    #[serde(default)]
    pub selection_polygon: Option<Vec<[f32; 2]>>,
    #[serde(default)]
    pub selection_coverage: Option<crate::selection::Coverage>,
    #[serde(default)]
    pub selection_previous: Option<crate::selection::Coverage>,
    /// Runtime wand seed; saved documents retain coverage, never replay metadata.
    #[serde(skip)]
    pub wand_source: Option<std::sync::Arc<crate::selection::WandSource>>,
    /// View-only source records for protected PSDs; never editable layer IDs or saved sources.
    #[serde(skip)]
    pub source_structure: Vec<crate::psd::SourceLayerInfo>,
    /// Runtime-only raw pattern inspection; not editable effects or saved PSD sources.
    #[serde(skip)]
    pub source_patterns: Vec<crate::psd::patterns::Source>,
    pub read_only: bool,
    pub warnings: Vec<String>,
}
impl Document {
    pub fn new(w: u32, h: u32) -> Result<Self, String> {
        Self::new_with_limits(w, h, crate::raster::CanvasLimits::Resident)
    }
    pub(crate) fn new_with_limits(
        w: u32,
        h: u32,
        limits: crate::raster::CanvasLimits,
    ) -> Result<Self, String> {
        limits.check(w, h)?;
        Ok(Self {
            id: id(),
            name: "Untitled.psd".into(),
            width: w,
            height: h,
            revision: 0,
            bit_depth: 8,
            icc_profile: None,
            srgb_tagged: false,
            psd_intentionally_untagged: false,
            psd_rgb8_multiply: false,
            psd_resolution: None,
            psd_print_scale: None,
            psd_pixel_aspect: None,
            psd_grid: None,
            psd_layer_id_seed: None,
            psd_print_flags: None,
            psd_print_flags_info: None,
            psd_copyright: None,
            psd_file_info: None,
            psd_global_angle: None,
            psd_global_altitude: None,
            psd_global_mask: None,
            psd_global_metadata: vec![],
            layers: vec![Layer::new("Paint 1", "paint", w, h)],
            filters: vec![],
            selection: None,
            selection_polygon: None,
            selection_coverage: None,
            selection_previous: None,
            wand_source: None,
            source_structure: vec![],
            source_patterns: vec![],
            read_only: false,
            warnings: vec![],
        })
    }
    pub fn new_depth(w: u32, h: u32, depth: u16) -> Result<Self, String> {
        if ![8, 16].contains(&depth) {
            return Err("Color depth must be 8 or 16 bits".into());
        }
        let mut doc = Self::new(w, h)?;
        doc.bit_depth = depth;
        doc.ensure_depth();
        Ok(doc)
    }
    /// Promote new/imported sources once; never lower existing precision.
    pub(crate) fn ensure_depth(&mut self) {
        if self.layers.iter().any(|l| {
            l.pixels.depth == 16
                || l.effects.iter().any(|e| {
                    crate::effects::patterns::from_settings(&e.kind, &e.settings)
                        .ok()
                        .flatten()
                        .is_some_and(|b| b.depth == 16)
                })
                || l.mask
                    .as_ref()
                    .is_some_and(|m| m.steps.iter().any(|s| s.pixels.depth == 16))
        }) {
            self.bit_depth = 16;
        }
        if self.bit_depth == 16 {
            for layer in &mut self.layers {
                layer.pixels.promote16();
                if let Some(mask) = &mut layer.mask {
                    for step in &mut mask.steps {
                        step.pixels.promote16();
                    }
                }
            }
        }
        if !self.psd_intentionally_untagged && self.layers.iter().any(|l| {
            l.effects
                .iter()
                .any(|e| crate::effects::patterns::active(&e.kind, &e.settings))
        }) {
            self.srgb_tagged = true;
        }
    }
    pub fn preview(
        &self,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Result<(u32, u32, Vec<u8>, [i32; 4]), String> {
        self.preview_checked(rect, edge, target, mask, &|| Ok(()))
    }
    pub(crate) fn preview_checked(
        &self,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        check: &crate::render::Check<'_>,
    ) -> Result<(u32, u32, Vec<u8>, [i32; 4]), String> {
        if self.bit_depth == 16 {
            return crate::depth16::preview_checked(self, rect, edge, target, mask, check);
        }
        let (w, h, mut bytes, rect) = self.preview_native8_checked(rect, edge, target, mask, check)?;
        if !mask {
            if let Some(profile) = self.icc_profile.as_deref() {
                // Unsupported profiles retain raw feedback with a visible warning.
                // Explicit conversion uses the strict, fallible path instead.
                if crate::color_profile::supported(profile).is_ok() {
                    // Keep CMS row boundaries identical to the native renderer.
                    for row in bytes.chunks_exact_mut(w as usize * 4) {
                        check()?;
                        crate::color_profile::convert8(profile, row)?;
                    }
                }
            }
        }
        check()?;
        Ok((w, h, bytes, rect))
    }
    pub(crate) fn preview_native8(
        &self,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Result<(u32, u32, Vec<u8>, [i32; 4]), String> {
        self.preview_native8_checked(rect, edge, target, mask, &|| Ok(()))
    }
    pub(crate) fn preview_native8_checked(
        &self,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
        check: &crate::render::Check<'_>,
    ) -> Result<(u32, u32, Vec<u8>, [i32; 4]), String> {
        check()?;
        let mut rect = rect.unwrap_or([0, 0, self.width as i32, self.height as i32]);
        rect[0] = rect[0].clamp(0, self.width as i32 - 1);
        rect[1] = rect[1].clamp(0, self.height as i32 - 1);
        rect[2] = rect[2].clamp(rect[0] + 1, self.width as i32);
        rect[3] = rect[3].clamp(rect[1] + 1, self.height as i32);
        let rw = (rect[2] - rect[0]) as u32;
        let rh = (rect[3] - rect[1]) as u32;
        let scale = (edge.max(1).min(8192) as f32 / rw.max(rh) as f32).min(1.0);
        let w = ((rw as f32 * scale).round() as u32).max(1);
        let h = ((rh as f32 * scale).round() as u32).max(1);
        if target.is_none() && !mask && crate::filters::active(self) {
            let image = crate::filters::prepare8_checked(self, check)?;
            let bytes = crate::render::rgba(w, h, check, |x, y| {
                image.get(
                    rect[0] + (x as f32 / scale) as i32,
                    rect[1] + (y as f32 / scale) as i32,
                )
            })?;
            return Ok((w, h, bytes, rect));
        }
        let layer = if let Some(target) = target {
            Some(
                self.layers
                    .iter()
                    .find(|l| l.id == target)
                    .ok_or("Layer no longer exists")?,
            )
        } else {
            None
        };
        if let Some(layer) = layer.filter(|_| mask) {
            let prepared = crate::mask::prepare_layers(std::slice::from_ref(layer), check)?;
            let bytes = crate::render::rgba(w, h, check, |x, y| {
                let sx = rect[0] + (x as f32 / scale) as i32;
                let sy = rect[1] + (y as f32 / scale) as i32;
                let gray = (layer.mask_value_prepared(sx - layer.x, sy - layer.y, prepared[0].as_deref(), true) * 255.) as u8;
                [gray, gray, gray, 255]
            })?;
            return Ok((w, h, bytes, rect));
        }
        let prepared = crate::mask::prepare_layers(&self.layers, check)?;
        let colors = crate::effects::prepare_checked(self, &prepared, check)?;
        check()?;
        // Downsampled whole-document previews only; full-resolution export stays on the CPU.
        if target.is_none() && !mask && scale < 1.0 && !self.psd_rgb8_multiply {
            let xs: Vec<_> = (0..w)
                .map(|x| rect[0] + (x as f32 / scale) as i32)
                .collect();
            let ys: Vec<_> = (0..h)
                .map(|y| rect[1] + (y as f32 / scale) as i32)
                .collect();
            if let Some(bytes) = crate::gpu::composite::try_render(self, &colors, &xs, &ys) {
                check()?;
                return Ok((w, h, bytes, rect));
            }
        }
        let target_index = target.and_then(|id| self.layers.iter().position(|l| l.id == id));
        let plan = crate::compositor::Plan::new(self, &prepared, &colors);
        let root = plan.group(None);
        let bytes = crate::render::rgba(w, h, check, |x, y| {
            let sx = rect[0] + (x as f32 / scale) as i32;
            let sy = rect[1] + (y as f32 / scale) as i32;
            let p = if let Some(l) = layer {
                if let Some(image) = target_index.and_then(|i| colors[i].as_ref()) {
                    if crate::effects::canvas_frame(l) {
                        image.get(sx, sy)
                    } else {
                        image.get(sx - l.x, sy - l.y)
                    }
                } else if l.kind == "fill" {
                    if sx >= l.x
                        && sy >= l.y
                        && sx < l.x + l.pixels.width as i32
                        && sy < l.y + l.pixels.height as i32
                    {
                        l.color
                    } else {
                        [0; 4]
                    }
                } else if l.kind == "group" {
                    plan.sample(plan.group(Some(&l.id)), sx, sy)
                } else {
                    l.pixels.get(sx - l.x, sy - l.y)
                }
            } else {
                plan.sample(root, sx, sy)
            };
            p
        })?;
        Ok((w, h, bytes, rect))
    }
    pub fn export_png(&self) -> Result<Vec<u8>, String> {
        crate::production_export::render_document(
            self,
            crate::production_export::Format::Png,
            crate::jpeg_xl::Options::default(),
            &crate::production_export::Control::default(),
        )
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct Scope {
    pub target: Option<String>,
    pub rect: Option<[i32; 4]>,
}
impl Scope {
    pub fn layer(target: &str) -> Self {
        Self {
            target: Some(target.into()),
            rect: None,
        }
    }
    pub fn overlap(&self, other: &Self) -> bool {
        if self.target.is_some() && other.target.is_some() && self.target != other.target {
            return false;
        }
        match (self.rect, other.rect) {
            (Some(a), Some(b)) => a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1],
            _ => true,
        }
    }
}
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Lease {
    pub id: String,
    pub owner: String,
    pub description: String,
    pub scopes: Vec<Scope>,
    pub expires: u64,
}
#[derive(Clone)]
pub struct History {
    pub before: Document,
    pub after: Document,
    pub actor: String,
    pub label: String,
    pub task: Option<String>,
    pub scopes: Vec<Scope>,
    pub gesture: Option<String>,
    /// Revisions compensated by a selective undo; never part of editable sources.
    pub reverted: Vec<u64>,
}
/// Ephemeral visual activity; never stored in PSDs or undo snapshots.
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct AiChange {
    pub serial: u64,
    pub actor: String,
    pub label: String,
    pub tool: String,
    pub scopes: Vec<Scope>,
    pub at: u64,
}
pub struct Engine {
    pub project_id: String,
    pub closed: bool,
    pub(crate) workspace: Option<std::sync::Weak<std::sync::Mutex<crate::workspace::Workspace>>>,
    pub(crate) workspace_owner:
        Option<std::sync::Arc<std::sync::Mutex<crate::workspace::Workspace>>>,
    pub(crate) authority: authority::DocumentAuthority,
    pub leases: Vec<Lease>,
    pub changes: Vec<(u64, Vec<Scope>)>,
    pub saved_revision: u64,
    /// Runtime I/O is shared with workers; never serialized into project sources.
    pub(crate) file_operations: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub feedback: BTreeMap<String, String>,
    pub path: Option<std::path::PathBuf>,
    pub status: runtime_value::RuntimeValue<String>,
    pub file_version: Option<crate::file_safety::Version>,
    pub(crate) file_ownership: Option<std::sync::Arc<crate::file_ownership::Ownership>>,
    pub(crate) file_state_charge: Option<std::sync::Arc<crate::psd::memory36::MetadataCharge>>,
    pub capture_ui: Option<std::path::PathBuf>,
    /// A human-requested native close review; never serialized into documents.
    pub native_close_review: Option<(String, u64)>,
    pub capture_panel: Option<String>,
    pub mcp_clients: BTreeMap<String, u64>,
    pub activity: String,
    pub ai_change: Option<runtime_value::RuntimeValue<AiChange>>,
    ai_serial: u64,
    pub focus_requested: bool,
    pub loading: Option<crate::loading::Control>,
    pub saving: Option<crate::saving::Control>,
    pub brush_library: std::sync::Arc<std::sync::Mutex<crate::brush_library::Library>>,
    pub tool_settings: std::sync::Arc<std::sync::Mutex<crate::tool_settings::Store>>,
    pub filter_library: std::sync::Arc<std::sync::Mutex<crate::filter_library::Library>>,
    pub export_library:
        std::sync::Arc<std::sync::Mutex<crate::production_export::library::Library>>,
    pub(crate) proposals: Vec<crate::collaboration::Proposal>,
    pub(crate) recent_tasks: Vec<crate::collaboration::TaskRecord>,
    pub(crate) code_jobs: Vec<crate::code::Job>,
}
/// Resident rollback only. Runtime services, file ownership and proposal owners
/// stay with their one live Engine throughout a cross-project transaction.
pub(crate) struct ResidentCheckpoint {
    document: Document,
    undo: Vec<History>,
    redo: Vec<History>,
    truncated: BTreeSet<(String, String)>,
    changes: Vec<(u64, Vec<Scope>)>,
    leases: Vec<Lease>,
    ai_change: Option<runtime_value::RuntimeValue<AiChange>>,
    ai_serial: u64,
    status: runtime_value::RuntimeValue<String>,
}
impl ResidentCheckpoint {
    pub(crate) fn restore(self, engine: &mut Engine) -> Result<(), String> {
        let state = engine.authority.resident_state_mut()?;
        state.document = self.document;
        state.undo = self.undo;
        state.redo = self.redo;
        state.truncated_tasks = self.truncated;
        engine.changes = self.changes;
        for lease in &mut engine.leases {
            if let Some(before) = self
                .leases
                .iter()
                .find(|before| before.id == lease.id && before.owner == lease.owner)
            {
                lease.expires = lease.expires.min(before.expires);
            }
        }
        engine.ai_change = self.ai_change;
        engine.ai_serial = self.ai_serial;
        engine.status = if self.status.starts_with("Reserved by")
            && self.leases.iter().any(|before| {
                !engine
                    .leases
                    .iter()
                    .any(|lease| before.id == lease.id && before.owner == lease.owner)
            }) {
            "Reservation released · ready to edit".into()
        } else {
            self.status
        };
        engine.expire();
        Ok(())
    }
}
impl Engine {
    pub(crate) fn resident_checkpoint(&self) -> Result<ResidentCheckpoint, String> {
        let state = self.authority.resident_state()?;
        Ok(ResidentCheckpoint {
            document: state.document.clone(),
            undo: state.undo.clone(),
            redo: state.redo.clone(),
            truncated: state.truncated_tasks.clone(),
            changes: self.changes.clone(),
            leases: self.leases.clone(),
            ai_change: self.ai_change.clone(),
            ai_serial: self.ai_serial,
            status: self.status.clone(),
        })
    }
    /// An explicit resident-only route. Native callers must use a captured
    /// source/metadata view and a prepared worker instead of materializing pixels.
    pub(crate) fn resident(&self) -> Result<&Document, String> {
        self.authority.resident()
    }
    pub fn resident_state(&self) -> Result<&ResidentAuthority, String> {
        self.authority.resident_state()
    }
    pub fn resident_state_mut(&mut self) -> Result<&mut ResidentAuthority, String> {
        self.authority.resident_state_mut()
    }
    pub(crate) fn into_resident_document(self) -> Result<Document, String> {
        Ok(self.authority.into_resident()?.document)
    }
    /// Isolated edit against the captured revision. Keep the live write guards
    /// and preset resolution; the draft needs no earlier history or file owner.
    pub(crate) fn resident_draft(&self) -> Result<Self, String> {
        self.ensure_write_available()?;
        let mut draft = Self::from_document(self.resident()?.clone());
        draft.project_id = self.project_id.clone();
        draft.leases = self.leases.clone();
        draft.saving = self.saving.clone();
        draft.brush_library = self.brush_library.clone();
        draft.filter_library = self.filter_library.clone();
        Ok(draft)
    }
    pub(crate) fn doc_snapshot(&self) -> view::DocumentSnapshot {
        self.authority.snapshot()
    }
    pub(crate) fn history_lengths(&self) -> [usize; 2] {
        self.authority.history_lengths()
    }
    pub(crate) fn history_entries(&self) -> authority::HistoryEntries<'_> {
        self.authority.history_entries()
    }
    /// Capture strong immutable owners under the engine lock; staging and source
    /// I/O must happen only after releasing that lock and the root/registry locks.
    pub fn capture_scene(&self, preview: Option<&Document>) -> Result<scene::CapturedScene, String> {
        if self.closed {
            return Err("Cannot capture a closed project".into());
        }
        if let Some(document) = self.native_document() {
            if preview.is_some() {
                return Err("Native previews require their captured prepared-output envelope".into());
            }
            return scene::CapturedScene::from_native(scene::SceneSnapshot::capture_native(
                &self.project_id,
                document,
                &self.leases,
                self.file_version,
                self.file_ownership.clone(),
            )?);
        }
        self.authority.capture_scene(
            preview,
            self.project_id.clone(),
            self.leases.clone(),
            self.file_version,
            self.file_ownership.clone(),
        )
    }
}
fn num(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(default)
}
fn text<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(default)
}
pub(crate) fn color(v: &Value) -> Pixel {
    let a = v.get("color").and_then(Value::as_array);
    std::array::from_fn(|i| {
        a.and_then(|a| a.get(i))
            .and_then(Value::as_u64)
            .unwrap_or(if i == 3 { 255 } else { 0 })
            .min(255) as u8
    })
}
pub(crate) fn raster_color(command: &Value, depth: u16) -> Result<crate::raster::Pixel16, String> {
    use crate::raster::Pixel16;
    let exact = matches!(
        command["op"].as_str(),
        Some("paint" | "paint.fill" | "shape" | "gradient" | "smudge")
    );
    let mut rgba = if let Some(value) = command.get("color16").filter(|_| exact) {
        if depth != 16 {
            return Err("color16 requires a 16-bit raster target".into());
        }
        <Pixel16 as serde::Deserialize>::deserialize(value)
            .map_err(|_| "color16 must contain four RGBA integers from 0 to 65535")?
    } else {
        color(command).map(|value| u16::from(value) * 257)
    };
    if command["mask"] == true {
        rgba = [rgba[0], rgba[0], rgba[0], rgba[3]];
    }
    Ok(rgba)
}
pub fn rect(v: &Value) -> Option<[i32; 4]> {
    let a = v.as_array()?;
    if a.len() != 4 {
        return None;
    }
    let mut out = [0; 4];
    for i in 0..4 {
        let n = a[i].as_i64()?;
        if !(-100000..=100000).contains(&n) {
            return None;
        }
        out[i] = n as i32;
    }
    (out[2] >= out[0] && out[3] >= out[1]).then_some(out)
}

pub(crate) fn activity_tool(command: &Value) -> &'static str {
    match text(command, "op", "") {
        "paint" if command["erase"] == true => "eraser",
        "paint.clear_selection" => "eraser",
        "paint" => "brush",
        "smudge" => "smudge",
        "clone" => "clone",
        "heal" => "heal",
        "crop" => "selection",
        "canvas.resize" | "image.resize" | "resize" => "scale",
        "liquify.stroke" => "liquify",
        "adjustment.add" => "effects",
        op if op.starts_with("source.") => "layers",
        op if op.starts_with("path.") => "pen",
        "move" => "move",
        "transform" if num(command, "angle", 0.0) != 0.0 => "rotate",
        "transform" => "scale",
        "image.place" | "image.import" | "image.paste" | "image.patch" => "place",
        "layer.update" if command.get("opacity").is_some() => "opacity",
        "fill" | "paint.fill" => "fill",
        op if op.starts_with("effect.") || op.starts_with("filter.") => "effects",
        op if op.starts_with("mask.") => "mask",
        "selection" => "selection",
        op if op.starts_with("selection.") => "selection",
        op if op.starts_with("layer.") => "layers",
        _ => "canvas",
    }
}

#[derive(Default)]
struct SelectionGesture<'a> {
    source: Option<(
        &'a Value,
        Option<[i32; 4]>,
        Option<Vec<[f32; 2]>>,
        Vec<String>,
        Option<crate::selection::Coverage>,
    )>,
}
impl<'a> SelectionGesture<'a> {
    fn prepare(&mut self, doc: &mut Document, command: &'a Value) {
        if !matches!(command["op"].as_str(), Some("move" | "transform"))
            || command["selection_only"] == false
        {
            self.source = None;
            return;
        }
        let target = command["layer"].as_str().unwrap_or("").to_owned();
        if let Some((previous, bounds, polygon, targets, coverage)) = &mut self.source {
            if crate::transform::shares_selection(previous, command) && !targets.contains(&target) {
                doc.selection = *bounds;
                doc.selection_polygon = polygon.clone();
                doc.selection_coverage = coverage.clone();
                targets.push(target);
                return;
            }
        }
        self.source = Some((
            command,
            doc.selection,
            doc.selection_polygon.clone(),
            vec![target],
            doc.selection_coverage.clone(),
        ));
    }
}

impl Engine {
    /// Transient visual feedback uses the same commands as a committed edit, without touching history or shared state.
    pub fn preview_edits(doc: Document, commands: &[Value]) -> Result<Document, String> {
        let mut engine = Self::from_document(doc);
        let commands = engine
            .filter_library
            .lock()
            .unwrap()
            .resolve_commands(commands)?;
        let resolved = engine
            .brush_library
            .lock()
            .unwrap()
            .resolve_commands(&commands)?;
        let mut imported_bytes = 0;
        let mut selection_gesture = SelectionGesture::default();
        let resolved = crate::transform::resolve_commands(resolved)?;
        for command in &resolved {
            selection_gesture.prepare(&mut engine.authority.resident_state_mut()?.document, command);
            if command["op"] == "image.import" {
                engine.validate_source(command)?;
                engine.authority.resident_state_mut()?.document.wand_source = None;
                engine.apply_import(command, None, &mut imported_bytes)?;
            } else {
                engine.apply(command)?;
            }
        }
        engine.authority.resident_state_mut()?.document.ensure_depth();
        crate::compositor::validate_clipping(&engine.authority.resident_state()?.document)?;
        crate::mask::validate_budget(&engine.authority.resident_state()?.document.layers)?;
        crate::effects::validate_budget(&engine.authority.resident_state()?.document)?;
        if engine.authority.resident_state()?.document.bit_depth == 16 {
            crate::depth16::validate_budget(&engine.authority.resident_state()?.document)?;
        }
        crate::effects::invalidate(&mut engine.authority.resident_state_mut()?.document, &commands);
        engine.into_resident_document()
    }
    pub fn new() -> Self {
        Self::from_document(Document::new(1024, 768).unwrap())
    }
    pub(crate) fn from_document(doc: Document) -> Self {
        Self::from_authority(authority::DocumentAuthority::from_resident(doc))
    }
    pub(crate) fn from_native(
        doc: native::NativeDocument,
        history_limit: usize,
    ) -> Result<Self, String> {
        Ok(Self::from_authority(
            authority::DocumentAuthority::from_native(doc, history_limit)?,
        ))
    }
    fn from_authority(authority: authority::DocumentAuthority) -> Self {
        Self {
            project_id: id(),
            closed: false,
            workspace: None,
            workspace_owner: None,
            authority,
            leases: vec![],
            changes: vec![],
            saved_revision: 0,
            file_operations: Default::default(),
            feedback: BTreeMap::new(),
            path: None,
            status: "Ready".into(),
            file_version: None,
            file_ownership: None,
            file_state_charge: None,
            capture_ui: None,
            native_close_review: None,
            capture_panel: None,
            mcp_clients: BTreeMap::new(),
            activity: String::new(),
            ai_change: None,
            ai_serial: 0,
            focus_requested: false,
            loading: None,
            saving: None,
            brush_library: std::sync::Arc::new(std::sync::Mutex::new(
                crate::brush_library::Library::default(),
            )),
            tool_settings: Default::default(),
            export_library: Default::default(),
            filter_library: std::sync::Arc::new(std::sync::Mutex::new(
                crate::filter_library::Library::default(),
            )),
            proposals: vec![],
            recent_tasks: vec![],
            code_jobs: vec![],
        }
    }
    pub fn expire(&mut self) {
        let expired: Vec<_> = self
            .leases
            .iter()
            .filter(|l| l.expires <= now())
            .map(|l| l.id.clone())
            .collect();
        for task in expired {
            self.finish_task(&task, "expired");
        }
        self.mcp_clients.retain(|_, expiry| *expiry > now());
        self.expire_proposals();
    }
    pub fn ensure_open(&self) -> Result<(), String> {
        if self.closed {
            Err("This project is closed; choose an open project ID".into())
        } else {
            Ok(())
        }
    }
    pub(crate) fn ensure_write_available(&self) -> Result<(), String> {
        self.ensure_open()?;
        if let Some(save) = &self.saving {
            save.ensure_editable()?;
        }
        Ok(())
    }
    pub(crate) fn mark_ai(&mut self, actor: &str, label: &str, tool: &str, scopes: Vec<Scope>) {
        if actor == "human" {
            return;
        }
        self.ai_serial += 1;
        self.ai_change = Some(
            AiChange {
                serial: self.ai_serial,
                actor: actor.into(),
                label: label.into(),
                tool: tool.into(),
                scopes,
                at: now(),
            }
            .into(),
        );
    }
    /// Capture metadata owners only; project the result after unlocking Engine.
    pub fn capture_state(&mut self) -> Result<StateCapture, String> {
        self.expire();
        if let Some(document) = self.native_document() {
            let header = RuntimeValue::project(
                &report::header::Header::new(self, document.metadata().revision()),
                document.budget(),
                &|| Ok(()),
            )?;
            let scene = Arc::new(self.capture_scene(None)?.into_native()?);
            return Ok(StateCapture::native(
                scene,
                header,
                self.saving.clone(),
                self.loading.clone(),
            ));
        }
        let mut result = self.state_core()?;
        result["project_id"] = json!(self.project_id);
        result["closed"] = json!(self.closed);
        result["proposals"] = json!(self.proposals.iter().map(|p| p.state()).collect::<Vec<_>>());
        result["task_recovery"] = self.task_recovery();
        Ok(StateCapture::resident(result))
    }
    fn state_core(&mut self) -> Result<Value, String> {
        let doc = self.authority.resident()?;
        Ok(json!({"saving":self.saving.as_ref().map(|c| c.status()),"loading":self.loading.as_ref().map(|c|{let s=c.status();json!({"stage":s.stage,"completed":s.completed,"total":s.total})}),"file_status":self.status,"document":{"id":doc.id,"name":doc.name,"width":doc.width,"height":doc.height,"revision":doc.revision,"bit_depth":doc.bit_depth,"global_light_angle":doc.psd_global_angle.map(u32::from_be_bytes),"color_profile":crate::color_profile::summary(doc.icc_profile.as_deref().map(Vec::as_slice)),"read_only":doc.read_only,"warnings":doc.warnings,"source_structure":doc.source_structure,"source_patterns":doc.source_patterns,"filters":doc.filters,"selection":doc.selection,"selection_polygon":crate::selection::polygon(&doc)},"layers":doc.layers.iter().map(|l|json!({"id":l.id,"name":l.name,"kind":l.kind,"parent":l.parent,"clip_to":l.clip_to,"visible":l.visible,"locked":l.locked,"opacity":l.opacity,"fill_opacity":crate::psd::fill_opacity::value(&l.psd_metadata),"blend_clipped_as_group":crate::psd::clipping::grouped(&l.psd_metadata),"blend_if":crate::psd::blend_ranges::value(&l.psd_metadata),"blend_channels":crate::psd::blend_channels::enabled(&l.psd_metadata),"color_label":crate::psd::layer_color_label(&l.psd_metadata),"blend":l.blend,"bounds":[l.x,l.y,l.x+l.pixels.width as i32,l.y+l.pixels.height as i32],"effects":l.effects,"source":l.source,"path_document_nodes":crate::path::observe(l),"transform_source":crate::retained::observe(&l.pixels),"mask":l.mask.as_ref().map(|m|json!({"enabled":m.enabled,"linked":m.linked,"steps":m.steps.iter().map(|s|json!({"id":s.id,"kind":s.kind,"enabled":s.enabled,"weight":s.weight,"value":s.value,"settings":s.settings,"transform_source":crate::retained::observe(&s.pixels)})).collect::<Vec<_>>()}))})).collect::<Vec<_>>(),"reservations":self.leases,"ai_change":self.ai_change,"dirty":doc.revision!=self.saved_revision}))
    }
    pub fn scope_overlap(&self, a: &Scope, b: &Scope) -> bool {
        view::scope_overlap(self.authority.view(), a, b)
    }
    pub fn reserve(
        &mut self,
        owner: &str,
        description: &str,
        scopes: Vec<Scope>,
    ) -> Result<Lease, String> {
        self.ensure_open()?;
        self.expire();
        for l in &self.leases {
            if l.owner != owner
                && scopes
                    .iter()
                    .any(|s| l.scopes.iter().any(|t| self.scope_overlap(s, t)))
            {
                return Err(format!("Reserved by {}: {}", l.owner, l.description));
            }
        }
        let lease = Lease {
            id: id(),
            owner: owner.into(),
            description: description.into(),
            scopes,
            expires: now() + 300,
        };
        self.activity = description.into();
        self.leases.push(lease.clone());
        Ok(lease)
    }
    pub fn check(&mut self, actor: &str, scopes: &[Scope]) -> Result<(), String> {
        self.ensure_write_available()?;
        self.expire();
        for l in &self.leases {
            if l.owner != actor
                && scopes
                    .iter()
                    .any(|s| l.scopes.iter().any(|t| self.scope_overlap(s, t)))
            {
                return Err(format!(
                    "Reserved by {}: {}. Release the reservation or edit elsewhere.",
                    l.owner, l.description
                ));
            }
        }
        Ok(())
    }
    pub fn scopes(&self, c: &Value) -> Vec<Scope> {
        if let Some(document) = self.native_document() {
            return native::CommandDescriptor::from_normalized(document, document.metadata().revision(), std::slice::from_ref(c), &|| Ok(()))
                .map(|descriptor| descriptor.scopes().to_vec())
                .unwrap_or_else(|_| vec![Scope { target: None, rect: None }]);
        }
        let Ok(doc) = self.authority.resident() else {
            return vec![Scope { target: None, rect: None }];
        };
        let mut scopes = Self::resident_command_scopes(doc, c);
        if doc.selection.is_some()
            && matches!(c["op"].as_str(), Some("move" | "transform"))
            && c["selection_only"] != false
        {
            if let Some(layer) = c["layer"]
                .as_str()
                .and_then(|id| doc.layers.iter().find(|layer| layer.id == id))
            {
                if let Some(mask) = &layer.mask {
                    for scope in &mut scopes {
                        if let Some(area) = &mut scope.rect {
                            *area = reframe_scope::widen(
                                *area,
                                [layer.x, layer.y],
                                [layer.pixels.width, layer.pixels.height],
                                mask.linked,
                                mask.steps
                                    .iter()
                                    .map(|step| [step.pixels.width, step.pixels.height]),
                            );
                        }
                    }
                }
            }
        }
        if doc.selection.is_some()
            && (c["op"] == "paint.clear_selection"
                || (matches!(c["op"].as_str(), Some("move" | "transform"))
                    && c["selection_only"] != false))
        {
            scopes.push(Scope::layer("@selection"));
        }
        if c["op"] == "mask.from_selection"
            || (c["op"] == "mask.add" && doc.selection.is_some())
        {
            scopes.push(Scope::layer("@selection"));
        }
        scopes
    }
    pub(crate) fn resident_command_scopes(doc: &Document, c: &Value) -> Vec<Scope> {
        let op = text(c, "op", "");
        if op.starts_with("filter.") || document_settings::may_promote(doc.bit_depth, c)
            || document_settings::may_initialize_global_light(doc.psd_global_angle, c)
        {
            return vec![Scope {
                target: None,
                rect: None,
            }];
        }
        if matches!(op, "image.paste" | "layer.paste") {
            let layer = |i: usize| {
                let l = &doc.layers[i];
                crate::clipboard::paste::Layer {
                    id: &l.id,
                    parent: l.parent.as_deref(),
                    kind: &l.kind,
                    clip_to: l.clip_to.as_deref(),
                    locked: l.locked,
                }
            };
            let plan = if op == "layer.paste" {
                crate::clipboard::paste::Plan::layers(
                    text(c, "layer", ""),
                    doc.layers.len(),
                    1,
                    layer,
                )
            } else {
                crate::clipboard::paste::Plan::new(c, doc.layers.len(), layer)
            };
            return plan
                .map(|plan| plan.scopes(doc.layers.len(), layer))
                .unwrap_or_else(|_| {
                    vec![Scope {
                        target: None,
                        rect: None,
                    }]
                });
        }
        let target = c.get("layer").and_then(Value::as_str).map(String::from);
        if matches!(op, "path.to_selection" | "path.to_mask") {
            let mut scopes = vec![Scope::layer(c["path"].as_str().unwrap_or("@invalid_path"))];
            if op == "path.to_selection" {
                scopes.push(Scope::layer("@selection"));
            } else {
                scopes.push(Scope { target, rect: None });
            }
            return scopes;
        }
        if matches!(op, "effect.bake" | "mask.step.bake") {
            return vec![Scope { target, rect: None }];
        }
        if visibility_only(c) {
            return vec![Scope {
                target: target.map(|t| format!("@visibility:{t}")),
                rect: None,
            }];
        }
        if op == "selection" || op.starts_with("selection.") {
            return vec![Scope {
                target: Some("@selection".into()),
                rect: None,
            }];
        }
        if ["move", "transform"].contains(&op)
            && target.as_ref().is_some_and(|t| {
                doc
                    .layers
                    .iter()
                    .any(|l| l.id == *t && l.kind == "group")
            })
        {
            return vec![Scope { target, rect: None }];
        }
        if op == "layer.merge" {
            return crate::merge::requested(c)
                .and_then(|ids| crate::merge::resolve(&doc, &ids))
                .map(|ids| ids.into_iter().map(|id| Scope::layer(&id)).collect())
                .unwrap_or_else(|_| {
                    vec![Scope {
                        target: None,
                        rect: None,
                    }]
                });
        }
        if ["layer.add", "image.import", "source.add", "path.create"].contains(&op) {
            return c["parent"]
                .as_str()
                .map(|p| {
                    vec![Scope {
                        target: Some(p.into()),
                        rect: None,
                    }]
                })
                .unwrap_or_default();
        }
        if [
            "layer.reorder",
            "layer.delete",
            "layer.merge",
            "layer.duplicate",
            "layer.duplicate_selection",
            "layer.paste",
            "group.create_selected",
            "adjustment.add",
            "layer.clip",
            "layer.parent",
            "image.paste",
            "image.place",
        ]
        .contains(&op)
            && !(op == "image.place" && c["new_layer"] == false)
        {
            let mut targets = vec![];
            if let Some(t) = target.clone() {
                targets.push(t);
            }
            if let Some(ids) = c["layers"].as_array() {
                targets.extend(ids.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            for key in ["parent", "clip_to", "base"] {
                if let Some(t) = c[key].as_str() {
                    targets.push(t.into());
                }
            }
            // Clip changes can alter their old base's composited unit.
            if op == "layer.clip" {
                if let Some(t) = target
                    .as_ref()
                    .and_then(|t| doc.layers.iter().find(|l| l.id == *t))
                    .and_then(|l| l.clip_to.clone())
                {
                    targets.push(t);
                }
            }
            if op == "adjustment.add" && targets.is_empty() {
                return vec![Scope {
                    target: None,
                    rect: None,
                }];
            }
            targets.sort();
            targets.dedup();
            return targets
                .into_iter()
                .map(|t| Scope {
                    target: Some(t),
                    rect: None,
                })
                .collect();
        }
        if op == "fill"
            && c["mask"] != true
            && target.as_deref().is_some_and(|id| {
                doc.layers.iter().any(|layer| {
                    layer.id == id
                        && (layer.kind == "group"
                            || (layer.kind == "fill" && doc.selection.is_none()))
                })
            })
        {
            return vec![Scope { target, rect: None }];
        }
        if matches!(op, "paint.fill" | "paint.clear_selection") {
            let layer = target
                .as_deref()
                .and_then(|id| doc.layers.iter().find(|l| l.id == id));
            if layer.is_some_and(|l| l.kind == "group") && c["mask"] != true {
                return vec![Scope { target, rect: None }];
            }
            let mut area = if op == "paint.clear_selection" {
                doc.selection
            } else {
                c.get("rect").and_then(rect).or(doc.selection)
            }
            .unwrap_or([0, 0, doc.width as i32, doc.height as i32]);
            if let Some(selection) = doc.selection {
                area = [
                    area[0].max(selection[0]),
                    area[1].max(selection[1]),
                    area[2].min(selection[2]),
                    area[3].min(selection[3]),
                ];
            }
            return vec![Scope {
                target,
                rect: Some(area),
            }];
        }
        if op == "image.place" && c["new_layer"] == false {
            return vec![Scope { target, rect: None }];
        }
        if [
            "new",
            "crop",
            "resize",
            "canvas.resize",
            "image.resize",
            "document.settings",
            "document.global_light",
            "layer.add",
            "layer.reorder",
            "layer.delete",
            "layer.merge",
            "layer.duplicate",
            "layer.duplicate_selection",
            "layer.paste",
            "group.create_selected",
            "adjustment.add",
            "layer.clip",
            "layer.parent",
            "image.import",
            "image.paste",
            "image.place",
        ]
        .contains(&op)
        {
            return vec![Scope {
                target: None,
                rect: None,
            }];
        }
        let mut area = if ["paint", "smudge", "clone", "heal", "liquify.stroke"].contains(&op) {
            c.get("points").and_then(Value::as_array).map(|p| {
                let r = if op == "liquify.stroke" {
                    crate::liquify::command::radius(c)
                } else {
                    num(c, "radius", 10.0)
                } as f32
                    + 2.0;
                let mut a = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                for point in p {
                    if let Some(pair) = point.as_array() {
                        let x = pair.first().and_then(Value::as_f64).unwrap_or(0.0) as f32;
                        let y = pair.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                        a = [
                            a[0].min(x - r),
                            a[1].min(y - r),
                            a[2].max(x + r),
                            a[3].max(y + r),
                        ];
                    }
                }
                [
                    a[0].floor() as i32,
                    a[1].floor() as i32,
                    a[2].ceil() as i32,
                    a[3].ceil() as i32,
                ]
            })
        } else {
            None
        };
        if ["shape", "gradient", "fill", "adjust", "pixels.replace"].contains(&op) {
            area = c["rect"]
                .as_array()
                .and_then(|_| rect(&c["rect"]))
                .or(doc.selection);
        }
        if op == "transform" && c.get("warp").is_some() {
            match crate::warp::parse(c) {
                Ok(Some(_)) => {
                    let area = doc
                        .selection
                        .filter(|_| c["selection_only"] != false)
                        .and_then(|b| {
                            crate::warp::CommandMap::new(c, [0., 0.])
                                .and_then(|m| m.bounds(b.map(|v| v as f64)))
                                .ok()
                                .map(|p| {
                                    [
                                        b[0].min(p[0].floor() as i32),
                                        b[1].min(p[1].floor() as i32),
                                        b[2].max(p[2].ceil() as i32),
                                        b[3].max(p[3].ceil() as i32),
                                    ]
                                })
                        });
                    return vec![Scope { target, rect: area }];
                }
                Err(_) => return vec![Scope { target, rect: None }],
                Ok(None) => {}
            }
        }
        if op == "transform" && c.get("projective").is_some() {
            match crate::projective::combined(c) {
                Ok(Some(map)) => {
                    let area = doc
                        .selection
                        .filter(|_| c["selection_only"] != false)
                        .and_then(|b| {
                            map.bounds(b.map(|v| v as f64)).ok().map(|p| {
                                [
                                    b[0].min(p[0].floor() as i32),
                                    b[1].min(p[1].floor() as i32),
                                    b[2].max(p[2].ceil() as i32),
                                    b[3].max(p[3].ceil() as i32),
                                ]
                            })
                        });
                    return vec![Scope { target, rect: area }];
                }
                Err(_) => return vec![Scope { target, rect: None }],
                Ok(None) => {}
            }
        }
        if ["move", "transform"].contains(&op) && c["selection_only"] != false {
            if let Some(b) = doc.selection {
                let moving = op == "move";
                let [dx, dy] = if moving {
                    [num(c, "dx", 0.).round(), num(c, "dy", 0.).round()]
                } else {
                    crate::transform::translation(c).unwrap_or([0., 0.])
                };
                let angle = if moving {
                    0.
                } else {
                    num(c, "angle", 0.).to_radians()
                };
                let (sin, cos) = angle.sin_cos();
                let (sx, sy) = if moving {
                    (1., 1.)
                } else {
                    crate::transform::scales(c).unwrap_or((1., 1.))
                };
                let px = c["pivot"][0]
                    .as_f64()
                    .unwrap_or((b[0] as f64 + b[2] as f64) / 2.);
                let py = c["pivot"][1]
                    .as_f64()
                    .unwrap_or((b[1] as f64 + b[3] as f64) / 2.);
                let shear = crate::transform::selected_shear(c).unwrap_or([0., 0.]);
                let linear = [cos * sx, sin * sx, -sin * sy, cos * sy];
                let skew = crate::transform::skewed(linear, c)
                    .ok()
                    .filter(|_| shear != [0., 0.]);
                let mut out = b;
                for p in crate::selection::rectangle(b) {
                    let (x, y) = if let Some([a, bb, cc, d]) = skew {
                        (
                            px + a * (p[0] as f64 - px) + cc * (p[1] as f64 - py) + dx,
                            py + bb * (p[0] as f64 - px) + d * (p[1] as f64 - py) + dy,
                        )
                    } else {
                        (
                            px + (p[0] as f64 - px) * sx * cos - (p[1] as f64 - py) * sy * sin + dx,
                            py + (p[0] as f64 - px) * sx * sin + (p[1] as f64 - py) * sy * cos + dy,
                        )
                    };
                    out = [
                        out[0].min(x.floor() as i32),
                        out[1].min(y.floor() as i32),
                        out[2].max(x.ceil() as i32),
                        out[3].max(y.ceil() as i32),
                    ];
                }
                if shear != [0., 0.] || c["fractional_translation"] == true {
                    let (mut sin, mut cos) = (num(c, "angle", 0.) as f32).to_radians().sin_cos();
                    if sin.abs() < 0.000001 {
                        sin = 0.;
                    }
                    if cos.abs() < 0.000001 {
                        cos = 0.;
                    }
                    let rectangle = crate::selection::rectangle(b);
                    let covered = doc
                        .selection_coverage
                        .as_ref()
                        .is_some_and(|coverage| coverage.bounds == b);
                    let points = if covered {
                        rectangle.as_slice()
                    } else {
                        crate::selection::polygon(&doc).unwrap_or(&rectangle)
                    };
                    for &p in points {
                        let [x, y] = crate::transform::selected_point(
                            p,
                            [px as f32, py as f32],
                            [dx as f32, dy as f32],
                            [sx as f32, sy as f32],
                            [sin, cos],
                            shear,
                        );
                        out = [
                            out[0].min(x.floor() as i32),
                            out[1].min(y.floor() as i32),
                            out[2].max(x.ceil() as i32),
                            out[3].max(y.ceil() as i32),
                        ];
                    }
                }
                area = Some(out);
            }
        }
        let area = area.map(|a| {
            if ["move", "transform"].contains(&op) {
                return a;
            }
            doc.selection.map_or(a, |b| {
                [
                    a[0].max(b[0]),
                    a[1].max(b[1]),
                    a[2].min(b[2]),
                    a[3].min(b[3]),
                ]
            })
        });
        vec![Scope { target, rect: area }]
    }
    pub fn edit(
        &mut self,
        actor: &str,
        commands: &[Value],
        expected: Option<u64>,
        task: Option<&str>,
        label: &str,
    ) -> Result<Value, String> {
        self.edit_with_gesture(actor, commands, expected, task, label, None)
    }
    pub fn edit_with_gesture(
        &mut self,
        actor: &str,
        commands: &[Value],
        expected: Option<u64>,
        task: Option<&str>,
        label: &str,
        gesture: Option<&str>,
    ) -> Result<Value, String> {
        self.edit_transaction(
            actor, commands, expected, task, label, gesture, None, None, None, None, None, None,
        )
    }
    pub(crate) fn edit_prepared_merge(
        &mut self,
        actor: &str,
        command: Value,
        expected: Option<u64>,
        task: Option<&str>,
        prepared: crate::merge::Prepared,
    ) -> Result<Value, String> {
        self.edit_transaction(
            actor,
            &[command],
            expected,
            task,
            "Merge layers",
            None,
            Some(prepared),
            None,
            None,
            None,
            None,
            None,
        )
    }
    /// Typed copy-on-write paste avoids serializing raster data through JSON.
    pub fn paste_layers(
        &mut self,
        actor: &str,
        snapshot: &crate::layer_clipboard::Layers,
        target: &str,
        expected: Option<u64>,
        task: Option<&str>,
    ) -> Result<Value, String> {
        self.edit_transaction(
            actor,
            &[json!({"op":"layer.paste","layer":target})],
            expected,
            task,
            "Paste layers",
            None,
            None,
            Some(snapshot.clone()),
            None,
            None,
            None,
            None,
        )
    }
    pub(crate) fn paste_resident_images(
        &mut self,
        actor: &str,
        commands: &[Value],
        revision: u64,
        pixels: Vec<Raster>,
    ) -> Result<Value, String> {
        if commands.len() != pixels.len()
            || commands.iter().any(|command| command["op"] != "image.paste")
        {
            return Err("Prepared clipboard pixels require one image.paste command per image".into());
        }
        self.edit_transaction(
            actor, commands, Some(revision), None, "Paste images", None,
            None, None, None, None, None, Some(pixels.into_iter()),
        )
    }
    pub(crate) fn commit_proposal(
        &mut self,
        proposal: &crate::collaboration::Proposal,
    ) -> Result<Value, String> {
        let doc = proposal
            .draft
            .as_ref()
            .ok_or("This proposal is no longer available")?;
        if proposal.document != self.authority.resident_state()?.document.id || proposal.revision != self.authority.resident_state()?.document.revision {
            return Err("Proposal source changed; request a fresh proposal".into());
        }
        self.edit_transaction(
            &proposal.actor,
            &proposal.commands,
            Some(proposal.revision),
            proposal.task.as_deref(),
            &proposal.label,
            None,
            None,
            None,
            Some((**doc).clone()),
            None,
            None,
            None,
        )
    }
    pub(crate) fn commit_code(
        &mut self,
        actor: &str,
        commands: &[Value],
        task: &str,
        label: &str,
        doc: Document,
    ) -> Result<Value, String> {
        self.edit_transaction(
            actor,
            commands,
            Some(self.authority.resident_state()?.document.revision),
            Some(task),
            label,
            None,
            None,
            None,
            Some(doc),
            None,
            None,
            None,
        )
    }
    /// Decode outside the engine lock, then atomically commit the captured pixels.
    pub(crate) fn import_images(
        &mut self,
        commands: &[Value],
        pixels: Vec<crate::image_import::Prepared>,
        revision: u64,
    ) -> Result<Value, String> {
        if commands.len() != pixels.len() || commands.iter().any(|c| c["op"] != "image.import") {
            return Err("Invalid prepared image batch".into());
        }
        let bytes: usize = pixels.iter().map(|p| p.pixels.stored_bytes()).sum();
        if bytes > crate::image_import::BUDGET {
            return Err("Image batch exceeds 256 MiB".into());
        }
        self.edit_transaction(
            "human",
            commands,
            Some(revision),
            None,
            "Import images",
            None,
            None,
            None,
            None,
            Some(pixels.into()),
            None,
            None,
        )
    }
    fn validate_source(&self, c: &Value) -> Result<(), String> {
        let document = self.authority.view();
        if c.get("source_revision")
            .is_some_and(|r| r.as_u64() != Some(document.revision()))
            || c.get("document_id")
                .is_some_and(|id| id.as_str() != Some(document.id()))
        {
            return Err(
                "The source project changed. Review it again before applying this result.".into(),
            );
        }
        Ok(())
    }
    fn apply_import(
        &mut self,
        c: &Value,
        pixels: Option<crate::image_import::Prepared>,
        imported_bytes: &mut usize,
    ) -> Result<(), String> {
        let pixels = match pixels {
            Some(pixels) => pixels,
            None => crate::image_import::prepare_file(
                std::path::Path::new(c["path"].as_str().ok_or("Missing image path")?),
                c,
                crate::image_import::BUDGET - *imported_bytes,
            )?,
        };
        *imported_bytes = imported_bytes
            .checked_add(pixels.pixels.stored_bytes())
            .ok_or("Image batch exceeds 256 MiB")?;
        if *imported_bytes > crate::image_import::BUDGET {
            return Err("Image batch exceeds 256 MiB".into());
        }
        crate::image_import::apply(&mut self.authority.resident_state_mut()?.document, c, pixels)
    }
    fn edit_transaction(
        &mut self,
        actor: &str,
        commands: &[Value],
        expected: Option<u64>,
        task: Option<&str>,
        label: &str,
        gesture: Option<&str>,
        mut prepared: Option<crate::merge::Prepared>,
        mut clipboard: Option<crate::layer_clipboard::Layers>,
        prepared_doc: Option<Document>,
        mut imports: Option<std::collections::VecDeque<crate::image_import::Prepared>>,
        pixels: Option<backed::PreparedPixels>,
        mut pasted_images: Option<std::vec::IntoIter<Raster>>,
    ) -> Result<Value, String> {
        self.ensure_write_available()?;
        if self.authority.resident_state()?.document.read_only {
            return Err("This PSD is read-only. Create a compatible copy first.".into());
        }
        if commands.is_empty() || commands.len() > 100 {
            return Err("A batch must contain 1–100 commands".into());
        }
        let commands = self
            .filter_library
            .lock()
            .unwrap()
            .resolve_commands(commands)?;
        let resolved = self
            .brush_library
            .lock()
            .unwrap()
            .resolve_commands(&commands)?;
        let resolved = crate::transform::resolve_commands(resolved)?;
        let commands = resolved.as_slice();
        if let Some(pixels) = &pixels {
            if commands.len() != 1 {
                return Err("Prepared pixels require exactly one command".into());
            }
            pixels.validate(&self.authority.resident_state()?.document, &commands[0])?;
        }
        let before = self.authority.resident_state()?.document.clone();
        let mut scopes = vec![];
        for c in commands {
            scopes.extend(self.scopes(c));
        }
        self.check(actor, &scopes)?;
        if let Some(rev) = expected {
            if rev > self.authority.resident_state()?.document.revision {
                return Err("Revision is from the future".into());
            }
            if rev < self.authority.resident_state()?.document.revision {
                let earliest = self
                    .changes
                    .first()
                    .map(|x| x.0)
                    .unwrap_or(self.authority.resident_state()?.document.revision);
                if rev + 1 < earliest
                    || self.changes.iter().filter(|x| x.0 > rev).any(|(_, other)| {
                        scopes
                            .iter()
                            .any(|s| other.iter().any(|t| self.scope_overlap(s, t)))
                    })
                {
                    return Err(
                        "Conflicting document change. Observe the target again before editing."
                            .into(),
                    );
                }
            }
        }
        if let Some(task) = task {
            if !self.leases.iter().any(|l| l.id == task && l.owner == actor) {
                return Err("Task reservation expired or was released".into());
            }
        }
        let mut pasted_roots = None;
        let mut imported_bytes = 0usize;
        let mut selection_gesture = SelectionGesture::default();
        if let Some(mut doc) = prepared_doc {
            if doc.id != before.id || doc.revision != before.revision + 1 {
                return Err("Proposal source changed; request a fresh proposal".into());
            }
            doc.name = self.authority.resident_state()?.document.name.clone();
            self.authority.resident_state_mut()?.document = doc;
        } else {
            for c in commands {
                selection_gesture.prepare(&mut self.authority.resident_state_mut()?.document, c);
                if let Err(error) = self.validate_source(c) {
                    self.authority.resident_state_mut()?.document = before;
                    return Err(error);
                }
                if c["op"] != "selection.wand_update"
                    && !(c["op"] == "selection" && c["kind"] == "wand")
                {
                    self.authority.resident_state_mut()?.document.wand_source = None;
                }
                let applied = if c["op"] == "image.import" {
                    let pixels = imports
                        .as_mut()
                        .map(|items| {
                            items
                                .pop_front()
                                .ok_or_else(|| "Missing prepared image".to_owned())
                        })
                        .transpose();
                    pixels.and_then(|pixels| self.apply_import(c, pixels, &mut imported_bytes))
                } else if let Some(images) = &mut pasted_images {
                    crate::clipboard::paste_with_pixels(
                        &mut self.authority.resident_state_mut()?.document, c,
                        || images.next().ok_or_else(|| "Missing prepared clipboard pixels".into()),
                    )
                } else if c["op"] == "layer.paste" {
                    if let Some(snapshot) = clipboard.take() {
                        crate::layer_clipboard::paste(
                            &mut self.authority.resident_state_mut()?.document,
                            &snapshot,
                            text(c, "layer", ""),
                        )
                        .map(|roots| {
                            pasted_roots = Some(roots);
                        })
                    } else {
                        Err("Use the typed layer clipboard to paste layers".into())
                    }
                } else if c["op"] == "layer.merge" {
                    if let Some(prepared) = prepared.take() {
                        prepared.apply(&mut self.authority.resident_state_mut()?.document)
                    } else {
                        self.apply(c)
                    }
                } else if let Some(pixels) = &pixels {
                    self.apply_pixels(c, Some(pixels))
                } else {
                    self.apply(c)
                };
                if let Err(err) = applied {
                    self.authority.resident_state_mut()?.document = before;
                    return Err(err);
                }
            }
        }
        self.authority.resident_state_mut()?.document.ensure_depth();
        let bytes: usize = self
            .authority.resident_state()?.document
            .layers
            .iter()
            .map(|l| {
                l.pixels.stored_bytes()
                    + l.mask
                        .as_ref()
                        .map(|m| {
                            m.steps
                                .iter()
                                .map(|s| s.pixels.stored_bytes())
                                .sum::<usize>()
                        })
                        .unwrap_or(0)
            })
            .sum();
        if bytes > 512 * 1024 * 1024 {
            self.authority.resident_state_mut()?.document = before;
            return Err(
                "Document exceeded the initial 512 MiB raster budget; edit was rolled back.".into(),
            );
        }
        if let Err(error) = crate::mask::validate_budget(&self.authority.resident_state()?.document.layers) {
            self.authority.resident_state_mut()?.document = before;
            return Err(error);
        }
        if let Err(error) = crate::effects::validate_budget(&self.authority.resident_state()?.document) {
            self.authority.resident_state_mut()?.document = before;
            return Err(error);
        }
        if let Err(error) = crate::compositor::validate_clipping(&self.authority.resident_state()?.document) {
            self.authority.resident_state_mut()?.document = before;
            return Err(error);
        }
        if self.authority.resident_state()?.document.bit_depth == 16 {
            if let Err(error) = crate::depth16::validate_budget(&self.authority.resident_state()?.document) {
                self.authority.resident_state_mut()?.document = before;
                return Err(error);
            }
        }
        if let Some(pixels) = &pixels {
            if let Err(error) = pixels.check_cancelled() {
                self.authority.resident_state_mut()?.document = before;
                return Err(error);
            }
        }
        crate::effects::invalidate(&mut self.authority.resident_state_mut()?.document, commands);
        self.authority.resident_state_mut()?.document.revision = before.revision + 1;
        let revision = self.authority.resident_state()?.document.revision;
        let created: Vec<String> = self
            .authority.resident_state()?.document
            .layers
            .iter()
            .filter(|l| !before.layers.iter().any(|old| old.id == l.id))
            .map(|l| l.id.clone())
            .collect();
        let document = &self.authority.resident_state()?.document;
        let created_roots = pasted_roots.unwrap_or_else(|| {
            document
                .layers
                .iter()
                .filter(|l| {
                    created.contains(&l.id)
                        && !l.parent.as_ref().is_some_and(|p| created.contains(p))
                })
                .map(|l| l.id.clone())
                .collect::<Vec<_>>()
        });
        let visual_scopes: Vec<Scope> = commands
            .iter()
            .filter_map(|command| {
                command
                    .get("layer")
                    .and_then(Value::as_str)
                    .map(|target| Scope {
                        target: Some(target.into()),
                        rect: command
                            .get("rect")
                            .and_then(rect)
                            .or_else(|| self.scopes(command).first().and_then(|scope| scope.rect)),
                    })
            })
            .chain(created.iter().map(|target| Scope::layer(target)))
            .collect();
        let visual_scopes = if visual_scopes.is_empty() {
            scopes.clone()
        } else {
            visual_scopes
        };
        let command = &commands[commands.len() - 1];
        let tool = activity_tool(command);
        self.mark_ai(actor, label, tool, visual_scopes);
        let continuous = gesture.is_some()
            && self.authority.resident_state()?.undo.last().is_some_and(|h| {
                h.actor == actor
                    && h.task.as_deref() == task
                    && h.gesture.as_deref() == gesture
                    && h.after.revision == before.revision
                    && h.after.id == before.id
            });
        let after = self.authority.resident_state()?.document.clone();
        if continuous {
            let history = self.authority.resident_state_mut()?.undo.last_mut().unwrap();
            history.after = after;
            history.scopes.extend(scopes.clone());
            history.scopes.dedup();
        } else {
            self.authority.resident_state_mut()?.undo.push(History {
                before,
                after,
                actor: actor.into(),
                label: label.into(),
                task: task.map(String::from),
                scopes: scopes.clone(),
                gesture: gesture.map(String::from),
                reverted: vec![],
            });
        }
        self.trim_history();
        self.authority.resident_state_mut()?.redo.clear();
        self.changes.push((revision, scopes));
        if self.changes.len() > 200 {
            self.changes.remove(0);
        }
        for l in &mut self.leases {
            if l.owner == actor {
                l.expires = now() + 300;
            }
        }
        self.status = label.into();
        Ok(
            json!({"revision":revision,"applied":commands.len(),"created":created,"created_roots":created_roots,"layers":self.authority.resident_state()?.document.layers.iter().map(|l|json!({"id":l.id,"name":l.name})).collect::<Vec<_>>()}),
        )
    }
    fn apply(&mut self, c: &Value) -> Result<(), String> {
        self.apply_pixels(c, None)
    }
    fn apply_pixels(
        &mut self,
        c: &Value,
        pixels: Option<&backed::PreparedPixels>,
    ) -> Result<(), String> {
        if c["op"] != "selection.wand_update" && !(c["op"] == "selection" && c["kind"] == "wand") {
            self.authority.resident_state_mut()?.document.wand_source = None;
        }
        // Reframe before taking native-original and soft-selection baselines. The
        // outer transaction still owns the complete pre-gesture undo snapshot.
        self.authority.resident_state_mut()?.document.ensure_depth();
        crate::fill::prepare_visual_baseline(&mut self.authority.resident_state_mut()?.document, c)?;
        crate::edit_bounds::prepare(&mut self.authority.resident_state_mut()?.document, c)?;
        let document = &self.authority.resident_state()?.document;
        let before = crate::retained::has_originals(document).then(|| document.clone());
        self.apply_retained(c, pixels)?;
        if let Some(before) = before {
            let whole_transform = matches!(c["op"].as_str(), Some("move" | "transform"))
                && (before.selection.is_none() || c["selection_only"] == false);
            if !whole_transform {
                let after = &mut self.authority.resident_state_mut()?.document;
                crate::retained::reconcile(&before, after);
                crate::edit_bounds::mask_original::reconcile_resident(&before, after, c)?;
            }
        }
        Ok(())
    }
    fn apply_retained(
        &mut self,
        c: &Value,
        pixels: Option<&backed::PreparedPixels>,
    ) -> Result<(), String> {
        self.validate_source(c)?;
        if matches!(
            c["op"].as_str(),
            Some(
                "paint"
                    | "smudge"
                    | "clone"
                    | "heal"
                    | "shape"
                    | "gradient"
                    | "fill"
                    | "paint.fill"
                    | "adjust"
                    | "image.patch"
                    | "pixels.replace"
            )
        ) {
            let document = &self.authority.resident_state()?.document;
            if let Some(coverage) = document
                .selection_coverage
                .clone()
                .filter(|m| document.selection == Some(m.bounds))
            {
                let before = self.authority.resident_state()?.document.clone();
                self.apply_inner(c, pixels)?;
                crate::selection::restrict_document(
                    &mut self.authority.resident_state_mut()?.document,
                    &before,
                    &coverage,
                );
                return Ok(());
            }
        }
        self.apply_inner(c, pixels)
    }
    fn apply_inner(
        &mut self,
        c: &Value,
        pixels: Option<&backed::PreparedPixels>,
    ) -> Result<(), String> {
        let mut linked;
        let c = if matches!(c["op"].as_str(), Some("move" | "transform"))
            && c["mask"] == true
            && self.authority.resident_state()?.document.layers.iter().any(|layer| {
                c["layer"].as_str() == Some(layer.id.as_str())
                    && layer.mask.as_ref().is_some_and(|mask| mask.linked)
            }) {
            linked = c.clone();
            linked["mask"] = json!(false);
            &linked
        } else {
            c
        };
        self.authority.resident_state_mut()?.document.ensure_depth();
        let op = text(c, "op", "");
        let target = text(c, "layer", "");
        if op.starts_with("filter.") {
            return crate::filters::apply(&mut self.authority.resident_state_mut()?.document, c);
        }
        if op == "source.add" {
            return crate::source::add(&mut self.authority.resident_state_mut()?.document, c);
        }
        if op == "path.create" {
            return crate::path::create(&mut self.authority.resident_state_mut()?.document, c);
        }
        if op == "path.to_selection" {
            return crate::path::to_selection(&mut self.authority.resident_state_mut()?.document, c);
        }
        if ["crop", "canvas.resize", "image.resize", "resize"].contains(&op) {
            return crate::geometry::apply(&mut self.authority.resident_state_mut()?.document, c);
        }
        if c.get("rect")
            .is_some_and(|r| !r.is_null() && rect(r).is_none())
        {
            return Err("Invalid rectangle coordinates".into());
        }
        if op == "document.global_light" {
            let angle = document_settings::global_light_angle(c)?;
            let document = &mut self.authority.resident_state_mut()?.document;
            if document.psd_global_angle != Some(angle.to_be_bytes()) {
                for effect in document.layers.iter_mut().flat_map(|layer| &mut layer.effects).chain(&mut document.filters) {
                    if let Some(light) = crate::effects::styles::GlobalLight::for_document(&effect.settings, document.psd_global_angle) {
                        light.set_angle(Arc::make_mut(&mut effect.settings), angle);
                    }
                }
                document.psd_global_angle = Some(angle.to_be_bytes());
            }
            return Ok(());
        }
        if op == "document.settings" {
            let settings = document_settings::Settings::new(
                [self.authority.resident_state()?.document.width, self.authority.resident_state()?.document.height],
                self.authority.resident_state()?.document.bit_depth,
                c,
                crate::raster::CanvasLimits::Resident,
            )?;
            let [w, h] = settings.size;
            let depth = settings.depth;
            if depth == 8
                && self.authority.resident_state()?.document.layers.iter().any(|l| {
                    l.effects.iter().any(|e| {
                        crate::effects::patterns::from_settings(&e.kind, &e.settings)
                            .ok()
                            .flatten()
                            .is_some_and(|b| b.depth == 16)
                    })
                })
            {
                return Err(document_settings::PATTERN_PRECISION.into());
            }
            if depth != self.authority.resident_state()?.document.bit_depth {
                for layer in &mut self.authority.resident_state_mut()?.document.layers {
                    layer.pixels.retained = None;
                    layer.pixels.convert_depth(depth as u16);
                    if let Some(mask) = &mut layer.mask {
                        mask.cache_key = id();
                        for step in &mut mask.steps {
                            step.pixels.retained = None;
                            step.pixels.convert_depth(depth as u16);
                        }
                    }
                }
            }
            self.authority.resident_state_mut()?.document.width = w as u32;
            self.authority.resident_state_mut()?.document.height = h as u32;
            self.authority.resident_state_mut()?.document.bit_depth = depth as u16;
            self.authority.resident_state_mut()?.document.selection = None;
            self.authority.resident_state_mut()?.document.selection_polygon = None;
            self.authority.resident_state_mut()?.document.selection_coverage = None;
            self.authority.resident_state_mut()?.document.selection_previous = None;
            return Ok(());
        }
        if op == "adjustment.add" {
            crate::adjustment::add(&mut self.authority.resident_state_mut()?.document, c)?;
            return Ok(());
        }
        if op == "layer.duplicate_selection" {
            crate::layer_clipboard::duplicate_selection(&mut self.authority.resident_state_mut()?.document, c)?;
            return Ok(());
        }
        if op == "layer.duplicate" {
            let ids = crate::grouping::requested(c)?;
            crate::layer_clipboard::duplicate_with_ids(
                &mut self.authority.resident_state_mut()?.document,
                &ids,
                c.get("root_ids"),
            )?;
            return Ok(());
        }
        if op == "group.create_selected" {
            let ids = crate::grouping::requested(c)?;
            crate::grouping::create(&mut self.authority.resident_state_mut()?.document, &ids, text(c, "name", "Folder"))?;
            return Ok(());
        }
        if op == "layer.merge" {
            // Earlier commands in the same batch may have changed effect sources.
            crate::effects::invalidate(&mut self.authority.resident_state_mut()?.document, &[json!({"op":"layer.merge"})]);
            let ids = crate::merge::requested(c)?;
            let prepared = crate::merge::prepare(&self.authority.resident_state()?.document, &ids, c["name"].as_str())?;
            return prepared.apply(&mut self.authority.resident_state_mut()?.document);
        }
        if op == "image.paste" {
            return crate::clipboard::paste(&mut self.authority.resident_state_mut()?.document, c);
        }
        if op == "image.place" {
            return crate::placement::place(&mut self.authority.resident_state_mut()?.document, c);
        }
        if op == "image.import" {
            return self.apply_import(c, None, &mut 0);
        }
        if op == "new" {
            self.authority.resident_state_mut()?.document = Document::new(
                num(c, "width", 1024.0) as u32,
                num(c, "height", 768.0) as u32,
            )?;
            return Ok(());
        }
        if op.starts_with("selection.")
            || (op == "selection"
                && (c.get("kind").is_some()
                    || c.get("mode").is_some()
                    || c.get("feather").is_some()))
        {
            return crate::selection::apply(&mut self.authority.resident_state_mut()?.document, c);
        }
        if op == "selection" {
            let document = &mut self.authority.resident_state_mut()?.document;
            document.selection_previous = crate::selection::current(document)
                .or_else(|| document.selection_previous.clone());
            self.authority.resident_state_mut()?.document.selection_coverage = None;
            if let Some(polygon) = c.get("polygon").filter(|p| !p.is_null()) {
                let points: Vec<[f32; 2]> = serde_json::from_value(polygon.clone())
                    .map_err(|_| "Selection polygon must contain [x,y] points")?;
                let bounds = crate::selection::bounds(&points)?;
                if c.get("rect").and_then(rect).is_some_and(|r| r != bounds) {
                    return Err("Selection rectangle must match its polygon bounds".into());
                }
                self.authority.resident_state_mut()?.document.selection = Some(bounds);
                self.authority.resident_state_mut()?.document.selection_polygon = Some(points);
            } else {
                self.authority.resident_state_mut()?.document.selection = c.get("rect").and_then(rect);
                self.authority.resident_state_mut()?.document.selection_polygon = None;
            }
            return Ok(());
        }
        if op == "layer.add" {
            if self.authority.resident_state()?.document.layers.len() >= 100 {
                return Err("Initial version supports up to 100 layers".into());
            }
            let kind = text(c, "kind", "paint");
            if !["paint", "fill", "group"].contains(&kind) {
                return Err("Unsupported layer kind".into());
            }
            let mut layer = Layer::new(
                text(c, "name", "New layer"),
                kind,
                self.authority.resident_state()?.document.width,
                self.authority.resident_state()?.document.height,
            );
            if c.get("color").is_some() {
                layer.color = color(c);
            }
            if let Some(parent) = c.get("parent").and_then(Value::as_str) {
                if !self
                    .authority.resident_state()?.document
                    .layers
                    .iter()
                    .any(|l| l.id == parent && l.kind == "group")
                {
                    return Err("Parent must be an existing group".into());
                }
                layer.parent = Some(parent.into());
            }
            self.authority.resident_state_mut()?.document.layers.insert(0, layer);
            return Ok(());
        }
        let i = self
            .authority.resident_state()?.document
            .layers
            .iter()
            .position(|l| l.id == target)
            .ok_or_else(|| format!("Unknown layer: {target}"))?;
        if visibility_only(c) {
            self.authority.resident_state_mut()?.document.layers[i].visible =
                c["visible"].as_bool().ok_or("Visibility must be boolean")?;
            return Ok(());
        }
        let mut parent = self.authority.resident_state()?.document.layers[i].parent.as_deref();
        for _ in 0..16 {
            let Some(pid) = parent else {
                break;
            };
            let p = self
                .authority.resident_state()?.document
                .layers
                .iter()
                .find(|l| l.id == pid)
                .ok_or("Invalid parent")?;
            if p.locked {
                return Err("Parent group is locked".into());
            }
            parent = p.parent.as_deref();
        }
        if self.authority.resident_state()?.document.layers[i].locked && op != "layer.update" {
            return Err("Layer is locked".into());
        }
        if op == "source.update" {
            return crate::source::update(&mut self.authority.resident_state_mut()?.document.layers[i], c);
        }
        if matches!(op, "effect.bake" | "mask.step.bake") {
            return crate::bake::apply(&mut self.authority.resident_state_mut()?.document, i, c);
        }
        if op == "path.to_mask" {
            return crate::path::to_mask(&mut self.authority.resident_state_mut()?.document, i, c);
        }
        if op.starts_with("path.") {
            return crate::path::update(&mut self.authority.resident_state_mut()?.document.layers[i], c);
        }
        if op == "source.rasterize" {
            if self.authority.resident_state_mut()?.document.layers[i].source.take().is_none() {
                return Err("This layer has no editable source".into());
            }
            return Ok(());
        }
        if self.authority.resident_state()?.document.layers[i].source.is_some() {
            if op == "transform"
                && (crate::projective::parse(c)?.is_some() || crate::warp::parse(c)?.is_some())
            {
                return Err("Rasterize the editable text/vector source at its native precision before Distort, Perspective or Warp".into());
            }
            let mask = c["mask"] == true;
            if !mask
                && matches!(
                    op,
                    "paint"
                        | "smudge"
                        | "clone"
                        | "heal"
                        | "fill"
                        | "paint.fill"
                        | "paint.clear_selection"
                        | "shape"
                        | "gradient"
                        | "adjust"
                        | "image.patch"
                        | "pixels.replace"
                )
            {
                return Err("Edit this layer's text/vector properties, paint its mask, or explicitly rasterize it first".into());
            }
            if matches!(op, "move" | "transform")
                && (mask || (self.authority.resident_state()?.document.selection.is_some() && c["selection_only"] != false))
            {
                return Err("Transform the complete editable text/vector layer with selection_only:false, or rasterize it for a pixel/mask transform".into());
            }
        }
        if op == "layer.delete" {
            let mut ids = vec![target.to_string()];
            loop {
                let count = ids.len();
                for l in &self.authority.resident_state()?.document.layers {
                    if l.parent.as_ref().map(|p| ids.contains(p)).unwrap_or(false)
                        && !ids.contains(&l.id)
                    {
                        ids.push(l.id.clone());
                    }
                }
                if ids.len() == count {
                    break;
                }
            }
            if self
                .authority.resident_state()?.document
                .layers
                .iter()
                .any(|l| ids.contains(&l.id) && l.locked)
            {
                return Err("Unlock the selected layers and their children before deleting".into());
            }
            if self.authority.resident_state()?.document.layers.iter().any(|l| {
                l.clip_to.as_ref().is_some_and(|b| ids.contains(b)) && !ids.contains(&l.id)
            }) {
                return Err("Release clipping or select the complete clipping group before deleting its base".into());
            }
            self.authority.resident_state_mut()?.document.layers.retain(|l| !ids.contains(&l.id));
            return Ok(());
        }
        if op == "layer.clip" {
            let base = c.get("base").and_then(Value::as_str).map(String::from);
            self.authority.resident_state_mut()?.document.layers[i].clip_to = base;
            crate::compositor::validate_clipping(&self.authority.resident_state()?.document)?;
            return Ok(());
        }
        if op == "layer.reorder" {
            let at = num(c, "index", 0.0) as usize;
            if at >= self.authority.resident_state()?.document.layers.len() {
                return Err("Invalid layer index".into());
            }
            let l = self.authority.resident_state_mut()?.document.layers.remove(i);
            self.authority.resident_state_mut()?.document.layers.insert(at, l);
            return Ok(());
        }
        if op == "layer.parent" {
            let parent = c.get("parent").and_then(Value::as_str);
            let mut p = parent;
            let mut depth = 0;
            while let Some(pid) = p {
                if pid == target {
                    return Err("A group cannot contain itself".into());
                }
                let node = self
                    .authority.resident_state()?.document
                    .layers
                    .iter()
                    .find(|l| l.id == pid && l.kind == "group")
                    .ok_or("Invalid parent group")?;
                if node.locked {
                    return Err("Destination folder is locked".into());
                }
                p = node.parent.as_deref();
                depth += 1;
                if depth > 16 {
                    return Err("Group nesting limit exceeded".into());
                }
            }
            self.authority.resident_state_mut()?.document.layers[i].parent = parent.map(String::from);
            return Ok(());
        }
        if op == "layer.update" {
            let l = &mut self.authority.resident_state_mut()?.document.layers[i];
            if l.locked && c.get("locked") != Some(&Value::Bool(false)) {
                return Err("Unlock the layer before changing it".into());
            }
            if let Some(name) = c.get("name").and_then(Value::as_str) {
                l.name = name.into();
            }
            if let Some(v) = c.get("visible").and_then(Value::as_bool) {
                l.visible = v;
            }
            if let Some(v) = c.get("locked").and_then(Value::as_bool) {
                l.locked = v;
                if !v {
                    l.psd_metadata.retain(|entry| entry.key != "lspf");
                }
            }
            if let Some(v) = c.get("opacity").and_then(Value::as_f64) {
                l.opacity = (v as f32).clamp(0.0, 1.0);
            }
            if let Some(v) = c.get("blend").and_then(Value::as_str) {
                if !crate::raster::layer_blends(&l.kind).any(|(mode, _)| mode == v) {
                    return Err("Unsupported blend mode".into());
                }
                l.blend = v.into();
            }
            if c.get("color").is_some() {
                l.color = color(c);
            }
            crate::psd::fill_opacity::update(&mut l.psd_metadata, c)?;
            crate::psd::clipping::update(&mut l.psd_metadata, c)?;
            crate::psd::blend_ranges::update(&mut l.psd_metadata, c)?;
            crate::psd::blend_channels::update(&mut l.psd_metadata, c)?;
            crate::psd::update_layer_color_label(&mut l.psd_metadata, c)?;
            return Ok(());
        }
        if op == "mask.add" {
            if self.authority.resident_state()?.document.layers[i].mask.is_some() {
                return Err("Layer already has a mask".into());
            }
            if self.authority.resident_state()?.document.selection.is_some() {
                let mut command = c.clone();
                command["op"] = json!("mask.from_selection");
                command["mode"] = json!("replace");
                return crate::selection::masks::apply(&mut self.authority.resident_state_mut()?.document, i, &command);
            }
            let layer = &mut self.authority.resident_state_mut()?.document.layers[i];
            layer.mask = Some(Mask {
                linked: true,
                enabled: true,
                cache_key: id(),
                steps: vec![
                    MaskStep {
                        weight: 1.0,
                        id: id(),
                        kind: "fill".into(),
                        enabled: true,
                        value: mask_parameter("fill", num(c, "value", 255.0))?,
                        pixels: Raster::new_depth(
                            layer.pixels.width,
                            layer.pixels.height,
                            layer.pixels.depth,
                        ),
                        settings: Value::Null,
                    },
                    MaskStep {
                        weight: 1.0,
                        id: id(),
                        kind: "paint".into(),
                        enabled: true,
                        value: 0.0,
                        pixels: Raster::new_depth(
                            layer.pixels.width,
                            layer.pixels.height,
                            layer.pixels.depth,
                        ),
                        settings: Value::Null,
                    },
                ],
            });
            return Ok(());
        }
        if op == "mask.remove" {
            self.authority.resident_state_mut()?.document.layers[i].mask = None;
            return Ok(());
        }
        if op == "mask.refine" || op == "mask.from_selection" {
            return crate::selection::masks::apply(&mut self.authority.resident_state_mut()?.document, i, c);
        }
        if op == "mask.from_color" {
            if self.authority.resident_state()?.document.bit_depth == 16 {
                let image = crate::depth16::layer_image(&self.authority.resident_state()?.document, i)?;
                let mask = crate::depth16::mask_image(&self.authority.resident_state()?.document, i)?;
                let source = crate::effects::Image {
                    width: image.width,
                    height: image.height,
                    bytes: image
                        .words
                        .iter()
                        .map(|v| ((u32::from(*v) + 128) / 257) as u8)
                        .collect(),
                };
                return crate::smart_mask::from_color_with_native(
                    &mut self.authority.resident_state_mut()?.document.layers[i],
                    &source,
                    c,
                    mask.as_deref(),
                );
            }
            let l = &self.authority.resident_state()?.document.layers[i];
            let masks = self
                .authority.resident_state()?.document
                .layers
                .iter()
                .map(|l| {
                    l.mask
                        .as_ref()
                        .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
                })
                .collect::<Vec<_>>();
            let colors = crate::effects::prepare(&self.authority.resident_state()?.document, &masks)?;
            let source = if let Some(image) = &colors[i] {
                image.clone()
            } else {
                let (w, h) = if l.kind == "group" {
                    (self.authority.resident_state()?.document.width, self.authority.resident_state()?.document.height)
                } else {
                    (l.pixels.width, l.pixels.height)
                };
                let bytes = if l.kind == "group" {
                    self.authority.resident_state()?.document.preview(None, w.max(h), Some(&l.id), false)?.2
                } else if l.kind == "fill" {
                    (0..w as usize * h as usize).flat_map(|_| l.color).collect()
                } else {
                    l.pixels.rgba()
                };
                Arc::new(crate::effects::Image {
                    width: w,
                    height: h,
                    bytes,
                })
            };
            let source = if crate::effects::canvas_frame(l) {
                Arc::new(crate::effects::Image {
                    width: l.pixels.width,
                    height: l.pixels.height,
                    bytes: crate::render::rgba8(l.pixels.width, l.pixels.height, |x, y| {
                        source.get(x as i32 + l.x, y as i32 + l.y)
                    }),
                })
            } else {
                source
            };
            crate::smart_mask::from_color(&mut self.authority.resident_state_mut()?.document.layers[i], &source, c)?;
            return Ok(());
        }
        if op.starts_with("effect.") {
            use crate::effects::styles::{apply_global_light_request, global_light_request};
            let doc = &mut self.authority.resident_state_mut()?.document;
            let l = &mut doc.layers[i];
            match op {
                "effect.add" => {
                    if l.effects.len() >= 32 {
                        return Err("Effect stack limit: 32".into());
                    }
                    let kind = text(c, "kind", "levels");
                    let settings = c
                        .get("settings")
                        .cloned()
                        .unwrap_or_else(|| crate::effects::defaults(kind));
                    let mut settings = crate::effects::normalized(kind, &settings)?;
                    let global = global_light_request(kind, c)?;
                    apply_global_light_request(&mut settings, &mut doc.psd_global_angle, global);
                    l.effects.push(crate::effects::Effect {
                        weight: crate::effects::command_weight(c)?,
                        id: id(),
                        kind: kind.into(),
                        enabled: true,
                        settings: settings.into(),
                    });
                }
                "effect.update" => {
                    let e = l
                        .effects
                        .iter_mut()
                        .find(|e| e.id == text(c, "effect", ""))
                        .ok_or("Unknown effect")?;
                    let global = global_light_request(&e.kind, c)?;
                    if let Some(v) = c.get("enabled").and_then(Value::as_bool) {
                        e.enabled = v;
                    }
                    if c.get("weight").is_some() {
                        e.weight = crate::effects::command_weight(c)?;
                    }
                    if let Some(settings) = c.get("settings") {
                        let mut settings = crate::effects::normalized(&e.kind, settings)?;
                        apply_global_light_request(&mut settings, &mut doc.psd_global_angle, global);
                        e.settings = settings.into();
                    } else if global.is_some() {
                        apply_global_light_request(Arc::make_mut(&mut e.settings), &mut doc.psd_global_angle, global);
                    }
                }
                "effect.delete" => {
                    let at = l
                        .effects
                        .iter()
                        .position(|e| e.id == text(c, "effect", ""))
                        .ok_or("Unknown effect")?;
                    l.effects.remove(at);
                }
                "effect.reorder" => {
                    let from = l
                        .effects
                        .iter()
                        .position(|e| e.id == text(c, "effect", ""))
                        .ok_or("Unknown effect")?;
                    let to = c["index"].as_u64().ok_or("Invalid effect index")? as usize;
                    if to >= l.effects.len() {
                        return Err("Invalid effect index".into());
                    }
                    let effect = l.effects.remove(from);
                    l.effects.insert(to, effect);
                }
                _ => return Err("Unknown effect operation".into()),
            }
            return Ok(());
        }
        if op.starts_with("mask.") {
            let l = &mut self.authority.resident_state_mut()?.document.layers[i];
            let m = l.mask.as_mut().ok_or("Layer has no mask")?;
            if op != "mask.toggle" {
                m.cache_key = id();
            }
            match op {
                "mask.link" => {
                    m.linked = c["linked"].as_bool().ok_or("Mask linked must be boolean")?;
                }
                "mask.toggle" => {
                    m.enabled = c
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(!m.enabled)
                }
                "mask.step.add" => {
                    if m.steps.len() >= 32 {
                        return Err("Initial mask stack supports up to 32 steps".into());
                    }
                    let definition = crate::mask::commands::NewStep::parse(c)?;
                    m.steps.push(MaskStep {
                        weight: definition.weight,
                        id: id(),
                        kind: definition.kind.into(),
                        enabled: true,
                        value: definition.value,
                        pixels: Raster::new_depth(l.pixels.width, l.pixels.height, l.pixels.depth),
                        settings: definition.settings.into_owned(),
                    });
                }
                "mask.step.update" => {
                    let s = m
                        .steps
                        .iter_mut()
                        .find(|s| s.id == text(c, "step", ""))
                        .ok_or("Unknown mask step")?;
                    let update = crate::mask::commands::StepUpdate::parse(&s.kind, c)?;
                    if let Some(v) = update.enabled {
                        s.enabled = v;
                    }
                    if let Some(settings) = update.settings {
                        s.settings = settings.clone();
                    }
                    if let Some(weight) = update.weight {
                        s.weight = weight;
                    }
                    if let Some(value) = update.value {
                        s.value = value;
                    }
                }
                "mask.step.delete" => m.steps.retain(|s| s.id != text(c, "step", "")),
                "mask.step.reorder" => {
                    let si = m
                        .steps
                        .iter()
                        .position(|s| s.id == text(c, "step", ""))
                        .ok_or("Unknown mask step")?;
                    let to = num(c, "index", 0.0) as usize;
                    if to >= m.steps.len() {
                        return Err("Invalid stack index".into());
                    }
                    let s = m.steps.remove(si);
                    m.steps.insert(to, s);
                }
                _ => return Err("Unknown mask operation".into()),
            };
            return Ok(());
        }
        if self.authority.resident_state()?.document.layers[i].kind == "adjustment"
            && (!c["mask"].as_bool().unwrap_or(false)
                || ["move", "transform", "liquify.stroke"].contains(&op))
        {
            return Err("Edit the adjustment effect settings or paint on its mask".into());
        }
        if op == "liquify.stroke" {
            let l = &self.authority.resident_state()?.document.layers[i];
            let x = if l.kind == "group" { 0 } else { l.x };
            let y = if l.kind == "group" { 0 } else { l.y };
            let mut stroke = crate::liquify::command::stroke(
                c,
                [x, y],
                self.authority.resident_state()?.document.selection,
                crate::selection::polygon(&self.authority.resident_state()?.document),
            )?;
            let document = &self.authority.resident_state()?.document;
            let coverage = document
                .selection_coverage
                .as_ref()
                .filter(|m| document.selection == Some(m.bounds))
                .map(|m| m.local(x, y));
            if coverage
                .as_ref()
                .is_some_and(|m| m.bounds[0] >= m.bounds[2])
            {
                return Ok(());
            }
            stroke["coverage"] = json!(coverage);
            let l = &mut self.authority.resident_state_mut()?.document.layers[i];
            let at = if let Some(at) = crate::liquify::command::target(
                c,
                l.effects
                    .iter()
                    .map(|e| (e.id.as_str(), e.kind.as_str(), e.enabled)),
            )? {
                at
            } else {
                l.effects.push(crate::effects::Effect {
                    weight: 1.0,
                    id: id(),
                    kind: "liquify".into(),
                    enabled: true,
                    settings: crate::liquify::defaults().into(),
                });
                l.effects.len() - 1
            };
            l.effects[at].settings =
                crate::liquify::command::append(&l.effects[at].settings, stroke)?.into();
            return Ok(());
        }
        if op == "paint.fill" {
            return crate::fill::apply(&mut self.authority.resident_state_mut()?.document, c);
        }
        if op == "paint.clear_selection" {
            return crate::fill::clear_selection(&mut self.authority.resident_state_mut()?.document, c);
        }
        let document = &self.authority.resident_state()?.document;
        let selection_coverage = document
            .selection_coverage
            .clone()
            .filter(|m| document.selection == Some(m.bounds));
        let selection_polygon = crate::selection::polygon(&self.authority.resident_state()?.document).map(Vec::from);
        let selection = self.authority.resident_state()?.document.selection;
        let canvas = [0, 0, self.authority.resident_state()?.document.width as i32, self.authority.resident_state()?.document.height as i32];
        if op == "fill"
            && self.authority.resident_state()?.document.layers[i].kind == "group"
            && !c["mask"].as_bool().unwrap_or(false)
        {
            if self.authority.resident_state()?.document.layers.len() >= 100 {
                return Err("Initial version supports up to 100 layers".into());
            }
            let mut child = Layer::new("Foreground fill", "paint", self.authority.resident_state()?.document.width, self.authority.resident_state()?.document.height);
            child.parent = Some(target.into());
            child.pixels = Raster::new_depth(self.authority.resident_state()?.document.width, self.authority.resident_state()?.document.height, self.authority.resident_state()?.document.bit_depth);
            let area = selection.unwrap_or([0, 0, self.authority.resident_state()?.document.width as i32, self.authority.resident_state()?.document.height as i32]);
            let col = color(c);
            for y in area[1].max(0)..area[3].min(self.authority.resident_state()?.document.height as i32) {
                for x in area[0].max(0)..area[2].min(self.authority.resident_state()?.document.width as i32) {
                    if selection_polygon.as_ref().is_some_and(|points| {
                        !crate::selection::contains(points, x as f32 + 0.5, y as f32 + 0.5)
                    }) {
                        continue;
                    }
                    child.pixels.set(x, y, col);
                }
            }
            self.authority.resident_state_mut()?.document.layers.insert(i + 1, child);
            return Ok(());
        }
        if ["move", "transform"].contains(&op) && self.authority.resident_state()?.document.layers[i].kind == "group" {
            return crate::transform::folder(&mut self.authority.resident_state_mut()?.document, target, c);
        }
        if ["clone", "heal"].contains(&op) {
            return crate::retouch::apply(&mut self.authority.resident_state_mut()?.document, i, c);
        }
        let layer = &mut self.authority.resident_state_mut()?.document.layers[i];
        if ["move", "transform"].contains(&op) && c["selection_only"].as_bool() != Some(false) {
            if let Some(coverage) = selection_coverage {
                let transformed = crate::transform::selection_with_coverage(layer, &coverage, c)?;
                self.authority.resident_state_mut()?.document.selection = Some(transformed.bounds);
                self.authority.resident_state_mut()?.document.selection_polygon = None;
                self.authority.resident_state_mut()?.document.selection_coverage = Some(transformed);
                return Ok(());
            }
            if let Some(area) = selection {
                let polygon =
                    selection_polygon.unwrap_or_else(|| crate::selection::rectangle(area));
                let (bounds, polygon) =
                    crate::transform::selection_with_polygon(layer, &polygon, c)?;
                self.authority.resident_state_mut()?.document.selection = Some(bounds);
                self.authority.resident_state_mut()?.document.selection_polygon = Some(polygon);
                return Ok(());
            }
        }
        if op == "transform" || c.get("mask").and_then(Value::as_bool).unwrap_or(false) {
            if let Some(m) = &mut layer.mask {
                m.cache_key = id();
            }
        }
        if ["move", "transform"].contains(&op) {
            return crate::transform::layer(layer, c);
        }
        if op == "fill"
            && !c["mask"].as_bool().unwrap_or(false)
            && layer.kind == "fill"
            && selection.is_none()
        {
            layer.color = color(c);
            return Ok(());
        }
        if (layer.kind == "group" || layer.kind == "fill" || layer.kind == "adjustment")
            && !c.get("mask").and_then(Value::as_bool).unwrap_or(false)
        {
            return Err("Paint on a paint layer or its mask".into());
        }
        if ["paint", "smudge"].contains(&op) {
            let (mut points, pressures) = crate::brush::points_from_command(c)?;
            let clip = selection.map(|s| {
                [
                    s[0] - layer.x,
                    s[1] - layer.y,
                    s[2] - layer.x,
                    s[3] - layer.y,
                ]
            });
            let x = layer.x;
            let y = layer.y;
            let mut settings = crate::brush::Settings::from_command(c)?;
            settings.texture_origin = [x, y];
            for point in &mut points {
                point[0] -= x as f32;
                point[1] -= y as f32;
            }
            let raster = edit_raster(layer, c)?;
            let color = raster_color(c, raster.depth)?;
            let polygon = selection_polygon.as_ref().map(|points| {
                points
                    .iter()
                    .map(|p| [p[0] - x as f32, p[1] - y as f32])
                    .collect::<Vec<_>>()
            });
            if op == "smudge" {
                crate::smudge::paint_native_color(
                    raster,
                    &points,
                    pressures.as_deref(),
                    settings,
                    color,
                    clip,
                    polygon.as_deref(),
                )?;
            } else {
                crate::brush::paint_native_color(
                    raster,
                    &points,
                    pressures.as_deref(),
                    settings,
                    color,
                    c["erase"].as_bool().unwrap_or(false),
                    clip,
                    polygon.as_deref(),
                )?;
            }
            return Ok(());
        }
        if op == "fill" || op == "shape" || op == "gradient" {
            let area = c.get("rect").and_then(rect).or(selection).unwrap_or(canvas);
            let (lx, ly, lw, lh) = (
                layer.x,
                layer.y,
                layer.pixels.width as i32,
                layer.pixels.height as i32,
            );
            let raster = edit_raster(layer, c)?;
            let primitive = primitive::Primitive::new(c, area, raster.depth)?;
            let mut clip = area;
            if let Some(s) = selection {
                clip = [
                    clip[0].max(s[0]),
                    clip[1].max(s[1]),
                    clip[2].min(s[2]),
                    clip[3].min(s[3]),
                ];
            }
            for yy in clip[1].max(ly)..clip[3].min(ly + lh) {
                for xx in clip[0].max(lx)..clip[2].min(lx + lw) {
                    if selection_polygon.as_ref().is_some_and(|points| {
                        !crate::selection::contains(points, xx as f32 + 0.5, yy as f32 + 0.5)
                    }) {
                        continue;
                    }
                    let tx = xx - lx;
                    let ty = yy - ly;
                    if !primitive.contains(xx, yy) {
                        continue;
                    }
                    if raster.depth == 16 {
                        raster.set16(tx, ty, primitive.blend16(xx, yy, raster.get16(tx, ty)));
                    } else {
                        raster.set(tx, ty, primitive.blend8(xx, yy, raster.get(tx, ty)));
                    }
                }
            }
            return Ok(());
        }
        if c.get("mask").and_then(Value::as_bool).unwrap_or(false) {
            return Err("This operation does not support masks; use paint, fill, shape, gradient, or mask effects.".into());
        }
        if op == "adjust" {
            let origin = [layer.x, layer.y];
            let frame = [
                layer.x,
                layer.y,
                layer.x + layer.pixels.width as i32,
                layer.y + layer.pixels.height as i32,
            ];
            let area = c.get("rect").and_then(rect).or(selection).unwrap_or(frame);
            let primitive = primitive::Primitive::new(c, area, layer.pixels.depth)?;
            let selected = selection.unwrap_or(frame);
            let clip = [
                area[0].max(selected[0]).max(frame[0]),
                area[1].max(selected[1]).max(frame[1]),
                area[2].min(selected[2]).min(frame[2]),
                area[3].min(selected[3]).min(frame[3]),
            ];
            let tile_clip = |tx: u32, ty: u32| {
                let left = origin[0] + (tx * crate::raster::TILE) as i32;
                let top = origin[1] + (ty * crate::raster::TILE) as i32;
                [
                    clip[0].max(left),
                    clip[1].max(top),
                    clip[2].min(left + crate::raster::TILE as i32),
                    clip[3].min(top + crate::raster::TILE as i32),
                ]
            };
            let contains = |x, y| {
                selection_polygon
                    .as_ref()
                    .is_none_or(|p| crate::selection::contains(p, x as f32 + 0.5, y as f32 + 0.5))
            };
            // Adjust stored RGB, including transparent samples, without filling
            // sparse holes or changing physical padding.
            if layer.pixels.depth == 16 {
                for (&(tx, ty), tile) in &mut layer.pixels.samples16 {
                    let [l, t, r, b] = tile_clip(tx, ty);
                    if l >= r || t >= b {
                        continue;
                    }
                    let tile = Arc::make_mut(tile);
                    for y in t..b {
                        for x in l..r {
                            if !contains(x, y) {
                                continue;
                            }
                            let at = (((y - origin[1]) as u32 % crate::raster::TILE
                                * crate::raster::TILE
                                + (x - origin[0]) as u32 % crate::raster::TILE)
                                * 4) as usize;
                            let before = std::array::from_fn(|c| tile[at + c]);
                            tile[at..at + 4].copy_from_slice(&primitive.blend16(x, y, before));
                        }
                    }
                }
            } else {
                for (&(tx, ty), tile) in &mut layer.pixels.tiles {
                    let [l, t, r, b] = tile_clip(tx, ty);
                    if l >= r || t >= b {
                        continue;
                    }
                    let tile = Arc::make_mut(tile);
                    for y in t..b {
                        for x in l..r {
                            if !contains(x, y) {
                                continue;
                            }
                            let at = (((y - origin[1]) as u32 % crate::raster::TILE
                                * crate::raster::TILE
                                + (x - origin[0]) as u32 % crate::raster::TILE)
                                * 4) as usize;
                            let before = std::array::from_fn(|c| tile[at + c]);
                            tile[at..at + 4].copy_from_slice(&primitive.blend8(x, y, before));
                        }
                    }
                }
            }
            return Ok(());
        }
        if op == "pixels.replace" {
            return if let Some(pixels) = pixels {
                crate::code::replace_pixels_bytes(
                    &mut self.authority.resident_state_mut()?.document,
                    c,
                    pixels.bytes(),
                    crate::code::SampleOrder::Big,
                )
            } else {
                crate::code::replace_pixels(&mut self.authority.resident_state_mut()?.document, c)
            };
        }
        if op == "image.patch" {
            return image_patch::apply(
                layer,
                c,
                selection,
                selection_polygon.as_deref(),
                selection_coverage.as_ref(),
            );
        }
        Err(format!("Unsupported operation: {op}"))
    }
    pub fn undo(&mut self, actor: &str) -> Result<(), String> {
        self.ensure_write_available()?;
        let h = self.authority.resident_state()?.undo.last().ok_or("Nothing to undo")?.clone();
        if actor != "human" && h.actor != actor {
            return Err("Latest change belongs to another participant. Observe again or inspect selective task undo.".into());
        }
        self.check(actor, &h.scopes)?;
        let revision = self.authority.resident_state()?.document.revision.checked_add(1).ok_or("Document revision overflow")?;
        self.authority.resident_state_mut()?.document = h.before.clone();
        crate::selection::restore_wand_revision(&mut self.authority.resident_state_mut()?.document, revision);
        self.authority.resident_state_mut()?.undo.pop();
        self.changes.push((revision, h.scopes.clone()));
        self.mark_ai(actor, "Undo AI edit", "history", h.scopes.clone());
        self.authority.resident_state_mut()?.redo.push(h);
        Ok(())
    }
    pub fn redo(&mut self, actor: &str) -> Result<(), String> {
        self.ensure_write_available()?;
        let h = self.authority.resident_state()?.redo.last().ok_or("Nothing to redo")?.clone();
        if actor != "human" && h.actor != actor {
            return Err("Redo belongs to another participant".into());
        }
        self.check(actor, &h.scopes)?;
        let revision = self.authority.resident_state()?.document.revision.checked_add(1).ok_or("Document revision overflow")?;
        self.authority.resident_state_mut()?.document = h.after.clone();
        crate::selection::restore_wand_revision(&mut self.authority.resident_state_mut()?.document, revision);
        self.authority.resident_state_mut()?.redo.pop();
        self.changes.push((revision, h.scopes.clone()));
        self.mark_ai(actor, "Redo AI edit", "history", h.scopes.clone());
        self.authority.resident_state_mut()?.undo.push(h);
        Ok(())
    }
    pub(crate) fn release_file_ownership(&mut self) {
        if let Some(owner) = self.file_ownership.take() {
            owner.release();
        }
    }
    pub fn replace(
        &mut self,
        mut doc: Document,
        path: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        self.ensure_write_available()?;
        if ![8, 16].contains(&doc.bit_depth) {
            return Err("Color depth must be 8 or 16 bits".into());
        }
        for coverage in [&doc.selection_coverage, &doc.selection_previous]
            .into_iter()
            .flatten()
        {
            coverage.validate()?;
        }
        for layer in &doc.layers {
            layer.pixels.validate_layout()?;
            if let Some(mask) = &layer.mask {
                for step in &mask.steps {
                    step.pixels.validate_layout()?;
                }
            }
        }
        doc.ensure_depth();
        self.check(
            "human",
            &[Scope {
                target: None,
                rect: None,
            }],
        )?;
        // Reopening the same serialized PSD is still a new runtime source.
        doc.id = id();
        self.authority.resident_state_mut()?.document = doc;
        self.path = path;
        self.saved_revision = self.authority.resident_state()?.document.revision;
        self.authority.resident_state_mut()?.undo.clear();
        self.authority.resident_state_mut()?.redo.clear();
        self.changes.clear();
        self.authority.resident_state_mut()?.truncated_tasks.clear();
        self.ai_change = None;
        self.proposals.clear();
        self.recent_tasks.clear();
        Ok(())
    }
}
impl Default for Engine {
    fn default() -> Self {
        Self::new()
    }
}

pub(crate) fn mask_parameter(kind: &str, value: f64) -> Result<f32, String> {
    let range = match kind {
        "blur" => 0.0..=64.0,
        "levels" => 0.1..=5.0,
        _ => 0.0..=255.0,
    };
    if !value.is_finite() || !range.contains(&value) {
        return Err(format!(
            "Invalid {kind} mask parameter: expected {}–{}",
            range.start(),
            range.end()
        ));
    }
    if kind == "density" && value.fract() != 0. {
        return Err("Mask density requires an integer sample from 0 to 255".into());
    }
    Ok(value as f32)
}
pub(crate) fn edit_raster<'a>(
    layer: &'a mut Layer,
    command: &Value,
) -> Result<&'a mut Raster, String> {
    if command
        .get("mask")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let mask = layer.mask.as_mut().ok_or("Layer has no mask")?;
        let requested = command.get("step").and_then(Value::as_str);
        let index = crate::mask::paint_step_index(mask, requested)?;
        Ok(&mut mask.steps[index].pixels)
    } else {
        Ok(&mut layer.pixels)
    }
}

fn visibility_only(c: &Value) -> bool {
    c["op"] == "layer.update"
        && c["visible"].is_boolean()
        && c.as_object().is_some_and(|o| {
            o.keys()
                .all(|k| ["op", "layer", "visible"].contains(&k.as_str()))
        })
}
