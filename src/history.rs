//! Three-way task compensation. Snapshots stay chronological; human changes are never replayed.
use crate::{
    engine::{id, Document, Engine, History, Layer, Mask, MaskStep, Scope},
    raster::{Raster, TILE},
};
use serde_json::{json, Value};
use std::collections::BTreeSet;

#[derive(Default)]
struct Inverse {
    conflicts: Vec<Value>,
    scopes: Vec<Scope>,
}
impl Inverse {
    fn conflict(
        &mut self,
        target: Option<&str>,
        field: &str,
        rect: Option<[i32; 4]>,
        reason: &str,
    ) {
        self.conflicts
            .push(json!({"target":target,"field":field,"rect":rect,"reason":reason}));
    }
    fn scope(&mut self, target: Option<&str>, rect: Option<[i32; 4]>) {
        let scope = Scope {
            target: target.map(String::from),
            rect,
        };
        if !self.scopes.contains(&scope) {
            self.scopes.push(scope);
        }
    }
    fn field<T: PartialEq + Clone>(
        &mut self,
        before: &T,
        after: &T,
        current: &mut T,
        target: Option<&str>,
        field: &str,
    ) {
        if before == after {
            return;
        }
        if field == "layer.visible" {
            let visibility = target.map(|id| format!("@visibility:{id}"));
            self.scope(visibility.as_deref(), None);
        } else {
            self.scope(target, None);
        }
        if current == after {
            *current = before.clone();
        } else {
            self.conflict(target, field, None, "Changed after this task");
        }
    }
    // Small source metadata only: raster bytes never pass through JSON.
    fn value(
        &mut self,
        before: &Value,
        after: &Value,
        current: &mut Value,
        target: Option<&str>,
        field: &str,
    ) {
        if before == after {
            return;
        }
        if let (Some(b), Some(a), Some(c)) = (
            before.as_object(),
            after.as_object(),
            current.as_object_mut(),
        ) {
            let keys: BTreeSet<_> = b.keys().chain(a.keys()).cloned().collect();
            for key in keys {
                let path = format!("{field}.{key}");
                if let (Some(b), Some(a), Some(c)) = (b.get(&key), a.get(&key), c.get_mut(&key)) {
                    self.value(b, a, c, target, &path);
                } else if b.get(&key) != a.get(&key) {
                    self.scope(target, None);
                    if c.get(&key) == a.get(&key) {
                        if let Some(v) = b.get(&key) {
                            c.insert(key, v.clone());
                        } else {
                            c.remove(&key);
                        }
                    } else {
                        self.conflict(target, &path, None, "Setting changed after this task");
                    }
                }
            }
        } else if before.as_array().is_some_and(|v| id_array(v))
            && after.as_array().is_some_and(|v| id_array(v))
            && current.as_array().is_some_and(|v| id_array(v))
        {
            let b = before.as_array().unwrap();
            let a = after.as_array().unwrap();
            let c = current.as_array_mut().unwrap();
            let bid: Vec<String> = b.iter().map(value_id).collect();
            let aid: Vec<String> = a.iter().map(value_id).collect();
            for v in a {
                let key = value_id(v);
                if let Some(old) = b.iter().find(|v| value_id(v) == key) {
                    if let Some(now) = c.iter_mut().find(|v| value_id(v) == key) {
                        self.value(old, v, now, target, &format!("{field}.{key}"));
                    } else if old != v {
                        self.conflict(
                            target,
                            field,
                            None,
                            "Edited source was removed after this task",
                        );
                    }
                } else {
                    self.scope(target, None);
                    if let Some(at) = c.iter().position(|v| value_id(v) == key) {
                        if c[at] == *v {
                            c.remove(at);
                        } else {
                            self.conflict(
                                target,
                                field,
                                None,
                                "Task-created source has later edits",
                            );
                        }
                    }
                }
            }
            for (i, v) in b.iter().enumerate() {
                let key = value_id(v);
                if !aid.contains(&key) {
                    self.scope(target, None);
                    if c.iter().any(|v| value_id(v) == key) {
                        self.conflict(target, field, None, "Removed source ID was reused");
                    } else {
                        let at = insert_at(&bid, i, &c.iter().map(value_id).collect::<Vec<_>>());
                        c.insert(at, v.clone());
                    }
                }
            }
            let current_ids: Vec<_> = c.iter().map(value_id).collect();
            if let Some(order) = self.order(&bid, &aid, &current_ids, target, field) {
                let old = c.clone();
                *c = order
                    .iter()
                    .map(|id| old.iter().find(|v| value_id(v) == *id).unwrap().clone())
                    .collect();
            }
        } else {
            self.field(before, after, current, target, field);
        }
    }
    fn order(
        &mut self,
        before: &[String],
        after: &[String],
        current: &[String],
        target: Option<&str>,
        field: &str,
    ) -> Option<Vec<String>> {
        let common: BTreeSet<_> = before.iter().filter(|id| after.contains(id)).collect();
        let b: Vec<_> = before.iter().filter(|id| common.contains(id)).collect();
        let a: Vec<_> = after.iter().filter(|id| common.contains(id)).collect();
        if b == a {
            return None;
        }
        self.scope(target, None);
        let c: Vec<_> = current.iter().filter(|id| common.contains(id)).collect();
        if c != a {
            self.conflict(target, field, None, "Order changed after this task");
            return None;
        }
        let mut n = 0;
        Some(
            current
                .iter()
                .map(|id| {
                    if common.contains(id) {
                        let id = (*b[n]).clone();
                        n += 1;
                        id
                    } else {
                        id.clone()
                    }
                })
                .collect(),
        )
    }
    fn raster(
        &mut self,
        before: &Raster,
        after: &Raster,
        current: &mut Raster,
        target: &str,
        field: &str,
        origin: [i32; 2],
    ) {
        if before == after {
            return;
        }
        if (before.width, before.height, before.depth) != (after.width, after.height, after.depth) {
            self.field(before, after, current, Some(target), field);
            return;
        }
        if (current.width, current.height, current.depth)
            != (after.width, after.height, after.depth)
        {
            self.conflict(
                Some(target),
                field,
                None,
                "Raster frame or precision changed after this task",
            );
            return;
        }
        let keys: BTreeSet<_> = if before.depth == 16 {
            before
                .samples16
                .keys()
                .chain(after.samples16.keys())
                .copied()
                .collect()
        } else {
            before
                .tiles
                .keys()
                .chain(after.tiles.keys())
                .copied()
                .collect()
        };
        let mut changed = None;
        let mut conflicts = None;
        for (tx, ty) in keys {
            let same = if before.depth == 16 {
                before.samples16.get(&(tx, ty)) == after.samples16.get(&(tx, ty))
            } else {
                before.tiles.get(&(tx, ty)) == after.tiles.get(&(tx, ty))
            };
            if same {
                continue;
            }
            for y in ty * TILE..((ty + 1) * TILE).min(before.height) {
                for x in tx * TILE..((tx + 1) * TILE).min(before.width) {
                    let b = before.get16(x as i32, y as i32);
                    let a = after.get16(x as i32, y as i32);
                    if b == a {
                        continue;
                    }
                    let p = [x as i32 + origin[0], y as i32 + origin[1]];
                    extend(&mut changed, p);
                    if current.get16(x as i32, y as i32) != a {
                        extend(&mut conflicts, p);
                    } else if before.depth == 16 {
                        current.set16(x as i32, y as i32, b);
                    } else {
                        current.set(x as i32, y as i32, before.get(x as i32, y as i32));
                    }
                }
            }
        }
        if let Some(rect) = changed {
            self.scope(Some(target), Some(rect));
        }
        if let Some(rect) = conflicts {
            self.conflict(
                Some(target),
                field,
                Some(rect),
                "Pixels changed after this task",
            );
        }
    }
    fn mask(
        &mut self,
        before: &Option<Mask>,
        after: &Option<Mask>,
        current: &mut Option<Mask>,
        target: &str,
        origin: [i32; 2],
    ) {
        match (before, after, current.as_mut()) {
            (Some(b), Some(a), Some(c)) => {
                self.field(
                    &b.enabled,
                    &a.enabled,
                    &mut c.enabled,
                    Some(target),
                    "mask.enabled",
                );
                let bid: Vec<_> = b.steps.iter().map(|s| s.id.clone()).collect();
                let aid: Vec<_> = a.steps.iter().map(|s| s.id.clone()).collect();
                for step in &a.steps {
                    if let Some(old) = b.steps.iter().find(|s| s.id == step.id) {
                        if let Some(now) = c.steps.iter_mut().find(|s| s.id == step.id) {
                            let field = format!("mask.{}", step.id);
                            let mut meta = step_meta(now);
                            self.value(
                                &step_meta(old),
                                &step_meta(step),
                                &mut meta,
                                Some(target),
                                &field,
                            );
                            now.kind = meta["kind"].as_str().unwrap().into();
                            now.enabled = meta["enabled"].as_bool().unwrap();
                            now.value = meta["value"].as_f64().unwrap() as f32;
                            now.settings = meta["settings"].clone();
                            self.raster(
                                &old.pixels,
                                &step.pixels,
                                &mut now.pixels,
                                target,
                                &format!("{field}.pixels"),
                                origin,
                            );
                        } else if !step_equal(old, step) {
                            self.conflict(
                                Some(target),
                                "mask",
                                None,
                                "Edited mask step was removed",
                            );
                        }
                    } else {
                        self.scope(Some(target), None);
                        if let Some(at) = c.steps.iter().position(|s| s.id == step.id) {
                            if step_equal(&c.steps[at], step) {
                                c.steps.remove(at);
                            } else {
                                self.conflict(
                                    Some(target),
                                    "mask",
                                    None,
                                    "Task-created mask step has later edits",
                                );
                            }
                        }
                    }
                }
                for (i, step) in b.steps.iter().enumerate() {
                    if !aid.contains(&step.id) {
                        self.scope(Some(target), None);
                        if c.steps.iter().any(|s| s.id == step.id) {
                            self.conflict(Some(target), "mask", None, "Removed mask ID was reused");
                        } else {
                            let at = insert_at(
                                &bid,
                                i,
                                &c.steps.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
                            );
                            c.steps.insert(at, step.clone());
                        }
                    }
                }
                let ids: Vec<_> = c.steps.iter().map(|s| s.id.clone()).collect();
                if let Some(order) = self.order(&bid, &aid, &ids, Some(target), "mask.order") {
                    let old = c.steps.clone();
                    c.steps = order
                        .iter()
                        .map(|id| old.iter().find(|s| s.id == *id).unwrap().clone())
                        .collect();
                }
                c.cache_key = id();
            }
            _ if !mask_equal(before, after) => {
                self.scope(Some(target), None);
                if mask_equal(current, after) {
                    *current = before.clone();
                } else {
                    self.conflict(Some(target), "mask", None, "Mask changed after this task");
                }
            }
            _ => {}
        }
    }
    fn layer(&mut self, before: &Layer, after: &Layer, current: &mut Layer) {
        let target = after.id.as_str();
        let origin = [current.x, current.y];
        if before.parent != after.parent {
            for parent in [&before.parent, &after.parent].into_iter().flatten() {
                self.scope(Some(parent), None);
            }
        }
        if before.clip_to != after.clip_to {
            for base in [&before.clip_to, &after.clip_to].into_iter().flatten() {
                self.scope(Some(base), None);
            }
        }
        let mut meta = layer_meta(current);
        self.value(
            &layer_meta(before),
            &layer_meta(after),
            &mut meta,
            Some(target),
            "layer",
        );
        macro_rules! assign {($($f:ident),*)=>{$(current.$f=serde_json::from_value(meta[stringify!($f)].clone()).unwrap();)*};}
        assign!(
            name,
            kind,
            parent,
            clip_to,
            visible,
            locked,
            opacity,
            blend,
            x,
            y,
            color,
            effects,
            source,
            psd_metadata
        );
        self.raster(
            &before.pixels,
            &after.pixels,
            &mut current.pixels,
            target,
            "pixels",
            origin,
        );
        self.mask(&before.mask, &after.mask, &mut current.mask, target, origin);
        current.effect_key = id();
    }
    fn document(&mut self, before: &Document, after: &Document, current: &mut Document) {
        // Frame/project changes have document-wide dependencies. Reverting a resize
        // or replacement must not crop/quantize artwork produced afterward.
        if (
            before.id.as_str(),
            before.width,
            before.height,
            before.bit_depth,
        ) != (
            after.id.as_str(),
            after.width,
            after.height,
            after.bit_depth,
        ) && !document_equal(after, current)
        {
            self.scope(None, None);
            self.conflict(
                None,
                "document.frame",
                None,
                "The project or channel frame changed and has later work",
            );
            return;
        }
        macro_rules! field {($($f:ident),*)=>{$(self.field(&before.$f,&after.$f,&mut current.$f,None,stringify!($f));)*};}
        field!(id, name, width, height, bit_depth, read_only, warnings);
        // Selection state is one coupled source: never mix coverage with an unrelated rectangle.
        let b = (
            &before.selection,
            &before.selection_polygon,
            &before.selection_coverage,
            &before.selection_previous,
        );
        let a = (
            &after.selection,
            &after.selection_polygon,
            &after.selection_coverage,
            &after.selection_previous,
        );
        let c = (
            &current.selection,
            &current.selection_polygon,
            &current.selection_coverage,
            &current.selection_previous,
        );
        if b != a {
            self.scope(Some("@selection"), None);
            if c == a {
                current.selection = before.selection;
                current.selection_polygon = before.selection_polygon.clone();
                current.selection_coverage = before.selection_coverage.clone();
                current.selection_previous = before.selection_previous.clone();
            } else {
                self.conflict(
                    Some("@selection"),
                    "selection",
                    None,
                    "Selection changed after this task",
                );
            }
        }
        let bid: Vec<_> = before.layers.iter().map(|l| l.id.clone()).collect();
        let aid: Vec<_> = after.layers.iter().map(|l| l.id.clone()).collect();
        for layer in &after.layers {
            if let Some(old) = before.layers.iter().find(|l| l.id == layer.id) {
                if let Some(now) = current.layers.iter_mut().find(|l| l.id == layer.id) {
                    self.layer(old, layer, now);
                } else if !layer_equal(old, layer) {
                    self.conflict(
                        Some(&layer.id),
                        "layer",
                        None,
                        "Edited layer was removed after this task",
                    );
                }
            } else {
                self.scope(Some(&layer.id), None);
                if let Some(at) = current.layers.iter().position(|l| l.id == layer.id) {
                    let dependents: Vec<_> = current
                        .layers
                        .iter()
                        .filter(|l| {
                            (l.parent.as_ref() == Some(&layer.id)
                                || l.clip_to.as_ref() == Some(&layer.id))
                                && after.layers.iter().find(|a| a.id == l.id).is_none_or(|a| {
                                    (l.parent.as_ref() == Some(&layer.id) && a.parent != l.parent)
                                        || (l.clip_to.as_ref() == Some(&layer.id)
                                            && a.clip_to != l.clip_to)
                                })
                        })
                        .map(|l| l.id.clone())
                        .collect();
                    if !dependents.is_empty() {
                        self.conflict(
                            Some(&layer.id),
                            "layer.dependencies",
                            None,
                            &format!(
                                "Later layers depend on this source: {}",
                                dependents.join(", ")
                            ),
                        );
                    } else if layer_equal(&current.layers[at], layer) {
                        current.layers.remove(at);
                    } else {
                        self.conflict(
                            Some(&layer.id),
                            "layer",
                            None,
                            "Task-created layer has later edits",
                        );
                    }
                }
            }
        }
        for (i, layer) in before.layers.iter().enumerate() {
            if !aid.contains(&layer.id) {
                self.scope(Some(&layer.id), None);
                for source in [&layer.parent, &layer.clip_to].into_iter().flatten() {
                    self.scope(Some(source), None);
                }
                if current.layers.iter().any(|l| l.id == layer.id) {
                    self.conflict(
                        Some(&layer.id),
                        "layer",
                        None,
                        "Removed layer ID was reused",
                    );
                } else {
                    let at = insert_at(
                        &bid,
                        i,
                        &current
                            .layers
                            .iter()
                            .map(|l| l.id.clone())
                            .collect::<Vec<_>>(),
                    );
                    current.layers.insert(at, layer.clone());
                }
            }
        }
        let ids: Vec<_> = current.layers.iter().map(|l| l.id.clone()).collect();
        if let Some(order) = self.order(&bid, &aid, &ids, None, "layer.order") {
            let old = current.layers.clone();
            current.layers = order
                .iter()
                .map(|id| old.iter().find(|l| l.id == *id).unwrap().clone())
                .collect();
        }
    }
}
fn id_array(a: &[Value]) -> bool {
    a.iter().all(|v| v["id"].is_string())
}
fn value_id(v: &Value) -> String {
    v["id"].as_str().unwrap().into()
}
fn insert_at(before: &[String], i: usize, current: &[String]) -> usize {
    before[i + 1..]
        .iter()
        .find_map(|id| current.iter().position(|s| s == id))
        .unwrap_or_else(|| {
            before[..i]
                .iter()
                .rev()
                .find_map(|id| current.iter().position(|s| s == id).map(|i| i + 1))
                .unwrap_or(current.len())
        })
}
fn extend(rect: &mut Option<[i32; 4]>, p: [i32; 2]) {
    *rect = Some(rect.map_or([p[0], p[1], p[0] + 1, p[1] + 1], |r| {
        [
            r[0].min(p[0]),
            r[1].min(p[1]),
            r[2].max(p[0] + 1),
            r[3].max(p[1] + 1),
        ]
    }));
}
fn step_meta(s: &MaskStep) -> Value {
    json!({"id":s.id,"kind":s.kind,"enabled":s.enabled,"value":s.value,"settings":s.settings})
}
fn step_equal(a: &MaskStep, b: &MaskStep) -> bool {
    step_meta(a) == step_meta(b) && a.pixels == b.pixels
}
fn mask_equal(a: &Option<Mask>, b: &Option<Mask>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            a.enabled == b.enabled
                && a.steps.len() == b.steps.len()
                && a.steps.iter().zip(&b.steps).all(|(a, b)| step_equal(a, b))
        }
        _ => false,
    }
}
fn layer_meta(l: &Layer) -> Value {
    json!({"name":l.name,"kind":l.kind,"parent":l.parent,"clip_to":l.clip_to,"visible":l.visible,"locked":l.locked,"opacity":l.opacity,"blend":l.blend,"x":l.x,"y":l.y,"color":l.color,"effects":l.effects,"source":l.source,"psd_metadata":l.psd_metadata})
}
fn layer_equal(a: &Layer, b: &Layer) -> bool {
    layer_meta(a) == layer_meta(b) && a.pixels == b.pixels && mask_equal(&a.mask, &b.mask)
}
fn document_equal(a: &Document, b: &Document) -> bool {
    (
        a.id.as_str(),
        a.name.as_str(),
        a.width,
        a.height,
        a.bit_depth,
        a.read_only,
        &a.warnings,
        &a.selection,
        &a.selection_polygon,
        &a.selection_coverage,
        &a.selection_previous,
    ) == (
        b.id.as_str(),
        b.name.as_str(),
        b.width,
        b.height,
        b.bit_depth,
        b.read_only,
        &b.warnings,
        &b.selection,
        &b.selection_polygon,
        &b.selection_coverage,
        &b.selection_previous,
    ) && a.layers.len() == b.layers.len()
        && a.layers
            .iter()
            .zip(&b.layers)
            .all(|(a, b)| a.id == b.id && layer_equal(a, b))
}

impl Engine {
    pub(crate) fn trim_history(&mut self) {
        if self.undo.len() > 40 {
            let old = self.undo.remove(0);
            if let Some(task) = old.task {
                if !self
                    .undo
                    .iter()
                    .any(|h| h.reverted.contains(&old.after.revision))
                {
                    self.truncated_tasks.insert((old.actor, task));
                }
            }
        }
    }
    pub fn task_history(&self) -> Value {
        let reverted: BTreeSet<_> = self
            .undo
            .iter()
            .flat_map(|h| h.reverted.iter().copied())
            .collect();
        let mut tasks = Vec::<Value>::new();
        for h in &self.undo {
            if let Some(task) = &h.task {
                if h.actor == "human" {
                    continue;
                }
                if !tasks
                    .iter()
                    .any(|v| v["actor"] == h.actor && v["task"] == *task)
                {
                    let active = self
                        .undo
                        .iter()
                        .filter(|e| {
                            e.actor == h.actor
                                && e.task.as_ref() == Some(task)
                                && !reverted.contains(&e.after.revision)
                        })
                        .count();
                    tasks.push(json!({"actor":h.actor,"task":task,"label":h.label,"active_batches":active,"complete":!self.truncated_tasks.contains(&(h.actor.clone(),task.clone()))}));
                }
            }
        }
        json!(tasks)
    }
    fn task_inverse(
        &self,
        actor: &str,
        owner: &str,
        task: &str,
    ) -> Result<(Document, Inverse, Vec<u64>), String> {
        if owner == "human" {
            return Err(
                "Selective undo is for agent tasks; human history remains chronological".into(),
            );
        }
        if actor != "human" && actor != owner {
            return Err("An agent can undo only its own tasks".into());
        }
        if self.truncated_tasks.contains(&(owner.into(), task.into())) {
            return Err(
                "Task exceeds the retained 40-step history; refusing a partial task undo".into(),
            );
        }
        let reverted: BTreeSet<_> = self
            .undo
            .iter()
            .flat_map(|h| h.reverted.iter().copied())
            .collect();
        let entries: Vec<_> = self
            .undo
            .iter()
            .filter(|h| {
                h.actor == owner
                    && h.task.as_deref() == Some(task)
                    && !reverted.contains(&h.after.revision)
            })
            .collect();
        if entries.is_empty() {
            return Err("No active batches for this task in the retained 40-step history".into());
        }
        let mut doc = self.doc.clone();
        let mut inverse = Inverse::default();
        for h in entries.iter().rev() {
            inverse.document(&h.before, &h.after, &mut doc);
        }
        if let Err(error) = crate::psd::validate(&doc) {
            inverse.conflict(None, "document", None, &error);
        }
        Ok((
            doc,
            inverse,
            entries.iter().map(|h| h.after.revision).collect(),
        ))
    }
    pub fn inspect_task_undo(
        &mut self,
        actor: &str,
        owner: &str,
        task: &str,
    ) -> Result<Value, String> {
        let (_, mut inverse, revisions) = self.task_inverse(actor, owner, task)?;
        if self.doc.read_only {
            inverse.conflict(None, "document", None, "Document is read-only");
        }
        if let Err(error) = self.check(actor, &inverse.scopes) {
            inverse.conflict(None, "reservation", None, &error);
        }
        Ok(
            json!({"revision":self.doc.revision,"actor":owner,"task":task,"can_undo":inverse.conflicts.is_empty()&&!self.doc.read_only,"batches":revisions,"scopes":inverse.scopes,"conflicts":inverse.conflicts}),
        )
    }
    pub fn undo_task(&mut self, actor: &str, owner: &str, task: &str) -> Result<Value, String> {
        if self.doc.read_only {
            return Err("Document is read-only".into());
        }
        let (mut doc, inverse, reverted) = self.task_inverse(actor, owner, task)?;
        if !inverse.conflicts.is_empty() {
            return Err(format!(
                "Task undo conflicts; no changes applied: {}",
                json!(inverse.conflicts)
            ));
        }
        self.check(actor, &inverse.scopes)?;
        let before = self.doc.clone();
        doc.revision = before.revision + 1;
        // Invalidate ancestors as well as directly changed sources; caches are derived only.
        for layer in &mut doc.layers {
            layer.effect_key = id();
            if let Some(mask) = &mut layer.mask {
                mask.cache_key = id();
            }
        }
        let label = format!("Undo task: {task}");
        let revision = doc.revision;
        self.doc = doc;
        self.undo.push(History {
            before,
            after: self.doc.clone(),
            actor: actor.into(),
            label: label.clone(),
            task: None,
            scopes: inverse.scopes.clone(),
            gesture: None,
            reverted: reverted.clone(),
        });
        self.trim_history();
        self.redo.clear();
        self.changes.push((revision, inverse.scopes.clone()));
        if self.changes.len() > 200 {
            self.changes.remove(0);
        }
        self.mark_ai(actor, &label, "history", inverse.scopes.clone());
        self.status = label;
        Ok(
            json!({"revision":revision,"task":task,"actor":owner,"reverted_batches":reverted,"scopes":inverse.scopes}),
        )
    }
}
