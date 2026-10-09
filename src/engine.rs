use crate::raster::{blend, check_size, Pixel, Raster};
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
    pub steps: Vec<MaskStep>,
    #[serde(skip, default = "id")]
    pub cache_key: String,
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
    pub source: Option<crate::source::Source>,
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
    pub layers: Vec<Layer>,
    pub selection: Option<[i32; 4]>,
    #[serde(default)]
    pub selection_polygon: Option<Vec<[f32; 2]>>,
    #[serde(default)]
    pub selection_coverage: Option<crate::selection::Coverage>,
    #[serde(default)]
    pub selection_previous: Option<crate::selection::Coverage>,
    pub read_only: bool,
    pub warnings: Vec<String>,
}
impl Document {
    pub fn new(w: u32, h: u32) -> Result<Self, String> {
        check_size(w, h)?;
        Ok(Self {
            id: id(),
            name: "Untitled.psd".into(),
            width: w,
            height: h,
            revision: 0,
            bit_depth: 8,
            icc_profile: None,
            srgb_tagged: false,
            layers: vec![Layer::new("Paint 1", "paint", w, h)],
            selection: None,
            selection_polygon: None,
            selection_coverage: None,
            selection_previous: None,
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
    }
    pub fn preview(
        &self,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Result<(u32, u32, Vec<u8>, [i32; 4]), String> {
        if self.bit_depth == 16 {
            return crate::depth16::preview(self, rect, edge, target, mask);
        }
        let (w, h, mut bytes, rect) = self.preview_native8(rect, edge, target, mask)?;
        if !mask {
            if let Some(profile) = self.icc_profile.as_deref() {
                // Unsupported profiles retain raw feedback with a visible warning.
                // Explicit conversion uses the strict, fallible path instead.
                if crate::color_profile::supported(profile).is_ok() {
                    crate::color_profile::convert8(profile, &mut bytes)?;
                }
            }
        }
        Ok((w, h, bytes, rect))
    }
    pub(crate) fn preview_native8(
        &self,
        rect: Option<[i32; 4]>,
        edge: u32,
        target: Option<&str>,
        mask: bool,
    ) -> Result<(u32, u32, Vec<u8>, [i32; 4]), String> {
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
        crate::mask::validate_budget(&self.layers)?;
        let prepared: Vec<_> = self
            .layers
            .iter()
            .map(|l| {
                l.mask
                    .as_ref()
                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
            })
            .collect();
        let colors = crate::effects::prepare(self, &prepared)?;
        // Downsampled whole-document previews only; full-resolution export stays on the CPU.
        if target.is_none() && !mask && scale < 1.0 {
            let xs: Vec<_> = (0..w)
                .map(|x| rect[0] + (x as f32 / scale) as i32)
                .collect();
            let ys: Vec<_> = (0..h)
                .map(|y| rect[1] + (y as f32 / scale) as i32)
                .collect();
            if let Some(bytes) = crate::gpu::composite::try_render(self, &colors, &xs, &ys) {
                return Ok((w, h, bytes, rect));
            }
        }
        let target_index = target.and_then(|id| self.layers.iter().position(|l| l.id == id));
        let plan = crate::compositor::Plan::new(self, &prepared, &colors);
        let root = plan.group(None);
        let bytes = crate::render::rgba8(w, h, |x, y| {
            let sx = rect[0] + (x as f32 / scale) as i32;
            let sy = rect[1] + (y as f32 / scale) as i32;
            let p = if let Some(l) = layer {
                if mask {
                    let m = (l.mask_value_prepared(
                        sx - l.x,
                        sy - l.y,
                        target_index.and_then(|i| prepared[i].as_deref()),
                        true,
                    ) * 255.0) as u8;
                    [m, m, m, 255]
                } else if let Some(image) = target_index.and_then(|i| colors[i].as_ref()) {
                    if ["group", "adjustment"].contains(&l.kind.as_str()) {
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
        });
        Ok((w, h, bytes, rect))
    }
    pub fn export_png(&self) -> Result<Vec<u8>, String> {
        let profile = self
            .icc_profile
            .as_deref()
            .map(Vec::as_slice)
            .or_else(|| self.srgb_tagged.then(crate::color_profile::srgb_profile));
        if self.bit_depth == 16 {
            let image = crate::depth16::render(self)?;
            return crate::raster::png16_with_profile(
                image.width,
                image.height,
                &image.words,
                profile,
            );
        }
        let (w, h, bytes, _) =
            self.preview_native8(None, self.width.max(self.height), None, false)?;
        crate::raster::png_with_profile(w, h, &bytes, profile)
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
#[derive(Clone)]
pub struct Engine {
    pub project_id: String,
    pub closed: bool,
    pub(crate) workspace: Option<std::sync::Weak<std::sync::Mutex<crate::workspace::Workspace>>>,
    pub(crate) workspace_owner:
        Option<std::sync::Arc<std::sync::Mutex<crate::workspace::Workspace>>>,
    pub doc: Document,
    pub leases: Vec<Lease>,
    pub undo: Vec<History>,
    pub redo: Vec<History>,
    pub(crate) truncated_tasks: BTreeSet<(String, String)>,
    pub changes: Vec<(u64, Vec<Scope>)>,
    pub saved_revision: u64,
    pub feedback: BTreeMap<String, String>,
    pub path: Option<std::path::PathBuf>,
    pub status: String,
    pub file_version: Option<(u64, u128)>,
    pub capture_ui: Option<std::path::PathBuf>,
    pub capture_panel: Option<String>,
    pub mcp_clients: BTreeMap<String, u64>,
    pub activity: String,
    pub ai_change: Option<AiChange>,
    ai_serial: u64,
    pub focus_requested: bool,
    pub loading: Option<crate::loading::Control>,
    pub brush_library: std::sync::Arc<std::sync::Mutex<crate::brush_library::Library>>,
    pub(crate) proposals: Vec<crate::collaboration::Proposal>,
    pub(crate) recent_tasks: Vec<crate::collaboration::TaskRecord>,
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

impl Engine {
    /// Transient visual feedback uses the same commands as a committed edit, without touching history or shared state.
    pub fn preview_edits(doc: Document, commands: &[Value]) -> Result<Document, String> {
        let mut engine = Self::new();
        engine.doc = doc;
        let resolved = engine
            .brush_library
            .lock()
            .unwrap()
            .resolve_commands(commands)?;
        for command in &resolved {
            engine.apply(command)?;
        }
        engine.doc.ensure_depth();
        crate::compositor::validate_clipping(&engine.doc)?;
        crate::mask::validate_budget(&engine.doc.layers)?;
        crate::effects::validate_budget(&engine.doc)?;
        if engine.doc.bit_depth == 16 {
            crate::depth16::validate_budget(&engine.doc)?;
        }
        crate::effects::invalidate(&mut engine.doc, commands);
        Ok(engine.doc)
    }
    pub fn new() -> Self {
        Self {
            project_id: id(),
            closed: false,
            workspace: None,
            workspace_owner: None,
            doc: Document::new(1024, 768).unwrap(),
            leases: vec![],
            undo: vec![],
            redo: vec![],
            truncated_tasks: BTreeSet::new(),
            changes: vec![],
            saved_revision: 0,
            feedback: BTreeMap::new(),
            path: None,
            status: "Ready".into(),
            file_version: None,
            capture_ui: None,
            capture_panel: None,
            mcp_clients: BTreeMap::new(),
            activity: String::new(),
            ai_change: None,
            ai_serial: 0,
            focus_requested: false,
            loading: None,
            brush_library: std::sync::Arc::new(std::sync::Mutex::new(
                crate::brush_library::Library::default(),
            )),
            proposals: vec![],
            recent_tasks: vec![],
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
    pub(crate) fn mark_ai(&mut self, actor: &str, label: &str, tool: &str, scopes: Vec<Scope>) {
        if actor == "human" {
            return;
        }
        self.ai_serial += 1;
        self.ai_change = Some(AiChange {
            serial: self.ai_serial,
            actor: actor.into(),
            label: label.into(),
            tool: tool.into(),
            scopes,
            at: now(),
        });
    }
    pub fn state(&mut self) -> Value {
        let mut result = self.state_core();
        result["project_id"] = json!(self.project_id);
        result["closed"] = json!(self.closed);
        result["proposals"] = json!(self.proposals.iter().map(|p| p.state()).collect::<Vec<_>>());
        result["task_recovery"] = self.task_recovery();
        result
    }
    fn state_core(&mut self) -> Value {
        self.expire();
        json!({"loading":self.loading.as_ref().map(|c|{let s=c.status();json!({"stage":s.stage,"completed":s.completed,"total":s.total})}),"file_status":self.status,"document":{"id":self.doc.id,"name":self.doc.name,"width":self.doc.width,"height":self.doc.height,"revision":self.doc.revision,"bit_depth":self.doc.bit_depth,"color_profile":crate::color_profile::summary(self.doc.icc_profile.as_deref().map(Vec::as_slice)),"read_only":self.doc.read_only,"warnings":self.doc.warnings,"selection":self.doc.selection,"selection_polygon":crate::selection::polygon(&self.doc)},"layers":self.doc.layers.iter().map(|l|json!({"id":l.id,"name":l.name,"kind":l.kind,"parent":l.parent,"clip_to":l.clip_to,"visible":l.visible,"locked":l.locked,"opacity":l.opacity,"blend":l.blend,"bounds":[l.x,l.y,l.x+l.pixels.width as i32,l.y+l.pixels.height as i32],"effects":l.effects,"source":l.source,"transform_source":crate::retained::observe(&l.pixels),"mask":l.mask.as_ref().map(|m|json!({"enabled":m.enabled,"steps":m.steps.iter().map(|s|json!({"id":s.id,"kind":s.kind,"enabled":s.enabled,"weight":s.weight,"value":s.value,"settings":s.settings,"transform_source":crate::retained::observe(&s.pixels)})).collect::<Vec<_>>()}))})).collect::<Vec<_>>(),"reservations":self.leases,"ai_change":self.ai_change,"dirty":self.doc.revision!=self.saved_revision})
    }
    pub fn scope_overlap(&self, a: &Scope, b: &Scope) -> bool {
        let visibility = |s: &Scope| {
            s.target
                .as_ref()
                .is_some_and(|t| t.starts_with("@visibility:"))
        };
        if visibility(a) || visibility(b) {
            return visibility(a) && visibility(b) && a.target == b.target;
        }
        if let (Some(x), Some(y)) = (&a.target, &b.target) {
            if x != y {
                let ancestor = |child: &str, parent: &str| {
                    let mut cur = Some(child);
                    for _ in 0..=16 {
                        let Some(id) = cur else {
                            return false;
                        };
                        if id == parent {
                            return true;
                        }
                        cur = self
                            .doc
                            .layers
                            .iter()
                            .find(|l| l.id == id)
                            .and_then(|l| l.parent.as_deref());
                    }
                    false
                };
                if !ancestor(x, y) && !ancestor(y, x) {
                    return false;
                }
                let mut a = a.clone();
                a.target = None;
                return a.overlap(b);
            }
        }
        a.overlap(b)
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
        self.ensure_open()?;
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
        let op = text(c, "op", "");
        let target = c.get("layer").and_then(Value::as_str).map(String::from);
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
                self.doc
                    .layers
                    .iter()
                    .any(|l| l.id == *t && l.kind == "group")
            })
        {
            return vec![Scope { target, rect: None }];
        }
        if op == "layer.merge" {
            return crate::merge::requested(c)
                .and_then(|ids| crate::merge::resolve(&self.doc, &ids))
                .map(|ids| ids.into_iter().map(|id| Scope::layer(&id)).collect())
                .unwrap_or_else(|_| {
                    vec![Scope {
                        target: None,
                        rect: None,
                    }]
                });
        }
        if ["layer.add", "image.import", "source.add"].contains(&op) {
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
                    .and_then(|t| self.doc.layers.iter().find(|l| l.id == *t))
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
        if op == "paint.fill" {
            let layer = target
                .as_deref()
                .and_then(|id| self.doc.layers.iter().find(|l| l.id == id));
            if layer.is_some_and(|l| l.kind == "group") && c["mask"] != true {
                return vec![Scope { target, rect: None }];
            }
            let mut area = c
                .get("rect")
                .and_then(rect)
                .or(self.doc.selection)
                .unwrap_or([0, 0, self.doc.width as i32, self.doc.height as i32]);
            if let Some(selection) = self.doc.selection {
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
            return vec![Scope {
                target,
                rect: c.get("rect").and_then(rect),
            }];
        }
        if [
            "new",
            "crop",
            "resize",
            "canvas.resize",
            "image.resize",
            "document.settings",
            "layer.add",
            "layer.reorder",
            "layer.delete",
            "layer.merge",
            "layer.duplicate",
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
                let r = num(c, "radius", 10.0) as f32 + 2.0;
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
        if ["shape", "gradient", "fill", "adjust"].contains(&op) {
            area = c["rect"]
                .as_array()
                .and_then(|_| rect(&c["rect"]))
                .or(self.doc.selection);
        }
        if ["move", "transform"].contains(&op) && c["selection_only"] != false {
            if let Some(b) = self.doc.selection {
                let moving = op == "move";
                let dx = if moving { num(c, "dx", 0.).round() } else { 0. };
                let dy = if moving { num(c, "dy", 0.).round() } else { 0. };
                let angle = if moving {
                    0.
                } else {
                    num(c, "angle", 0.).to_radians()
                };
                let (sin, cos) = angle.sin_cos();
                let sx = if moving { 1. } else { num(c, "scale_x", 1.) };
                let sy = if moving { 1. } else { num(c, "scale_y", 1.) };
                let px = c["pivot"][0]
                    .as_f64()
                    .unwrap_or((b[0] as f64 + b[2] as f64) / 2.);
                let py = c["pivot"][1]
                    .as_f64()
                    .unwrap_or((b[1] as f64 + b[3] as f64) / 2.);
                let mut out = b;
                for p in crate::selection::rectangle(b) {
                    let x = px + (p[0] as f64 - px) * sx * cos - (p[1] as f64 - py) * sy * sin + dx;
                    let y = py + (p[0] as f64 - px) * sx * sin + (p[1] as f64 - py) * sy * cos + dy;
                    out = [
                        out[0].min(x.floor() as i32),
                        out[1].min(y.floor() as i32),
                        out[2].max(x.ceil() as i32),
                        out[3].max(y.ceil() as i32),
                    ];
                }
                area = Some(out);
            }
        }
        let area = area.map(|a| {
            if ["move", "transform"].contains(&op) {
                return a;
            }
            self.doc.selection.map_or(a, |b| {
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
            actor, commands, expected, task, label, gesture, None, None, None,
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
        )
    }
    pub(crate) fn commit_proposal(
        &mut self,
        proposal: &crate::collaboration::Proposal,
    ) -> Result<Value, String> {
        let doc = proposal
            .draft
            .clone()
            .ok_or("This proposal is no longer available")?;
        if proposal.document != self.doc.id || proposal.revision != self.doc.revision {
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
            Some(doc),
        )
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
    ) -> Result<Value, String> {
        self.ensure_open()?;
        if self.doc.read_only {
            return Err("This PSD is read-only. Create a compatible copy first.".into());
        }
        if commands.is_empty() || commands.len() > 100 {
            return Err("A batch must contain 1–100 commands".into());
        }
        let resolved = self
            .brush_library
            .lock()
            .unwrap()
            .resolve_commands(commands)?;
        let commands = resolved.as_slice();
        let before = self.doc.clone();
        let mut scopes = vec![];
        for c in commands {
            scopes.extend(self.scopes(c));
        }
        self.check(actor, &scopes)?;
        if let Some(rev) = expected {
            if rev > self.doc.revision {
                return Err("Revision is from the future".into());
            }
            if rev < self.doc.revision {
                let earliest = self
                    .changes
                    .first()
                    .map(|x| x.0)
                    .unwrap_or(self.doc.revision);
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
        let mut selection_gesture: Option<(
            Value,
            Option<[i32; 4]>,
            Option<Vec<[f32; 2]>>,
            Vec<String>,
            Option<crate::selection::Coverage>,
        )> = None;
        if let Some(mut doc) = prepared_doc {
            if doc.id != before.id || doc.revision != before.revision + 1 {
                return Err("Proposal source changed; request a fresh proposal".into());
            }
            doc.name = self.doc.name.clone();
            self.doc = doc;
        } else {
            for c in commands {
                if matches!(c["op"].as_str(), Some("move" | "transform"))
                    && c["selection_only"] != false
                {
                    let mut signature = c.clone();
                    if let Some(object) = signature.as_object_mut() {
                        object.remove("layer");
                    }
                    let target = c["layer"].as_str().unwrap_or("").to_owned();
                    if let Some((previous, bounds, polygon, targets, coverage)) =
                        &mut selection_gesture
                    {
                        if *previous == signature && !targets.contains(&target) {
                            self.doc.selection = *bounds;
                            self.doc.selection_polygon = polygon.clone();
                            self.doc.selection_coverage = coverage.clone();
                            targets.push(target);
                        } else {
                            selection_gesture = Some((
                                signature,
                                self.doc.selection,
                                self.doc.selection_polygon.clone(),
                                vec![target],
                                self.doc.selection_coverage.clone(),
                            ));
                        }
                    } else {
                        selection_gesture = Some((
                            signature,
                            self.doc.selection,
                            self.doc.selection_polygon.clone(),
                            vec![target],
                            self.doc.selection_coverage.clone(),
                        ));
                    }
                } else {
                    selection_gesture = None;
                }
                let applied = if c["op"] == "layer.paste" {
                    if let Some(snapshot) = clipboard.take() {
                        crate::layer_clipboard::paste(
                            &mut self.doc,
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
                        prepared.apply(&mut self.doc)
                    } else {
                        self.apply(c)
                    }
                } else {
                    self.apply(c)
                };
                if let Err(err) = applied {
                    self.doc = before;
                    return Err(err);
                }
            }
        }
        self.doc.ensure_depth();
        let bytes: usize = self
            .doc
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
            self.doc = before;
            return Err(
                "Document exceeded the initial 512 MiB raster budget; edit was rolled back.".into(),
            );
        }
        if let Err(error) = crate::mask::validate_budget(&self.doc.layers) {
            self.doc = before;
            return Err(error);
        }
        if let Err(error) = crate::effects::validate_budget(&self.doc) {
            self.doc = before;
            return Err(error);
        }
        if let Err(error) = crate::compositor::validate_clipping(&self.doc) {
            self.doc = before;
            return Err(error);
        }
        if self.doc.bit_depth == 16 {
            if let Err(error) = crate::depth16::validate_budget(&self.doc) {
                self.doc = before;
                return Err(error);
            }
        }
        crate::effects::invalidate(&mut self.doc, commands);
        self.doc.revision = before.revision + 1;
        let revision = self.doc.revision;
        let created: Vec<String> = self
            .doc
            .layers
            .iter()
            .filter(|l| !before.layers.iter().any(|old| old.id == l.id))
            .map(|l| l.id.clone())
            .collect();
        let created_roots = pasted_roots.unwrap_or_else(|| {
            self.doc
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
        let tool = match text(command, "op", "") {
            "paint" if command["erase"] == true => "eraser",
            "paint" => "brush",
            "smudge" => "smudge",
            "clone" => "clone",
            "heal" => "heal",
            "crop" => "selection",
            "canvas.resize" | "image.resize" | "resize" => "scale",
            "liquify.stroke" => "liquify",
            "adjustment.add" => "effects",
            op if op.starts_with("source.") => "layers",
            "move" => "move",
            "transform" if num(command, "angle", 0.0) != 0.0 => "rotate",
            "transform" => "scale",
            "image.place" | "image.import" | "image.paste" | "image.patch" => "place",
            "layer.update" if command.get("opacity").is_some() => "opacity",
            "fill" | "paint.fill" => "fill",
            op if op.starts_with("effect.") => "effects",
            op if op.starts_with("mask.") => "mask",
            "selection" => "selection",
            op if op.starts_with("selection.") => "selection",
            op if op.starts_with("layer.") => "layers",
            _ => "canvas",
        };
        self.mark_ai(actor, label, tool, visual_scopes);
        let continuous = gesture.is_some()
            && self.undo.last().is_some_and(|h| {
                h.actor == actor
                    && h.task.as_deref() == task
                    && h.gesture.as_deref() == gesture
                    && h.after.revision == before.revision
                    && h.after.id == before.id
            });
        if continuous {
            let history = self.undo.last_mut().unwrap();
            history.after = self.doc.clone();
            history.scopes.extend(scopes.clone());
            history.scopes.dedup();
        } else {
            self.undo.push(History {
                before,
                after: self.doc.clone(),
                actor: actor.into(),
                label: label.into(),
                task: task.map(String::from),
                scopes: scopes.clone(),
                gesture: gesture.map(String::from),
                reverted: vec![],
            });
        }
        self.trim_history();
        self.redo.clear();
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
            json!({"revision":revision,"applied":commands.len(),"created":created,"created_roots":created_roots,"layers":self.doc.layers.iter().map(|l|json!({"id":l.id,"name":l.name})).collect::<Vec<_>>()}),
        )
    }
    fn apply(&mut self, c: &Value) -> Result<(), String> {
        // Reframe before taking native-original and soft-selection baselines. The
        // outer transaction still owns the complete pre-gesture undo snapshot.
        self.doc.ensure_depth();
        crate::edit_bounds::prepare(&mut self.doc, c)?;
        let before = crate::retained::has_originals(&self.doc).then(|| self.doc.clone());
        self.apply_retained(c)?;
        if let Some(before) = before {
            let whole_transform = matches!(c["op"].as_str(), Some("move" | "transform"))
                && (before.selection.is_none() || c["selection_only"] == false);
            if !whole_transform {
                crate::retained::reconcile(&before, &mut self.doc);
            }
        }
        Ok(())
    }
    fn apply_retained(&mut self, c: &Value) -> Result<(), String> {
        if c.get("source_revision")
            .is_some_and(|r| r.as_u64() != Some(self.doc.revision))
            || c.get("document_id")
                .is_some_and(|id| id.as_str() != Some(self.doc.id.as_str()))
        {
            return Err(
                "The source project changed. Review it again before applying this result.".into(),
            );
        }
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
            )
        ) {
            if let Some(coverage) = self
                .doc
                .selection_coverage
                .clone()
                .filter(|m| self.doc.selection == Some(m.bounds))
            {
                let before = self.doc.clone();
                self.apply_inner(c)?;
                crate::selection::restrict_document(&mut self.doc, &before, &coverage);
                return Ok(());
            }
        }
        self.apply_inner(c)
    }
    fn apply_inner(&mut self, c: &Value) -> Result<(), String> {
        self.doc.ensure_depth();
        let op = text(c, "op", "");
        let target = text(c, "layer", "");
        if op == "source.add" {
            return crate::source::add(&mut self.doc, c);
        }
        if ["crop", "canvas.resize", "image.resize", "resize"].contains(&op) {
            return crate::geometry::apply(&mut self.doc, c);
        }
        if c.get("rect")
            .is_some_and(|r| !r.is_null() && rect(r).is_none())
        {
            return Err("Invalid rectangle coordinates".into());
        }
        if op == "document.settings" {
            let w = c["width"].as_u64().unwrap_or(self.doc.width as u64);
            let h = c["height"].as_u64().unwrap_or(self.doc.height as u64);
            if w > u32::MAX as u64 || h > u32::MAX as u64 {
                return Err("Invalid canvas dimensions".into());
            }
            check_size(w as u32, h as u32)?;
            let depth = c["bit_depth"].as_u64().unwrap_or(self.doc.bit_depth as u64);
            if ![8, 16].contains(&depth) {
                return Err("Choose 8 or 16 bit channels".into());
            }
            if depth != self.doc.bit_depth as u64 {
                for layer in &mut self.doc.layers {
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
            self.doc.width = w as u32;
            self.doc.height = h as u32;
            self.doc.bit_depth = depth as u16;
            self.doc.selection = None;
            self.doc.selection_polygon = None;
            self.doc.selection_coverage = None;
            self.doc.selection_previous = None;
            return Ok(());
        }
        if op == "adjustment.add" {
            crate::adjustment::add(&mut self.doc, c)?;
            return Ok(());
        }
        if op == "layer.duplicate" {
            let ids = crate::grouping::requested(c)?;
            crate::layer_clipboard::duplicate(&mut self.doc, &ids)?;
            return Ok(());
        }
        if op == "group.create_selected" {
            let ids = crate::grouping::requested(c)?;
            crate::grouping::create(&mut self.doc, &ids, text(c, "name", "Folder"))?;
            return Ok(());
        }
        if op == "layer.merge" {
            // Earlier commands in the same batch may have changed effect sources.
            crate::effects::invalidate(&mut self.doc, &[json!({"op":"layer.merge"})]);
            let ids = crate::merge::requested(c)?;
            let prepared = crate::merge::prepare(&self.doc, &ids, c["name"].as_str())?;
            return prepared.apply(&mut self.doc);
        }
        if op == "image.paste" {
            return crate::clipboard::paste(&mut self.doc, c);
        }
        if op == "image.place" {
            return crate::placement::place(&mut self.doc, c);
        }
        if op == "image.import" {
            if self.doc.layers.len() >= 100 {
                return Err("Initial version supports up to 100 layers".into());
            }
            let path = std::path::Path::new(text(c, "path", ""));
            let ext = path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !["png", "jpg", "jpeg"].contains(&ext.as_str()) {
                return Err("Import PNG or JPEG images; open PSDs as documents".into());
            }
            let mut reader = image::ImageReader::open(path)
                .map_err(|e| e.to_string())?
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(128 * 1024 * 1024);
            reader.limits(limits);
            let img = reader.decode().map_err(|e| e.to_string())?;
            let mut layer = Layer::new(
                &path.file_stem().unwrap_or_default().to_string_lossy(),
                "paint",
                img.width(),
                img.height(),
            );
            layer.pixels = if self.doc.bit_depth == 16
                || matches!(
                    img,
                    image::DynamicImage::ImageLuma16(_)
                        | image::DynamicImage::ImageLumaA16(_)
                        | image::DynamicImage::ImageRgb16(_)
                        | image::DynamicImage::ImageRgba16(_)
                ) {
                Raster::from_rgba16(img.width(), img.height(), img.to_rgba16().as_raw())?
            } else {
                Raster::from_rgba(img.width(), img.height(), img.to_rgba8().as_raw())?
            };
            self.doc.layers.insert(0, layer);
            return Ok(());
        }
        if op == "new" {
            self.doc = Document::new(
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
            return crate::selection::apply(&mut self.doc, c);
        }
        if op == "selection" {
            self.doc.selection_previous = crate::selection::current(&self.doc)
                .or_else(|| self.doc.selection_previous.clone());
            self.doc.selection_coverage = None;
            if let Some(polygon) = c.get("polygon").filter(|p| !p.is_null()) {
                let points: Vec<[f32; 2]> = serde_json::from_value(polygon.clone())
                    .map_err(|_| "Selection polygon must contain [x,y] points")?;
                let bounds = crate::selection::bounds(&points)?;
                if c.get("rect").and_then(rect).is_some_and(|r| r != bounds) {
                    return Err("Selection rectangle must match its polygon bounds".into());
                }
                self.doc.selection = Some(bounds);
                self.doc.selection_polygon = Some(points);
            } else {
                self.doc.selection = c.get("rect").and_then(rect);
                self.doc.selection_polygon = None;
            }
            return Ok(());
        }
        if op == "layer.add" {
            if self.doc.layers.len() >= 100 {
                return Err("Initial version supports up to 100 layers".into());
            }
            let kind = text(c, "kind", "paint");
            if !["paint", "fill", "group"].contains(&kind) {
                return Err("Unsupported layer kind".into());
            }
            let mut layer = Layer::new(
                text(c, "name", "New layer"),
                kind,
                self.doc.width,
                self.doc.height,
            );
            if c.get("color").is_some() {
                layer.color = color(c);
            }
            if let Some(parent) = c.get("parent").and_then(Value::as_str) {
                if !self
                    .doc
                    .layers
                    .iter()
                    .any(|l| l.id == parent && l.kind == "group")
                {
                    return Err("Parent must be an existing group".into());
                }
                layer.parent = Some(parent.into());
            }
            self.doc.layers.insert(0, layer);
            return Ok(());
        }
        let i = self
            .doc
            .layers
            .iter()
            .position(|l| l.id == target)
            .ok_or_else(|| format!("Unknown layer: {target}"))?;
        if visibility_only(c) {
            self.doc.layers[i].visible =
                c["visible"].as_bool().ok_or("Visibility must be boolean")?;
            return Ok(());
        }
        let mut parent = self.doc.layers[i].parent.as_deref();
        for _ in 0..16 {
            let Some(pid) = parent else {
                break;
            };
            let p = self
                .doc
                .layers
                .iter()
                .find(|l| l.id == pid)
                .ok_or("Invalid parent")?;
            if p.locked {
                return Err("Parent group is locked".into());
            }
            parent = p.parent.as_deref();
        }
        if self.doc.layers[i].locked && op != "layer.update" {
            return Err("Layer is locked".into());
        }
        if op == "source.update" {
            return crate::source::update(&mut self.doc.layers[i], c);
        }
        if op == "source.rasterize" {
            if self.doc.layers[i].source.take().is_none() {
                return Err("This layer has no editable source".into());
            }
            return Ok(());
        }
        if self.doc.layers[i].source.is_some() {
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
                        | "shape"
                        | "gradient"
                        | "adjust"
                        | "image.patch"
                )
            {
                return Err("Edit this layer's text/vector properties, paint its mask, or explicitly rasterize it first".into());
            }
            if matches!(op, "move" | "transform")
                && (mask || (self.doc.selection.is_some() && c["selection_only"] != false))
            {
                return Err("Transform the complete editable text/vector layer with selection_only:false, or rasterize it for a pixel/mask transform".into());
            }
        }
        if op == "layer.delete" {
            let mut ids = vec![target.to_string()];
            loop {
                let count = ids.len();
                for l in &self.doc.layers {
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
                .doc
                .layers
                .iter()
                .any(|l| ids.contains(&l.id) && l.locked)
            {
                return Err("Unlock the selected layers and their children before deleting".into());
            }
            if self.doc.layers.iter().any(|l| {
                l.clip_to.as_ref().is_some_and(|b| ids.contains(b)) && !ids.contains(&l.id)
            }) {
                return Err("Release clipping or select the complete clipping group before deleting its base".into());
            }
            self.doc.layers.retain(|l| !ids.contains(&l.id));
            return Ok(());
        }
        if op == "layer.clip" {
            let base = c.get("base").and_then(Value::as_str).map(String::from);
            self.doc.layers[i].clip_to = base;
            crate::compositor::validate_clipping(&self.doc)?;
            return Ok(());
        }
        if op == "layer.reorder" {
            let at = num(c, "index", 0.0) as usize;
            if at >= self.doc.layers.len() {
                return Err("Invalid layer index".into());
            }
            let l = self.doc.layers.remove(i);
            self.doc.layers.insert(at, l);
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
                    .doc
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
            self.doc.layers[i].parent = parent.map(String::from);
            return Ok(());
        }
        if op == "layer.update" {
            let l = &mut self.doc.layers[i];
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
            return Ok(());
        }
        if op == "mask.add" {
            let layer = &mut self.doc.layers[i];
            if layer.mask.is_some() {
                return Err("Layer already has a mask".into());
            }
            layer.mask = Some(Mask {
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
            self.doc.layers[i].mask = None;
            return Ok(());
        }
        if op == "mask.refine" || op == "mask.from_selection" {
            return crate::selection::masks::apply(&mut self.doc, i, c);
        }
        if op == "mask.from_color" {
            if self.doc.bit_depth == 16 {
                let image = crate::depth16::layer_image(&self.doc, i)?;
                let mask = crate::depth16::mask_image(&self.doc, i)?;
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
                    &mut self.doc.layers[i],
                    &source,
                    c,
                    mask.as_deref(),
                );
            }
            let l = &self.doc.layers[i];
            let masks = self
                .doc
                .layers
                .iter()
                .map(|l| {
                    l.mask
                        .as_ref()
                        .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
                })
                .collect::<Vec<_>>();
            let colors = crate::effects::prepare(&self.doc, &masks)?;
            let source = if let Some(image) = &colors[i] {
                image.clone()
            } else {
                let (w, h) = if l.kind == "group" {
                    (self.doc.width, self.doc.height)
                } else {
                    (l.pixels.width, l.pixels.height)
                };
                let bytes = if l.kind == "group" {
                    self.doc.preview(None, w.max(h), Some(&l.id), false)?.2
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
            let source = if ["group", "adjustment"].contains(&l.kind.as_str()) {
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
            crate::smart_mask::from_color(&mut self.doc.layers[i], &source, c)?;
            return Ok(());
        }
        if op.starts_with("effect.") {
            let l = &mut self.doc.layers[i];
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
                    let settings = crate::effects::normalized(kind, &settings)?;
                    l.effects.push(crate::effects::Effect {
                        weight: crate::effects::command_weight(c)?,
                        id: id(),
                        kind: kind.into(),
                        enabled: true,
                        settings,
                    });
                }
                "effect.update" => {
                    let e = l
                        .effects
                        .iter_mut()
                        .find(|e| e.id == text(c, "effect", ""))
                        .ok_or("Unknown effect")?;
                    if let Some(v) = c.get("enabled").and_then(Value::as_bool) {
                        e.enabled = v;
                    }
                    if c.get("weight").is_some() {
                        e.weight = crate::effects::command_weight(c)?;
                    }
                    if let Some(settings) = c.get("settings") {
                        e.settings = crate::effects::normalized(&e.kind, settings)?;
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
            let l = &mut self.doc.layers[i];
            let m = l.mask.as_mut().ok_or("Layer has no mask")?;
            if op != "mask.toggle" {
                m.cache_key = id();
            }
            match op {
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
                    let kind = text(c, "kind", "invert");
                    if !crate::effects::catalog::get(kind).is_some_and(|entry| entry.supports(true))
                    {
                        return Err(
                            "Unsupported mask effect; inspect the shared effect catalog".into()
                        );
                    }
                    m.steps.push(MaskStep {
                        weight: crate::effects::command_weight(c)?,
                        id: id(),
                        kind: kind.into(),
                        enabled: true,
                        value: mask_parameter(
                            kind,
                            num(
                                c,
                                "value",
                                if kind == "levels" {
                                    1.0
                                } else if kind == "blur" {
                                    8.0
                                } else {
                                    255.0
                                },
                            ),
                        )?,
                        pixels: Raster::new_depth(l.pixels.width, l.pixels.height, l.pixels.depth),
                        settings: if ["curves", "adjust", "gaussian"].contains(&kind) {
                            let effect_kind = if kind == "gaussian" { "blur" } else { kind };
                            let v = c
                                .get("settings")
                                .cloned()
                                .unwrap_or_else(|| crate::effects::defaults(effect_kind));
                            crate::effects::validate(effect_kind, &v)?;
                            v
                        } else {
                            Value::Null
                        },
                    });
                }
                "mask.step.update" => {
                    let s = m
                        .steps
                        .iter_mut()
                        .find(|s| s.id == text(c, "step", ""))
                        .ok_or("Unknown mask step")?;
                    if let Some(v) = c.get("enabled").and_then(Value::as_bool) {
                        s.enabled = v;
                    }
                    if let Some(settings) = c.get("settings") {
                        let kind = if s.kind == "gaussian" {
                            "blur"
                        } else {
                            s.kind.as_str()
                        };
                        crate::effects::validate(kind, settings)?;
                        s.settings = settings.clone();
                    }
                    if c.get("weight").is_some() {
                        s.weight = crate::effects::command_weight(c)?;
                    }
                    if c.get("value").is_some() {
                        s.value = mask_parameter(&s.kind, num(c, "value", 1.0))?;
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
        if self.doc.layers[i].kind == "adjustment"
            && (!c["mask"].as_bool().unwrap_or(false)
                || ["move", "transform", "liquify.stroke"].contains(&op))
        {
            return Err("Edit the adjustment effect settings or paint on its mask".into());
        }
        if op == "liquify.stroke" {
            if c["mask"].as_bool().unwrap_or(false) {
                return Err("Liquify currently edits color; choose the Color channel".into());
            }
            let (points, pressures) = crate::brush::points_from_command(c)?;
            if pressures.is_some() {
                return Err("Liquify uses radius and strength, not per-point pressure".into());
            }
            let l = &self.doc.layers[i];
            let x = if l.kind == "group" { 0 } else { l.x };
            let y = if l.kind == "group" { 0 } else { l.y };
            let points: Vec<_> = points
                .iter()
                .map(|p| [p[0] - x as f32, p[1] - y as f32])
                .collect();
            let selection = self
                .doc
                .selection
                .map(|s| [s[0] - x, s[1] - y, s[2] - x, s[3] - y]);
            let polygon = crate::selection::polygon(&self.doc).map(|p| {
                p.iter()
                    .map(|p| [p[0] - x as f32, p[1] - y as f32])
                    .collect::<Vec<_>>()
            });
            let coverage = self
                .doc
                .selection_coverage
                .as_ref()
                .filter(|m| self.doc.selection == Some(m.bounds))
                .map(|m| m.local(x, y));
            if coverage
                .as_ref()
                .is_some_and(|m| m.bounds[0] >= m.bounds[2])
            {
                return Ok(());
            }
            let stroke = json!({"coverage":coverage,"mode":text(c,"mode","push"),"points":points,"radius":num(c,"radius",40.),"strength":num(c,"strength",0.5),"selection":selection,"polygon":polygon});
            let l = &mut self.doc.layers[i];
            let at = if let Some(effect) = c["effect"].as_str() {
                l.effects
                    .iter()
                    .position(|e| e.id == effect && e.kind == "liquify" && e.enabled)
                    .ok_or("Choose an enabled liquify effect")?
            } else if let Some(at) = l
                .effects
                .iter()
                .rposition(|e| e.kind == "liquify" && e.enabled)
            {
                at
            } else {
                if l.effects.len() >= 32 {
                    return Err("Effect stack limit: 32".into());
                }
                l.effects.push(crate::effects::Effect {
                    weight: 1.0,
                    id: id(),
                    kind: "liquify".into(),
                    enabled: true,
                    settings: crate::liquify::defaults(),
                });
                l.effects.len() - 1
            };
            let mut settings = crate::effects::normalized("liquify", &l.effects[at].settings)?;
            settings["strokes"]
                .as_array_mut()
                .ok_or("Invalid liquify stroke list")?
                .push(stroke);
            crate::liquify::validate(&settings)?;
            l.effects[at].settings = settings;
            return Ok(());
        }
        if op == "paint.fill" {
            return crate::fill::apply(&mut self.doc, c);
        }
        let selection_coverage = self
            .doc
            .selection_coverage
            .clone()
            .filter(|m| self.doc.selection == Some(m.bounds));
        let selection_polygon = crate::selection::polygon(&self.doc).map(Vec::from);
        let selection = self.doc.selection;
        let canvas = [0, 0, self.doc.width as i32, self.doc.height as i32];
        if op == "fill"
            && self.doc.layers[i].kind == "group"
            && !c["mask"].as_bool().unwrap_or(false)
        {
            if self.doc.layers.len() >= 100 {
                return Err("Initial version supports up to 100 layers".into());
            }
            let mut child = Layer::new("Foreground fill", "paint", self.doc.width, self.doc.height);
            child.parent = Some(target.into());
            let area = selection.unwrap_or([0, 0, self.doc.width as i32, self.doc.height as i32]);
            let col = color(c);
            for y in area[1].max(0)..area[3].min(self.doc.height as i32) {
                for x in area[0].max(0)..area[2].min(self.doc.width as i32) {
                    if selection_polygon.as_ref().is_some_and(|points| {
                        !crate::selection::contains(points, x as f32 + 0.5, y as f32 + 0.5)
                    }) {
                        continue;
                    }
                    child.pixels.set(x, y, col);
                }
            }
            self.doc.layers.insert(i + 1, child);
            return Ok(());
        }
        if ["move", "transform"].contains(&op) && self.doc.layers[i].kind == "group" {
            return crate::transform::folder(&mut self.doc, target, c);
        }
        if ["clone", "heal"].contains(&op) {
            return crate::retouch::apply(&mut self.doc, i, c);
        }
        let layer = &mut self.doc.layers[i];
        if ["move", "transform"].contains(&op) && c["selection_only"].as_bool() != Some(false) {
            if let Some(coverage) = selection_coverage {
                let transformed = crate::transform::selection_with_coverage(layer, &coverage, c)?;
                self.doc.selection = Some(transformed.bounds);
                self.doc.selection_polygon = None;
                self.doc.selection_coverage = Some(transformed);
                return Ok(());
            }
            if let Some(area) = selection {
                let polygon =
                    selection_polygon.unwrap_or_else(|| crate::selection::rectangle(area));
                let (bounds, polygon) =
                    crate::transform::selection_with_polygon(layer, &polygon, c)?;
                self.doc.selection = Some(bounds);
                self.doc.selection_polygon = Some(polygon);
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
        if op == "fill" && !c["mask"].as_bool().unwrap_or(false) {
            if layer.kind == "fill" && selection.is_none() {
                layer.color = color(c);
                return Ok(());
            }
            if layer.kind == "fill" {
                let col = layer.color;
                for y in 0..layer.pixels.height {
                    for x in 0..layer.pixels.width {
                        layer.pixels.set(x as i32, y as i32, col);
                    }
                }
                layer.kind = "paint".into();
            }
        }
        if (layer.kind == "group" || layer.kind == "fill" || layer.kind == "adjustment")
            && !c.get("mask").and_then(Value::as_bool).unwrap_or(false)
        {
            return Err("Paint on a paint layer or its mask".into());
        }
        if ["paint", "smudge"].contains(&op) {
            let (mut points, pressures) = crate::brush::points_from_command(c)?;
            let mut brush = color(c);
            let is_mask = c.get("mask").and_then(Value::as_bool).unwrap_or(false);
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
            if is_mask {
                let v = brush[0];
                brush = [v, v, v, brush[3]];
            }
            let settings = crate::brush::Settings::from_command(c)?;
            for point in &mut points {
                point[0] -= x as f32;
                point[1] -= y as f32;
            }
            let raster = edit_raster(layer, c)?;
            let polygon = selection_polygon.as_ref().map(|points| {
                points
                    .iter()
                    .map(|p| [p[0] - x as f32, p[1] - y as f32])
                    .collect::<Vec<_>>()
            });
            if op == "smudge" {
                crate::smudge::paint(
                    raster,
                    &points,
                    pressures.as_deref(),
                    settings,
                    brush,
                    clip,
                    polygon.as_deref(),
                )?;
            } else {
                crate::brush::paint_with_pressure(
                    raster,
                    &points,
                    pressures.as_deref(),
                    settings,
                    brush,
                    c["erase"].as_bool().unwrap_or(false),
                    clip,
                    polygon.as_deref(),
                )?;
            }
            return Ok(());
        }
        if op == "fill" || op == "shape" || op == "gradient" {
            let area = c.get("rect").and_then(rect).or(selection).unwrap_or(canvas);
            let mut col = color(c);
            if c.get("mask").and_then(Value::as_bool).unwrap_or(false) {
                col = [col[0], col[0], col[0], col[3]];
            }
            let (lx, ly, lw, lh) = (
                layer.x,
                layer.y,
                layer.pixels.width as i32,
                layer.pixels.height as i32,
            );
            let raster = edit_raster(layer, c)?;
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
                    let mut col = col;
                    if op == "shape" && text(c, "kind", "rectangle") == "ellipse" {
                        let rx = (area[2] - area[0]) as f32 / 2.0;
                        let ry = (area[3] - area[1]) as f32 / 2.0;
                        if rx <= 0.0
                            || ry <= 0.0
                            || ((xx as f32 - area[0] as f32 - rx) / rx).powi(2)
                                + ((yy as f32 - area[1] as f32 - ry) / ry).powi(2)
                                > 1.0
                        {
                            continue;
                        }
                    }
                    if raster.depth == 16 {
                        let mut native = col.map(|v| u16::from(v) * 257);
                        if op == "gradient" {
                            let t = (xx - area[0]) as f64 / (area[2] - area[0]).max(1) as f64;
                            native[3] = (f64::from(native[3]) * (1.0 - t)).round() as u16;
                        }
                        raster.set16(
                            tx,
                            ty,
                            crate::raster::blend16(raster.get16(tx, ty), native, 1.0, "normal"),
                        );
                    } else {
                        if op == "gradient" {
                            let t = (xx - area[0]) as f32 / (area[2] - area[0]).max(1) as f32;
                            col[3] = (col[3] as f32 * (1.0 - t)) as u8;
                        }
                        let dst = raster.get(tx, ty);
                        raster.set(tx, ty, blend(dst, col, 1.0, "normal"));
                    }
                }
            }
            return Ok(());
        }
        if c.get("mask").and_then(Value::as_bool).unwrap_or(false) {
            return Err("This operation does not support masks; use paint, fill, shape, gradient, or mask effects.".into());
        }
        if op == "adjust" {
            if layer.pixels.depth == 16 {
                let brightness = num(c, "brightness", 0.0).clamp(-1.0, 1.0);
                let contrast = num(c, "contrast", 1.0).clamp(0.0, 3.0);
                let saturation = num(c, "saturation", 1.0).clamp(0.0, 3.0);
                for tile in layer.pixels.samples16.values_mut() {
                    for p in Arc::make_mut(tile).chunks_exact_mut(4) {
                        let gray = f64::from(p[0]) * 0.2126
                            + f64::from(p[1]) * 0.7152
                            + f64::from(p[2]) * 0.0722;
                        for c in 0..3 {
                            p[c] = ((gray + (f64::from(p[c]) - gray) * saturation - 32767.5)
                                * contrast
                                + 32767.5
                                + brightness * 65535.0)
                                .clamp(0.0, 65535.0)
                                .round() as u16;
                        }
                    }
                }
                return Ok(());
            }
            let brightness = num(c, "brightness", 0.0).clamp(-1.0, 1.0) as f32;
            let contrast = num(c, "contrast", 1.0).clamp(0.0, 3.0) as f32;
            let saturation = num(c, "saturation", 1.0).clamp(0.0, 3.0) as f32;
            for tile in layer.pixels.tiles.values_mut() {
                for p in Arc::make_mut(tile).chunks_exact_mut(4) {
                    let gray = p[0] as f32 * 0.2126 + p[1] as f32 * 0.7152 + p[2] as f32 * 0.0722;
                    for c in 0..3 {
                        p[c] = ((gray + (p[c] as f32 - gray) * saturation - 127.5) * contrast
                            + 127.5
                            + brightness * 255.0)
                            .clamp(0.0, 255.0) as u8;
                    }
                }
            }
            return Ok(());
        }
        if op == "image.patch" {
            let path = text(c, "path", "");
            let img = image::ImageReader::open(path)
                .map_err(|e| e.to_string())?
                .with_guessed_format()
                .map_err(|e| e.to_string())?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(8192);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(128 * 1024 * 1024);
            let mut img = img;
            img.limits(limits);
            let img = img.decode().map_err(|e| e.to_string())?;
            let ox = num(c, "x", 0.0) as i32 - layer.x;
            let oy = num(c, "y", 0.0) as i32 - layer.y;
            if layer.pixels.depth == 16 {
                for (x, y, p) in img.to_rgba16().enumerate_pixels() {
                    let tx = ox + x as i32;
                    let ty = oy + y as i32;
                    layer.pixels.set16(
                        tx,
                        ty,
                        crate::raster::blend16(layer.pixels.get16(tx, ty), p.0, 1.0, "normal"),
                    );
                }
                return Ok(());
            }
            let img = img.to_rgba8();
            for (x, y, p) in img.enumerate_pixels() {
                let tx = ox + x as i32;
                let ty = oy + y as i32;
                layer
                    .pixels
                    .set(tx, ty, blend(layer.pixels.get(tx, ty), p.0, 1.0, "normal"));
            }
            return Ok(());
        }
        Err(format!("Unsupported operation: {op}"))
    }
    pub fn undo(&mut self, actor: &str) -> Result<(), String> {
        self.ensure_open()?;
        let h = self.undo.last().ok_or("Nothing to undo")?.clone();
        if actor != "human" && h.actor != actor {
            return Err("Latest change belongs to another participant. Observe again or inspect selective task undo.".into());
        }
        self.check(actor, &h.scopes)?;
        let revision = self.doc.revision + 1;
        self.doc = h.before.clone();
        self.doc.revision = revision;
        self.undo.pop();
        self.changes.push((revision, h.scopes.clone()));
        self.mark_ai(actor, "Undo AI edit", "history", h.scopes.clone());
        self.redo.push(h);
        Ok(())
    }
    pub fn redo(&mut self, actor: &str) -> Result<(), String> {
        self.ensure_open()?;
        let h = self.redo.last().ok_or("Nothing to redo")?.clone();
        if actor != "human" && h.actor != actor {
            return Err("Redo belongs to another participant".into());
        }
        self.check(actor, &h.scopes)?;
        let revision = self.doc.revision + 1;
        self.doc = h.after.clone();
        self.doc.revision = revision;
        self.redo.pop();
        self.changes.push((revision, h.scopes.clone()));
        self.mark_ai(actor, "Redo AI edit", "history", h.scopes.clone());
        self.undo.push(h);
        Ok(())
    }
    pub fn replace(
        &mut self,
        mut doc: Document,
        path: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        self.ensure_open()?;
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
        self.doc = doc;
        self.path = path;
        self.saved_revision = self.doc.revision;
        self.undo.clear();
        self.redo.clear();
        self.changes.clear();
        self.truncated_tasks.clear();
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

fn mask_parameter(kind: &str, value: f64) -> Result<f32, String> {
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
        let step = if let Some(id) = requested {
            mask.steps
                .iter_mut()
                .find(|s| s.id == id && s.kind == "paint")
        } else {
            mask.steps.iter_mut().rev().find(|s| s.kind == "paint")
        };
        Ok(&mut step.ok_or("Add a paint step to the mask")?.pixels)
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
