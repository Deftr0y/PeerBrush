use crate::raster::{blend, check_size, png, Pixel, Raster};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
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
            if !step.enabled {
                continue;
            }
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
        }
        v.clamp(0.0, 255.0) / 255.0
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Document {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub revision: u64,
    pub layers: Vec<Layer>,
    pub selection: Option<[i32; 4]>,
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
            layers: vec![Layer::new("Paint 1", "paint", w, h)],
            selection: None,
            read_only: false,
            warnings: vec![],
        })
    }
    pub(crate) fn sample_group(
        &self,
        parent: Option<&str>,
        x: i32,
        y: i32,
        prepared: &[Option<Arc<crate::mask::GrayMask>>],
        colors: &[Option<Arc<crate::effects::Image>>],
    ) -> Pixel {
        let mut out = [0; 4];
        for (index, l) in self
            .layers
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, l)| l.parent.as_deref() == parent)
        {
            if !l.visible {
                continue;
            }
            let p = if let Some(image) = &colors[index] {
                image.get(x - l.x, y - l.y)
            } else if l.kind == "group" {
                self.sample_group(Some(&l.id), x, y, prepared, colors)
            } else if l.kind == "fill" {
                if x - l.x >= 0
                    && y - l.y >= 0
                    && (x - l.x) < l.pixels.width as i32
                    && (y - l.y) < l.pixels.height as i32
                {
                    l.color
                } else {
                    [0; 4]
                }
            } else {
                l.pixels.get(x - l.x, y - l.y)
            };
            out = blend(
                out,
                p,
                l.opacity
                    * l.mask_value_prepared(x - l.x, y - l.y, prepared[index].as_deref(), false),
                &l.blend,
            );
        }
        out
    }
    pub fn preview(
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
        let target_index = target.and_then(|id| self.layers.iter().position(|l| l.id == id));
        let mut bytes = vec![0; w as usize * h as usize * 4];
        for y in 0..h {
            for x in 0..w {
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
                        image.get(sx - l.x, sy - l.y)
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
                        self.sample_group(Some(&l.id), sx, sy, &prepared, &colors)
                    } else {
                        l.pixels.get(sx - l.x, sy - l.y)
                    }
                } else {
                    self.sample_group(None, sx, sy, &prepared, &colors)
                };
                let i = ((y * w + x) * 4) as usize;
                bytes[i..i + 4].copy_from_slice(&p);
            }
        }
        Ok((w, h, bytes, rect))
    }
    pub fn export_png(&self) -> Result<Vec<u8>, String> {
        let (w, h, bytes, _) = self.preview(None, self.width.max(self.height), None, false)?;
        png(w, h, &bytes)
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
    pub doc: Document,
    pub leases: Vec<Lease>,
    pub undo: Vec<History>,
    pub redo: Vec<History>,
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
}
fn num(v: &Value, key: &str, default: f64) -> f64 {
    v.get(key).and_then(Value::as_f64).unwrap_or(default)
}
fn text<'a>(v: &'a Value, key: &str, default: &'a str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or(default)
}
fn color(v: &Value) -> Pixel {
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
        for command in commands {
            engine.apply(command)?;
        }
        crate::mask::validate_budget(&engine.doc.layers)?;
        crate::effects::validate_budget(&engine.doc)?;
        crate::effects::invalidate(&mut engine.doc, commands);
        Ok(engine.doc)
    }
    pub fn new() -> Self {
        Self {
            doc: Document::new(1024, 768).unwrap(),
            leases: vec![],
            undo: vec![],
            redo: vec![],
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
        }
    }
    pub fn expire(&mut self) {
        self.leases.retain(|l| l.expires > now());
        self.mcp_clients.retain(|_, expiry| *expiry > now());
    }
    fn mark_ai(&mut self, actor: &str, label: &str, tool: &str, scopes: Vec<Scope>) {
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
        self.expire();
        json!({"document":{"id":self.doc.id,"name":self.doc.name,"width":self.doc.width,"height":self.doc.height,"revision":self.doc.revision,"read_only":self.doc.read_only,"warnings":self.doc.warnings,"selection":self.doc.selection},"layers":self.doc.layers.iter().map(|l|json!({"id":l.id,"name":l.name,"kind":l.kind,"parent":l.parent,"visible":l.visible,"locked":l.locked,"opacity":l.opacity,"blend":l.blend,"bounds":[l.x,l.y,l.x+l.pixels.width as i32,l.y+l.pixels.height as i32],"effects":l.effects,"mask":l.mask.as_ref().map(|m|json!({"enabled":m.enabled,"steps":m.steps.iter().map(|s|json!({"id":s.id,"kind":s.kind,"enabled":s.enabled,"value":s.value,"settings":s.settings})).collect::<Vec<_>>()}))})).collect::<Vec<_>>(),"reservations":self.leases,"ai_change":self.ai_change,"dirty":self.doc.revision!=self.saved_revision})
    }
    fn scope_overlap(&self, a: &Scope, b: &Scope) -> bool {
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
            "layer.add",
            "layer.reorder",
            "layer.delete",
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
        let area = if op == "paint" {
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
        if self.doc.read_only {
            return Err("This PSD is read-only. Create a compatible copy first.".into());
        }
        if commands.is_empty() || commands.len() > 100 {
            return Err("A batch must contain 1–100 commands".into());
        }
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
        for c in commands {
            if let Err(err) = self.apply(c) {
                self.doc = before;
                return Err(err);
            }
        }
        let bytes: usize = self
            .doc
            .layers
            .iter()
            .map(|l| {
                l.pixels.bytes()
                    + l.mask
                        .as_ref()
                        .map(|m| m.steps.iter().map(|s| s.pixels.bytes()).sum::<usize>())
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
            "move" => "move",
            "transform" if num(command, "angle", 0.0) != 0.0 => "rotate",
            "transform" => "scale",
            "image.place" | "image.import" | "image.paste" | "image.patch" => "place",
            "layer.update" if command.get("opacity").is_some() => "opacity",
            "fill" => "fill",
            op if op.starts_with("effect.") => "effects",
            op if op.starts_with("mask.") => "mask",
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
            });
        }
        if self.undo.len() > 40 {
            self.undo.remove(0);
        }
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
            json!({"revision":revision,"applied":commands.len(),"created":created,"layers":self.doc.layers.iter().map(|l|json!({"id":l.id,"name":l.name})).collect::<Vec<_>>()}),
        )
    }
    fn apply(&mut self, c: &Value) -> Result<(), String> {
        let op = text(c, "op", "");
        let target = text(c, "layer", "");
        if c.get("rect")
            .is_some_and(|r| !r.is_null() && rect(r).is_none())
        {
            return Err("Invalid rectangle coordinates".into());
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
            let img = reader.decode().map_err(|e| e.to_string())?.to_rgba8();
            let mut layer = Layer::new(
                &path.file_stem().unwrap_or_default().to_string_lossy(),
                "paint",
                img.width(),
                img.height(),
            );
            layer.pixels = Raster::from_rgba(img.width(), img.height(), img.as_raw())?;
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
        if op == "selection" {
            self.doc.selection = c.get("rect").and_then(rect);
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
            self.doc.layers.retain(|l| !ids.contains(&l.id));
            return Ok(());
        }
        if op == "layer.duplicate" {
            if self.doc.layers[i].kind == "group" {
                return Err("Duplicate individual layers in this version".into());
            }
            let mut l = self.doc.layers[i].clone();
            l.id = id();
            l.name.push_str(" copy");
            self.doc.layers.insert(i, l);
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
                if ![
                    "normal", "multiply", "screen", "overlay", "darken", "lighten",
                ]
                .contains(&v)
                {
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
                        id: id(),
                        kind: "fill".into(),
                        enabled: true,
                        value: mask_parameter("fill", num(c, "value", 255.0))?,
                        pixels: Raster::new(layer.pixels.width, layer.pixels.height),
                        settings: Value::Null,
                    },
                    MaskStep {
                        id: id(),
                        kind: "paint".into(),
                        enabled: true,
                        value: 0.0,
                        pixels: Raster::new(layer.pixels.width, layer.pixels.height),
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
        if op == "mask.from_color" {
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
                    crate::effects::validate(kind, &settings)?;
                    l.effects.push(crate::effects::Effect {
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
                    if let Some(settings) = c.get("settings") {
                        crate::effects::validate(&e.kind, settings)?;
                        e.settings = settings.clone();
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
                    if ![
                        "paint", "fill", "invert", "levels", "blur", "curves", "adjust", "gaussian",
                    ]
                    .contains(&kind)
                    {
                        return Err(
                            "Supported mask steps: paint, fill, invert, levels, blur".into()
                        );
                    }
                    m.steps.push(MaskStep {
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
                        pixels: Raster::new(l.pixels.width, l.pixels.height),
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
        let selection = self.doc.selection;
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
                    child.pixels.set(x, y, col);
                }
            }
            self.doc.layers.insert(i + 1, child);
            return Ok(());
        }
        let layer = &mut self.doc.layers[i];
        if ["move", "transform"].contains(&op) && c["selection_only"].as_bool() != Some(false) {
            if let Some(area) = selection {
                self.doc.selection = Some(crate::transform::selection(layer, area, c)?);
                return Ok(());
            }
        }
        if op == "transform" || c.get("mask").and_then(Value::as_bool).unwrap_or(false) {
            if let Some(m) = &mut layer.mask {
                m.cache_key = id();
            }
        }
        if op == "transform" {
            if layer.kind == "group" {
                return Err("Transform individual layers in this initial version".into());
            }
            let angle = num(c, "angle", 0.0) as f32;
            let sx = num(c, "scale_x", 1.0) as f32;
            let sy = num(c, "scale_y", 1.0) as f32;
            if !angle.is_finite() || !(0.05..=20.0).contains(&sx) || !(0.05..=20.0).contains(&sy) {
                return Err("Transform scale must be 0.05–20 and rotation must be finite".into());
            }
            let pivot = c.get("pivot").and_then(Value::as_array);
            let px = pivot
                .and_then(|v| v.first())
                .and_then(Value::as_f64)
                .unwrap_or(layer.x as f64 + layer.pixels.width as f64 / 2.0)
                as f32;
            let py = pivot
                .and_then(|v| v.get(1))
                .and_then(Value::as_f64)
                .unwrap_or(layer.y as f64 + layer.pixels.height as f64 / 2.0)
                as f32;
            if !px.is_finite() || !py.is_finite() || px.abs() > 100000.0 || py.abs() > 100000.0 {
                return Err("Invalid transform pivot".into());
            }
            let (mut sin, mut cos) = angle.to_radians().sin_cos();
            if sin.abs() < 0.000001 {
                sin = 0.0;
            }
            if cos.abs() < 0.000001 {
                cos = 0.0;
            }
            let forward = |x: f32, y: f32| {
                let (x, y) = ((x - px) * sx, (y - py) * sy);
                [px + x * cos - y * sin, py + x * sin + y * cos]
            };
            let (x, y, w, h) = (layer.x, layer.y, layer.pixels.width, layer.pixels.height);
            let b = if layer.kind == "paint" {
                layer
                    .pixels
                    .content_bounds()
                    .unwrap_or([0, 0, w as i32, h as i32])
            } else {
                [0, 0, w as i32, h as i32]
            };
            let corners = [
                forward((x + b[0]) as f32, (y + b[1]) as f32),
                forward((x + b[2]) as f32, (y + b[1]) as f32),
                forward((x + b[2]) as f32, (y + b[3]) as f32),
                forward((x + b[0]) as f32, (y + b[3]) as f32),
            ];
            let left = corners
                .iter()
                .map(|p| p[0])
                .fold(f32::INFINITY, f32::min)
                .floor() as i32;
            let top = corners
                .iter()
                .map(|p| p[1])
                .fold(f32::INFINITY, f32::min)
                .floor() as i32;
            let right = corners
                .iter()
                .map(|p| p[0])
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil() as i32;
            let bottom = corners
                .iter()
                .map(|p| p[1])
                .fold(f32::NEG_INFINITY, f32::max)
                .ceil() as i32;
            let (nw, nh) = ((right - left) as u32, (bottom - top) as u32);
            check_size(nw, nh)?;
            let resample = |source: &Raster, fill: Option<Pixel>| {
                let mut out = Raster::new(nw, nh);
                for yy in 0..nh {
                    for xx in 0..nw {
                        let (dx, dy) = (
                            left as f32 + xx as f32 + 0.5 - px,
                            top as f32 + yy as f32 + 0.5 - py,
                        );
                        let ux = (dx * cos + dy * sin) / sx + px - x as f32 - 0.5;
                        let uy = (-dx * sin + dy * cos) / sy + py - y as f32 - 0.5;
                        let p = if let Some(col) = fill {
                            if ux >= -0.5
                                && uy >= -0.5
                                && ux < w as f32 - 0.5
                                && uy < h as f32 - 0.5
                            {
                                col
                            } else {
                                [0; 4]
                            }
                        } else {
                            source.sample(ux, uy)
                        };
                        out.set(xx as i32, yy as i32, p);
                    }
                }
                out
            };
            layer.pixels = resample(
                &layer.pixels,
                if layer.kind == "fill" {
                    Some(layer.color)
                } else {
                    None
                },
            );
            if let Some(mask) = &mut layer.mask {
                for step in &mut mask.steps {
                    step.pixels = if step.kind == "paint" {
                        resample(&step.pixels, None)
                    } else {
                        Raster::new(nw, nh)
                    };
                }
            }
            layer.kind = "paint".into();
            layer.x = left;
            layer.y = top;
            return Ok(());
        }
        if op == "move" {
            if layer.kind == "group" {
                return Err("Move the group's individual layers in this initial version".into());
            }
            let nx = layer.x as f64 + num(c, "dx", 0.0);
            let ny = layer.y as f64 + num(c, "dy", 0.0);
            if !nx.is_finite() || !ny.is_finite() || nx.abs() > 100000.0 || ny.abs() > 100000.0 {
                return Err("Move is outside the initial coordinate limits".into());
            }
            layer.x = nx.round() as i32;
            layer.y = ny.round() as i32;
            return Ok(());
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
        if (layer.kind == "group" || layer.kind == "fill")
            && !c.get("mask").and_then(Value::as_bool).unwrap_or(false)
        {
            return Err("Paint on a paint layer or its mask".into());
        }
        if op == "paint" {
            let points = c
                .get("points")
                .and_then(Value::as_array)
                .ok_or("Missing stroke points")?;
            if points.is_empty() || points.len() > 10000 {
                return Err("Invalid stroke length".into());
            }
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
            let points = points
                .iter()
                .map(|p| {
                    let a = p.as_array().ok_or("Invalid stroke point")?;
                    if a.len() != 2 {
                        return Err("Stroke points must be [x,y]");
                    }
                    Ok([
                        a[0].as_f64().ok_or("Invalid x")? as f32 - x as f32,
                        a[1].as_f64().ok_or("Invalid y")? as f32 - y as f32,
                    ])
                })
                .collect::<Result<Vec<_>, &str>>()?;
            let raster = edit_raster(layer, c)?;
            crate::brush::paint(
                raster,
                &points,
                settings,
                brush,
                c["erase"].as_bool().unwrap_or(false),
                clip,
            )?;
            return Ok(());
        }
        if op == "fill" || op == "shape" || op == "gradient" {
            let area = c.get("rect").and_then(rect).or(selection).unwrap_or([
                layer.x,
                layer.y,
                layer.x + layer.pixels.width as i32,
                layer.y + layer.pixels.height as i32,
            ]);
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
                    if op == "gradient" {
                        let t = (xx - area[0]) as f32 / (area[2] - area[0]).max(1) as f32;
                        col[3] = (col[3] as f32 * (1.0 - t)) as u8;
                    }
                    let dst = raster.get(tx, ty);
                    raster.set(tx, ty, blend(dst, col, 1.0, "normal"));
                }
            }
            return Ok(());
        }
        if c.get("mask").and_then(Value::as_bool).unwrap_or(false) {
            return Err("This operation does not support masks; use paint, fill, shape, gradient, or mask effects.".into());
        }
        if op == "adjust" {
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
            let img = img.decode().map_err(|e| e.to_string())?.to_rgba8();
            let ox = num(c, "x", 0.0) as i32 - layer.x;
            let oy = num(c, "y", 0.0) as i32 - layer.y;
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
        let h = self.undo.last().ok_or("Nothing to undo")?.clone();
        if actor != "human" && h.actor != actor {
            return Err("Latest change belongs to another participant. Observe again; selective task undo is planned.".into());
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
        doc: Document,
        path: Option<std::path::PathBuf>,
    ) -> Result<(), String> {
        self.check(
            "human",
            &[Scope {
                target: None,
                rect: None,
            }],
        )?;
        self.doc = doc;
        self.path = path;
        self.saved_revision = self.doc.revision;
        self.undo.clear();
        self.redo.clear();
        self.changes.clear();
        self.ai_change = None;
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
fn edit_raster<'a>(layer: &'a mut Layer, command: &Value) -> Result<&'a mut Raster, String> {
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
