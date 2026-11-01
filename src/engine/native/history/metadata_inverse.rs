//! Worker-only three-way metadata compensation. Portable source ordinals never
//! establish equality. The caller retains all source owners and refuses the
//! entire candidate if the report contains a conflict.
use super::super::*;
use crate::engine::{scene::*, Scope};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Before,
    After,
    Current,
}
#[derive(Clone, Copy)]
pub(crate) enum Source<'a> {
    Original(Side, &'a RasterRef36),
    Prepared(&'a RasterRef36),
}
/// All operations concern the physical plane ONLY. Retained originals and their
/// transforms are independent source nodes managed by this module. `same` must
/// be fallible and verify native tuples/padding, never compare portable IDs.
pub(crate) trait Resolver {
    fn same(&mut self, left: Source<'_>, right: Source<'_>) -> Result<bool>;
    /// Append one plane to the final ordered candidate roster. Return a plane-
    /// only reference; a prepared input belongs to the resolver's private table.
    fn import(&mut self, source: Source<'_>) -> Result<RasterRef36>;
    fn inverse(
        &mut self,
        before: Source<'_>,
        after: Source<'_>,
        current: Source<'_>,
        target: &str,
        field: &str,
        origin: [i32; 2],
    ) -> Result<RasterOutcome>;
}
pub(crate) struct RasterOutcome {
    pub plane: RasterRef36,
    pub changed: Option<[i32; 4]>,
    pub conflicts: Option<[i32; 4]>,
}
#[derive(Debug, Serialize, serde::Deserialize)]
pub(crate) struct Conflict {
    pub target: Option<String>,
    pub field: String,
    pub rect: Option<[i32; 4]>,
    pub reason: String,
}
#[derive(Default, Serialize, serde::Deserialize)]
pub(crate) struct Report {
    pub conflicts: Vec<Conflict>,
    pub scopes: Vec<Scope>,
}
pub(crate) struct MetadataResult {
    pub candidate: Admitted<Document36>,
    pub report: Admitted<Report>,
}

// Synthetic source nodes are installed only at typed DTO locations. A settings
// object that happens to look like a RasterRef36 remains ordinary metadata.
#[derive(Clone)]
enum Node {
    Value(Value),
    Object(BTreeMap<String, Node>),
    Array(Vec<Node>),
    Raster(usize, Option<Box<Node>>),
}
impl Node {
    fn from_value(value: Value) -> Self {
        match value {
            Value::Object(map) => Self::Object(
                map.into_iter()
                    .map(|(k, v)| (k, Self::from_value(v)))
                    .collect(),
            ),
            Value::Array(values) => Self::Array(values.into_iter().map(Self::from_value).collect()),
            value => Self::Value(value),
        }
    }
    fn object(&self) -> Result<&BTreeMap<String, Node>> {
        match self {
            Self::Object(value) => Ok(value),
            _ => Err("Native inverse expected typed metadata object".into()),
        }
    }
    fn object_mut(&mut self) -> Result<&mut BTreeMap<String, Node>> {
        match self {
            Self::Object(value) => Ok(value),
            _ => Err("Native inverse expected typed metadata object".into()),
        }
    }
    fn array(&self) -> Result<&[Node]> {
        match self {
            Self::Array(value) => Ok(value),
            _ => Err("Native inverse expected typed metadata array".into()),
        }
    }
    fn id(&self) -> Option<&str> {
        match self.object().ok()?.get("id")? {
            Self::Value(Value::String(value)) => Some(value),
            _ => None,
        }
    }
    fn string(&self) -> Option<&str> {
        match self {
            Self::Value(Value::String(value)) => Some(value),
            _ => None,
        }
    }
    fn integer(&self) -> Option<i32> {
        match self {
            Self::Value(value) => value.as_i64().and_then(|n| n.try_into().ok()),
            _ => None,
        }
    }
}
enum Entry {
    Original(Side, RasterRef36),
    Prepared(RasterRef36),
}
impl Entry {
    fn source(&self) -> Source<'_> {
        match self {
            Self::Original(side, reference) => Source::Original(*side, reference),
            Self::Prepared(reference) => Source::Prepared(reference),
        }
    }
    fn reference(&self) -> &RasterRef36 {
        match self {
            Self::Original(_, reference) => reference,
            Self::Prepared(reference) => reference,
        }
    }
}
const MAX_REPORT: usize = 4096;
const MAX_TEXT: usize = 2 * 1024 * 1024;
const REPORT_BYTES: usize = 8 * 1024 * 1024;
struct Inverse<'a, R> {
    resolver: &'a mut R,
    sources: Vec<Entry>,
    report: Report,
    text: usize,
    check: &'a dyn Fn() -> Result<()>,
}

/// `current` is a fresh admitted metadata copy, not live authority. All source
/// I/O (including equality) runs through the explicit retained resolver outside
/// Engine locks. No document revision is rewound by compensation.
pub(crate) fn inverse<'a>(
    before: &'a Document36,
    after: &'a Document36,
    current: Admitted<Document36>,
    rasters: &'a mut impl Resolver,
    pool: &ResidentBudget,
    check: &'a dyn Fn() -> Result<()>,
) -> Result<MetadataResult> {
    check()?;
    if !current.owner().uses_budget(pool) {
        return Err("Native task metadata uses another admission pool".into());
    }
    // Each fresh tree is measured/admitted before parsing. The separate flat
    // workspace owner covers node conversion, merged clones, materialization,
    // typed output, source-token storage and bounded diagnostics simultaneously.
    let b: Admitted<Value> = copy_admitted(before, pool, false, check)?;
    let a: Admitted<Value> = copy_admitted(after, pool, false, check)?;
    let c: Admitted<Value> = copy_admitted(&*current, pool, false, check)?;
    let bytes = [b.owner().bytes(), a.owner().bytes(), c.owner().bytes()]
        .into_iter()
        .try_fold(REPORT_BYTES, |sum, n| {
            n.checked_mul(12)
                .and_then(|n| sum.checked_add(n))
                .ok_or("Native inverse workspace footprint overflow")
        })?;
    let _workspace_owner = MetadataCharge::reserve(pool, bytes, check)?;
    let count = before.visit_rasters(|_| Ok(()))?
        + after.visit_rasters(|_| Ok(()))?
        + current.visit_rasters(|_| Ok(()))?;
    let capacity = count
        .checked_mul(2)
        .ok_or("Native inverse source count overflow")?;
    let mut sources = Vec::new();
    sources
        .try_reserve_exact(capacity)
        .map_err(|_| "Native inverse source index allocation refused")?;
    let mut work = Inverse {
        resolver: rasters,
        sources,
        report: Report::default(),
        text: 0,
        check,
    };
    let b = b.map_unique(|value, _| work.project(value, before, Side::Before))?;
    let a = a.map_unique(|value, _| work.project(value, after, Side::After))?;
    let c = c.map_unique(|value, _| work.project(value, &current, Side::Current))?;
    let mut candidate = (*c).clone();
    work.document(&b, &a, &mut candidate)?;
    let value = work.materialize(candidate)?;
    let mut candidate: Document36 = serde_json::from_value(value).map_err(|e| e.to_string())?;
    candidate.revision = current.revision;
    work.import_document(&mut candidate)?;
    candidate.visit_rasters(|_| Ok(()))?;
    check()?;
    // Transfer to independently measured flat payload owners. The much larger
    // transient workspace must not accumulate on every history compensation.
    let candidate = copy_metadata(&candidate, pool, check)?;
    let report = copy_admitted(&work.report, pool, false, check)?;
    drop(work.sources);
    // All scratch trees/payloads are gone before their admission is released.
    drop((b, a, c, current));
    Ok(MetadataResult { candidate, report })
}

impl<'a, R: Resolver> Inverse<'a, R> {
    fn source_node(&mut self, node: &mut Node, reference: &RasterRef36, side: Side) -> Result<()> {
        (self.check)()?;
        let mut retained = node
            .object_mut()?
            .remove("retained")
            .ok_or("Missing retained metadata")?;
        let retained = if let Some(original) = &reference.retained {
            let pixels = retained
                .object_mut()?
                .get_mut("pixels")
                .ok_or("Missing original source")?;
            self.source_node(pixels, &original.pixels, side)?;
            Some(Box::new(retained))
        } else {
            None
        };
        if self.sources.len() == self.sources.capacity() {
            return Err("Native inverse source index exceeds admitted count".into());
        }
        let token = self.sources.len();
        self.sources.push(Entry::Original(
            side,
            RasterRef36 {
                id: reference.id,
                width: reference.width,
                height: reference.height,
                depth: reference.depth,
                retained: None,
            },
        ));
        *node = Node::Raster(token, retained);
        Ok(())
    }
    fn coverage(&mut self, node: &mut Node, coverage: &Coverage36, side: Side) -> Result<()> {
        self.source_node(
            node.object_mut()?
                .get_mut("mask")
                .ok_or("Missing coverage mask")?,
            &coverage.mask,
            side,
        )
    }
    fn effects(&mut self, node: &mut Node, effects: &[Effect36], side: Side) -> Result<()> {
        let Node::Array(nodes) = node else {
            return Err("Missing typed effects".into());
        };
        for (node, effect) in nodes.iter_mut().zip(effects) {
            let Node::Array(clips) = node
                .object_mut()?
                .get_mut("clips")
                .ok_or("Missing typed clips")?
            else {
                return Err("Invalid clips".into());
            };
            for (clip, source) in clips.iter_mut().zip(&effect.clips) {
                self.coverage(
                    clip.object_mut()?
                        .get_mut("coverage")
                        .ok_or("Missing typed coverage")?,
                    &source.coverage,
                    side,
                )?;
            }
        }
        Ok(())
    }
    fn project(&mut self, value: Value, metadata: &Document36, side: Side) -> Result<Node> {
        let mut node = Node::from_value(value);
        let document = node.object_mut()?;
        let Node::Array(layers) = document.get_mut("layers").ok_or("Missing typed layers")? else {
            return Err("Invalid layers".into());
        };
        for (node, layer) in layers.iter_mut().zip(&metadata.layers) {
            let object = node.object_mut()?;
            self.source_node(
                object.get_mut("pixels").ok_or("Missing layer pixels")?,
                &layer.pixels,
                side,
            )?;
            if let Some(mask) = &layer.mask {
                let Node::Array(steps) = object
                    .get_mut("mask")
                    .ok_or("Missing mask")?
                    .object_mut()?
                    .get_mut("steps")
                    .ok_or("Missing steps")?
                else {
                    return Err("Invalid mask steps".into());
                };
                for (node, step) in steps.iter_mut().zip(&mask.steps) {
                    self.source_node(
                        node.object_mut()?
                            .get_mut("pixels")
                            .ok_or("Missing step pixels")?,
                        &step.pixels,
                        side,
                    )?;
                }
            }
            self.effects(
                object.get_mut("effects").ok_or("Missing effects")?,
                &layer.effects,
                side,
            )?;
        }
        self.effects(
            document.get_mut("filters").ok_or("Missing filters")?,
            &metadata.filters,
            side,
        )?;
        for (name, coverage) in [
            ("selection_coverage", &metadata.selection_coverage),
            ("selection_previous", &metadata.selection_previous),
        ] {
            if let Some(coverage) = coverage {
                self.coverage(
                    document.get_mut(name).ok_or("Missing selection coverage")?,
                    coverage,
                    side,
                )?;
            }
        }
        Ok(node)
    }
    fn equal(&mut self, left: &Node, right: &Node) -> Result<bool> {
        (self.check)()?;
        Ok(match (left, right) {
            (Node::Value(a), Node::Value(b)) => a == b,
            (Node::Raster(a, ar), Node::Raster(b, br)) => {
                self.resolver
                    .same(self.sources[*a].source(), self.sources[*b].source())?
                    && match (ar, br) {
                        (None, None) => true,
                        (Some(a), Some(b)) => self.equal(a, b)?,
                        _ => false,
                    }
            }
            (Node::Object(a), Node::Object(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                for (key, value) in a {
                    let Some(other) = b.get(key) else {
                        return Ok(false);
                    };
                    if !self.equal(value, other)? {
                        return Ok(false);
                    }
                }
                true
            }
            (Node::Array(a), Node::Array(b)) => {
                if a.len() != b.len() {
                    return Ok(false);
                }
                for (a, b) in a.iter().zip(b) {
                    if !self.equal(a, b)? {
                        return Ok(false);
                    }
                }
                true
            }
            _ => false,
        })
    }
    fn text(&mut self, bytes: usize) -> Result<()> {
        self.text = self
            .text
            .checked_add(bytes)
            .filter(|n| *n <= MAX_TEXT)
            .ok_or("Native inverse diagnostics exceed admission")?;
        Ok(())
    }
    fn scope(&mut self, target: Option<&str>, rect: Option<[i32; 4]>) -> Result<()> {
        if self
            .report
            .scopes
            .iter()
            .any(|s| s.target.as_deref() == target && s.rect == rect)
        {
            return Ok(());
        }
        if self.report.scopes.len() == MAX_REPORT {
            return Err("Too many native inverse scopes".into());
        }
        self.text(target.map_or(0, str::len))?;
        self.report
            .scopes
            .try_reserve_exact(1)
            .map_err(|_| "Native inverse scope allocation refused")?;
        self.report.scopes.push(Scope {
            target: target.map(str::to_owned),
            rect,
        });
        Ok(())
    }
    fn conflict(
        &mut self,
        target: Option<&str>,
        field: &str,
        rect: Option<[i32; 4]>,
        reason: &str,
    ) -> Result<()> {
        if self.report.conflicts.len() == MAX_REPORT || field.len() > 4096 {
            return Err("Too many native inverse conflicts or excessive path".into());
        }
        self.text(target.map_or(0, str::len) + field.len() + reason.len())?;
        self.report
            .conflicts
            .try_reserve_exact(1)
            .map_err(|_| "Native inverse conflict allocation refused")?;
        self.report.conflicts.push(Conflict {
            target: target.map(str::to_owned),
            field: field.into(),
            rect,
            reason: reason.into(),
        });
        Ok(())
    }
    fn atomic(
        &mut self,
        b: &Node,
        a: &Node,
        c: &mut Node,
        target: Option<&str>,
        field: &str,
    ) -> Result<()> {
        if self.equal(b, a)? {
            return Ok(());
        }
        let visibility = target
            .filter(|_| field == "layer.visible")
            .map(|id| format!("@visibility:{id}"));
        self.scope(visibility.as_deref().or(target), None)?;
        if self.equal(c, a)? {
            *c = b.clone();
            Ok(())
        } else {
            self.conflict(target, field, None, "Changed after this task")
        }
    }
    fn raster(
        &mut self,
        b: &Node,
        a: &Node,
        c: &mut Node,
        target: Option<&str>,
        field: &str,
        origin: [i32; 2],
    ) -> Result<()> {
        if self.equal(b, a)? {
            return Ok(());
        }
        let (Node::Raster(bi, br), Node::Raster(ai, _), Node::Raster(ci, _)) = (b, a, &*c) else {
            return self.atomic(b, a, c, target, field);
        };
        let frame = |r: &RasterRef36| (r.width, r.height, r.depth);
        if frame(self.sources[*bi].reference()) != frame(self.sources[*ai].reference()) {
            return self.atomic(b, a, c, target, field);
        }
        if frame(self.sources[*ci].reference()) != frame(self.sources[*ai].reference()) {
            return self.conflict(
                target,
                field,
                None,
                "Raster frame or precision changed after this task",
            );
        }
        let retained = if self.equal(c, a)? { br.clone() } else { None };
        let Node::Raster(_, current_retained) = &*c else {
            unreachable!()
        };
        let retained_same = match (&retained, current_retained) {
            (None, None) => true,
            (Some(left), Some(right)) => self.equal(left, right)?,
            _ => false,
        };
        if !retained_same {
            self.scope(target, None)?;
        }
        let outcome = self.resolver.inverse(
            self.sources[*bi].source(),
            self.sources[*ai].source(),
            self.sources[*ci].source(),
            target.unwrap_or("@selection"),
            field,
            origin,
        )?;
        if outcome.plane.retained.is_some()
            || frame(&outcome.plane) != frame(self.sources[*ci].reference())
        {
            return Err("Native pixel inverse returned a mismatched plane".into());
        }
        if let Some(rect) = outcome.changed {
            self.scope(target, Some(rect))?;
        }
        if let Some(rect) = outcome.conflicts {
            self.conflict(target, field, Some(rect), "Pixels changed after this task")?;
        }
        if self.sources.len() == self.sources.capacity() {
            return Err("Native inverse prepared source index exceeds admission".into());
        }
        let token = self.sources.len();
        self.sources.push(Entry::Prepared(outcome.plane));
        *c = Node::Raster(token, retained);
        Ok(())
    }
    fn merge(
        &mut self,
        b: &Node,
        a: &Node,
        c: &mut Node,
        target: Option<&str>,
        field: &str,
        origin: [i32; 2],
    ) -> Result<()> {
        if field.len() > 4096 {
            return Err("Native inverse metadata path exceeds bound".into());
        }
        if self.equal(b, a)? {
            return Ok(());
        }
        if matches!(b, Node::Raster(..)) {
            return self.raster(b, a, c, target, field, origin);
        }
        // A coverage clip carries its source, frame, origin and contours together.
        if field.ends_with(".clips") || field.ends_with(".coverage") {
            return self.atomic(b, a, c, target, field);
        }
        if let (Node::Object(b), Node::Object(a), Node::Object(c)) = (b, a, &mut *c) {
            for key in b.keys().chain(a.keys().filter(|key| !b.contains_key(*key))) {
                (self.check)()?;
                let path = format!("{field}.{key}");
                match (b.get(key), a.get(key), c.get_mut(key)) {
                    (Some(b), Some(a), Some(c)) => self.merge(b, a, c, target, &path, origin)?,
                    (b, a, _) => {
                        let changed = match (b, a) {
                            (Some(b), Some(a)) => !self.equal(b, a)?,
                            _ => true,
                        };
                        if !changed {
                            continue;
                        }
                        self.scope(target, None)?;
                        let same = match (c.get(key), a) {
                            (Some(c), Some(a)) => self.equal(c, a)?,
                            (None, None) => true,
                            _ => false,
                        };
                        if same {
                            if let Some(b) = b {
                                c.insert(key.clone(), b.clone());
                            } else {
                                c.remove(key);
                            }
                        } else {
                            self.conflict(target, &path, None, "Setting changed after this task")?;
                        }
                    }
                }
            }
            return Ok(());
        }
        if let (Node::Array(b), Node::Array(a), Node::Array(c)) = (b, a, &mut *c) {
            if b.iter()
                .chain(a)
                .chain(c.iter())
                .all(|node| node.id().is_some())
            {
                return self.sequence(b, a, c, target, field, origin, false);
            }
        }
        self.atomic(b, a, c, target, field)
    }
    fn sequence(
        &mut self,
        b: &[Node],
        a: &[Node],
        c: &mut Vec<Node>,
        target: Option<&str>,
        field: &str,
        origin: [i32; 2],
        layers: bool,
    ) -> Result<()> {
        for item in a {
            (self.check)()?;
            let id = item.id().ok_or("Missing inverse item ID")?;
            let old = b.iter().find(|node| node.id() == Some(id));
            let at = c.iter().position(|node| node.id() == Some(id));
            match (old, at) {
                (Some(old), Some(at)) if layers => self.layer(old, item, &mut c[at], id)?,
                (Some(old), Some(at)) => self.merge(
                    old,
                    item,
                    &mut c[at],
                    target,
                    &format!("{field}.{id}"),
                    origin,
                )?,
                (Some(old), None) if !self.equal(old, item)? => self.conflict(
                    if layers { Some(id) } else { target },
                    field,
                    None,
                    "Edited source was removed after this task",
                )?,
                (None, Some(at)) => {
                    let target = if layers { Some(id) } else { target };
                    self.scope(target, None)?;
                    let dependent = layers
                        && c.iter().any(|now| {
                            let Ok(now) = now.object() else { return false };
                            ["parent", "clip_to"].iter().any(|key| {
                                now.get(*key).and_then(Node::string) == Some(id)
                                    && !a.iter().any(|then| {
                                        then.id() == now.get("id").and_then(Node::string)
                                            && then
                                                .object()
                                                .ok()
                                                .and_then(|o| o.get(*key))
                                                .and_then(Node::string)
                                                == Some(id)
                                    })
                            })
                        });
                    if dependent {
                        self.conflict(
                            target,
                            "layer.dependencies",
                            None,
                            "Task-created layer has later dependents",
                        )?;
                    } else if self.equal(&c[at], item)? {
                        c.remove(at);
                    } else {
                        self.conflict(target, field, None, "Task-created source has later edits")?;
                    }
                }
                _ => {}
            }
        }
        for (i, item) in b.iter().enumerate() {
            let id = item.id().ok_or("Missing inverse item ID")?;
            if a.iter().any(|node| node.id() == Some(id)) {
                continue;
            }
            self.scope(if layers { Some(id) } else { target }, None)?;
            if layers {
                for key in ["parent", "clip_to"] {
                    if let Some(id) = item.object()?.get(key).and_then(Node::string) {
                        self.scope(Some(id), None)?;
                    }
                }
            }
            if c.iter().any(|node| node.id() == Some(id)) {
                self.conflict(target, field, None, "Removed source ID was reused")?;
            } else {
                let at = b[i + 1..]
                    .iter()
                    .find_map(|next| c.iter().position(|node| node.id() == next.id()))
                    .or_else(|| {
                        b[..i].iter().rev().find_map(|previous| {
                            c.iter()
                                .position(|node| node.id() == previous.id())
                                .map(|i| i + 1)
                        })
                    })
                    .unwrap_or(c.len());
                c.try_reserve_exact(1)
                    .map_err(|_| "Native inverse insertion allocation refused")?;
                c.insert(at, item.clone());
            }
        }
        let common =
            |id: Option<&str>| b.iter().any(|n| n.id() == id) && a.iter().any(|n| n.id() == id);
        let bid: Vec<_> = b
            .iter()
            .filter(|n| common(n.id()))
            .filter_map(Node::id)
            .collect();
        let aid: Vec<_> = a
            .iter()
            .filter(|n| common(n.id()))
            .filter_map(Node::id)
            .collect();
        if bid != aid {
            self.scope(target, None)?;
            let cid: Vec<_> = c
                .iter()
                .filter(|n| common(n.id()))
                .filter_map(Node::id)
                .collect();
            if cid != aid {
                self.conflict(target, field, None, "Order changed after this task")?;
            } else {
                // Swap only common slots; later additions retain their positions.
                let positions: Vec<_> = c
                    .iter()
                    .enumerate()
                    .filter(|(_, n)| common(n.id()))
                    .map(|(i, _)| i)
                    .collect();
                for (slot, id) in positions.iter().zip(bid) {
                    let at = c
                        .iter()
                        .position(|node| node.id() == Some(id))
                        .ok_or("Missing inverse order member")?;
                    c.swap(*slot, at);
                }
            }
        }
        Ok(())
    }
    fn layer(&mut self, b: &Node, a: &Node, c: &mut Node, id: &str) -> Result<()> {
        let (b, a, c) = (b.object()?, a.object()?, c.object_mut()?);
        for key in b
            .keys()
            .filter(|key| !["pixels", "mask"].contains(&key.as_str()))
        {
            if ["parent", "clip_to"].contains(&key.as_str()) && !self.equal(&b[key], &a[key])? {
                for value in [&b[key], &a[key]] {
                    if let Some(id) = value.string() {
                        self.scope(Some(id), None)?;
                    }
                }
            }
            self.merge(
                &b[key],
                &a[key],
                c.get_mut(key).ok_or("Missing layer field")?,
                Some(id),
                &format!("layer.{key}"),
                [0, 0],
            )?;
        }
        let origin = [
            c["x"].integer().ok_or("Invalid layer origin")?,
            c["y"].integer().ok_or("Invalid layer origin")?,
        ];
        self.raster(
            &b["pixels"],
            &a["pixels"],
            c.get_mut("pixels").ok_or("Missing layer source")?,
            Some(id),
            "pixels",
            origin,
        )?;
        let mask = c.get_mut("mask").ok_or("Missing layer mask")?;
        if matches!(
            (&b["mask"], &a["mask"], &*mask),
            (Node::Object(_), Node::Object(_), Node::Object(_))
        ) {
            self.merge(&b["mask"], &a["mask"], mask, Some(id), "mask", origin)
        } else {
            self.atomic(&b["mask"], &a["mask"], mask, Some(id), "mask")
        }
    }
    fn document(&mut self, b: &Node, a: &Node, c: &mut Node) -> Result<()> {
        let (bo, ao) = (b.object()?, a.object()?);
        let frame = ["id", "width", "height", "bit_depth"];
        let mut changed = false;
        for key in frame {
            changed |= !self.equal(&bo[key], &ao[key])?;
        }
        if changed {
            let co = c.object()?;
            for (key, value) in ao {
                if key != "revision" && !self.equal(value, &co[key])? {
                    self.scope(None, None)?;
                    return self.conflict(
                        None,
                        "document.frame",
                        None,
                        "The project or channel frame changed and has later work",
                    );
                }
            }
        }
        let co = c.object_mut()?;
        let selection = [
            "selection",
            "selection_polygon",
            "selection_coverage",
            "selection_previous",
        ];
        let mut selection_changed = false;
        let mut selection_same = true;
        for key in selection {
            selection_changed |= !self.equal(&bo[key], &ao[key])?;
            selection_same &= self.equal(&co[key], &ao[key])?;
        }
        if selection_changed {
            self.scope(Some("@selection"), None)?;
            if selection_same {
                for key in selection {
                    co.insert(key.into(), bo[key].clone());
                }
            } else {
                self.conflict(
                    Some("@selection"),
                    "selection",
                    None,
                    "Selection changed after this task",
                )?;
            }
        }
        // Profile bytes and their interpretation flag form a single encoding.
        let encoding = ["icc_profile", "srgb_tagged"];
        let mut encoding_changed = false;
        let mut encoding_same = true;
        for key in encoding {
            encoding_changed |= !self.equal(&bo[key], &ao[key])?;
            encoding_same &= self.equal(&co[key], &ao[key])?;
        }
        if encoding_changed {
            self.scope(None, None)?;
            if encoding_same {
                for key in encoding {
                    co.insert(key.into(), bo[key].clone());
                }
            } else {
                self.conflict(
                    None,
                    "document.encoding",
                    None,
                    "Color encoding changed after this task",
                )?;
            }
        }
        for key in bo.keys().filter(|key| {
            !selection.contains(&key.as_str())
                && !encoding.contains(&key.as_str())
                && !["revision", "layers", "filters"].contains(&key.as_str())
        }) {
            self.merge(
                &bo[key],
                &ao[key],
                co.get_mut(key).ok_or("Missing document field")?,
                None,
                &format!("document.{key}"),
                [0, 0],
            )?;
        }
        let mut filters = co["filters"].clone();
        self.merge(
            &bo["filters"],
            &ao["filters"],
            &mut filters,
            None,
            "document.filters",
            [0, 0],
        )?;
        let locked = co["layers"].array()?.iter().any(|layer| {
            layer
                .object()
                .ok()
                .and_then(|o| o.get("locked"))
                .is_some_and(|n| matches!(n, Node::Value(Value::Bool(true))))
        });
        if locked && !self.equal(&filters, &co["filters"])? {
            self.conflict(
                None,
                "document.filters",
                None,
                "Unlock layers before undoing whole-image filters",
            )?;
        } else {
            co.insert("filters".into(), filters);
        }
        let Node::Array(layers) = co.get_mut("layers").ok_or("Missing layers")? else {
            return Err("Invalid layers".into());
        };
        self.sequence(
            bo["layers"].array()?,
            ao["layers"].array()?,
            layers,
            None,
            "layers.order",
            [0, 0],
            true,
        )
    }
    fn materialize(&self, node: Node) -> Result<Value> {
        (self.check)()?;
        Ok(match node {
            Node::Value(value) => value,
            Node::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(key, node)| Ok((key, self.materialize(node)?)))
                    .collect::<Result<_>>()?,
            ),
            Node::Array(values) => Value::Array(
                values
                    .into_iter()
                    .map(|node| self.materialize(node))
                    .collect::<Result<_>>()?,
            ),
            Node::Raster(token, retained) => {
                let reference = self.sources[token].reference();
                serde_json::json!({"id":token+1,"width":reference.width,"height":reference.height,"depth":reference.depth,"retained":retained.map(|node|self.materialize(*node)).transpose()?})
            }
        })
    }
    fn import_plane(&mut self, reference: &mut RasterRef36) -> Result<()> {
        (self.check)()?;
        let token = reference
            .id
            .checked_sub(1)
            .ok_or("Missing inverse source token")? as usize;
        let entry = self
            .sources
            .get(token)
            .ok_or("Unknown inverse source token")?;
        let mut imported = self.resolver.import(entry.source())?;
        if imported.retained.is_some()
            || (imported.width, imported.height, imported.depth)
                != (reference.width, reference.height, reference.depth)
        {
            return Err("Native inverse resolver returned mismatched imported plane".into());
        }
        imported.retained = reference.retained.take();
        if let Some(original) = &mut imported.retained {
            self.import_plane(&mut original.pixels)?;
        }
        *reference = imported;
        Ok(())
    }
    fn import_document(&mut self, document: &mut Document36) -> Result<()> {
        for layer in &mut document.layers {
            self.import_plane(&mut layer.pixels)?;
            if let Some(mask) = &mut layer.mask {
                for step in &mut mask.steps {
                    self.import_plane(&mut step.pixels)?;
                }
            }
            for effect in &mut layer.effects {
                for clip in &mut effect.clips {
                    self.import_plane(&mut clip.coverage.mask)?;
                }
            }
        }
        for effect in &mut document.filters {
            for clip in &mut effect.clips {
                self.import_plane(&mut clip.coverage.mask)?;
            }
        }
        for coverage in [
            &mut document.selection_coverage,
            &mut document.selection_previous,
        ]
        .into_iter()
        .flatten()
        {
            self.import_plane(&mut coverage.mask)?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "metadata_inverse_tests.rs"]
mod tests;
