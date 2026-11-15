//! Whole-layer copy-on-write snapshots. The shared Engine owns undo and reservations.
use crate::{
    engine::{id, Document, Layer},
    raster::{check_size, Raster},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
mod forest;
pub(crate) use forest::Forest;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layers {
    pub width: u32,
    pub height: u32,
    pub roots: Vec<String>,
    pub layers: Vec<Layer>,
}

pub fn copy(doc: &Document, ids: &[String]) -> Result<Layers, String> {
    check_copy_source(doc.into())?;
    let forest = Forest::new(doc.into(), ids)?;
    let roots = forest.roots().map(|i| doc.layers[i].id.clone()).collect();
    let snapshot = Layers {
        width: doc.width,
        height: doc.height,
        roots,
        layers: doc
            .layers
            .iter()
            .enumerate()
            .filter(|(i, _)| forest.contains(*i))
            .map(|(_, layer)| layer.clone())
            .collect(),
    };
    validate(&snapshot)?;
    Ok(snapshot)
}

pub(crate) fn check_copy_source(doc: crate::engine::view::DocumentView<'_>) -> Result<(), String> {
    if doc.read_only() {
        return Err("Protected Photoshop sources need an explicit compatible copy before copying editable layers".into());
    }
    Ok(())
}

pub fn validate(snapshot: &Layers) -> Result<(), String> {
    validated_document(snapshot).map(drop)
}

pub(crate) fn validated_document(snapshot: &Layers) -> Result<Document, String> {
    check_size(snapshot.width, snapshot.height)?;
    if snapshot.layers.is_empty() || snapshot.layers.len() > 100 || snapshot.roots.is_empty() {
        return Err("Clipboard needs a valid layer tree within the 100-layer limit".into());
    }
    let mut ids = HashSet::new();
    for l in &snapshot.layers {
        if l.id.is_empty() || !ids.insert(l.id.as_str()) {
            return Err("Clipboard contains empty or duplicate layer IDs".into());
        }
        validate_raster(&l.pixels)?;
        if !(0.0..=1.0).contains(&l.opacity) {
            return Err("Clipboard layer opacity must be between 0 and 1".into());
        }
        if !crate::raster::layer_blends(&l.kind).any(|(mode, _)| mode == l.blend) {
            return Err("Clipboard contains an unsupported layer blend".into());
        }
        for effect in &l.effects {
            crate::effects::validate_weight(effect.weight)?;
        }
        if let Some(mask) = &l.mask {
            for step in &mask.steps {
                crate::effects::validate_weight(step.weight)?;
                validate_raster(&step.pixels)?;
            }
        }
    }
    let roots = snapshot
        .roots
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    if roots.len() != snapshot.roots.len() || roots.iter().any(|root| !ids.contains(root)) {
        return Err("Clipboard root layer IDs are invalid".into());
    }
    for l in &snapshot.layers {
        if roots.contains(l.id.as_str()) {
            if l.parent.as_deref().is_some_and(|p| ids.contains(p)) {
                return Err("A clipboard root cannot also be another copied layer's child".into());
            }
        } else if !l.parent.as_deref().is_some_and(|p| ids.contains(p)) {
            return Err("Clipboard layer tree contains a missing parent".into());
        }
    }
    let mut draft = Document::new(snapshot.width, snapshot.height)?;
    draft.bit_depth = snapshot.layers[0].pixels.depth;
    draft.layers = snapshot.layers.clone();
    for layer in &mut draft.layers {
        if roots.contains(layer.id.as_str()) {
            layer.parent = None;
        }
    }
    crate::psd::validate(&draft)?;
    Ok(draft)
}
fn validate_raster(r: &Raster) -> Result<(), String> {
    check_size(r.width, r.height)?;
    r.validate_layout()
}

fn unlocked(doc: &Document, target: &str) -> Result<(), String> {
    let mut next = Some(target);
    for depth in 0..=16 {
        let Some(at) = next else { return Ok(()) };
        let l = doc
            .layers
            .iter()
            .find(|l| l.id == at)
            .ok_or("Destination layer or folder no longer exists")?;
        if l.locked {
            return Err("Unlock the selected layer and its containing folders first".into());
        }
        next = l.parent.as_deref();
        if depth == 16 && next.is_some() {
            return Err("Group nesting limit exceeded".into());
        }
    }
    Ok(())
}

/// Roots and all copied descendants are deleted by one Engine batch only after OS copy succeeds.
pub fn cut_commands(doc: &Document, ids: &[String]) -> Result<Vec<Value>, String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    let snapshot = copy(doc, ids)?;
    for layer in &snapshot.layers {
        unlocked(doc, &layer.id)?;
    }
    Ok(snapshot
        .roots
        .iter()
        .map(|root| json!({"op":"layer.delete","layer":root}))
        .collect())
}
fn cloned(
    snapshot: &Layers,
    parent: Option<&str>,
    keep_parent: bool,
    root_ids: Option<&serde_json::Map<String, Value>>,
) -> (Vec<Layer>, Vec<String>) {
    let mapping = snapshot
        .layers
        .iter()
        .map(|l| {
            let copied = root_ids.and_then(|ids| ids.get(&l.id)).and_then(Value::as_str);
            (l.id.as_str(), copied.map_or_else(id, str::to_owned))
        })
        .collect::<HashMap<_, _>>();
    let root_set = snapshot
        .roots
        .iter()
        .map(String::as_str)
        .collect::<HashSet<_>>();
    let mut clones = snapshot
        .layers
        .iter()
        .map(|old| {
            let mut layer = old.clone();
            layer.id = mapping[old.id.as_str()].clone();
            layer.psd_metadata = crate::psd::metadata_for_duplicate(&old.psd_metadata);
            layer.parent = if root_set.contains(old.id.as_str()) {
                if keep_parent {
                    old.parent.clone()
                } else {
                    parent.map(String::from)
                }
            } else {
                old.parent.as_deref().map(|p| mapping[p].clone())
            };
            if root_set.contains(old.id.as_str()) {
                layer.name.push_str(" copy");
            }
            layer.clip_to = old
                .clip_to
                .as_deref()
                .map(|base| mapping.get(base).cloned().unwrap_or_else(|| base.into()));
            layer.effect_key = id();
            for effect in &mut layer.effects {
                effect.id = id();
            }
            if let Some(mask) = &mut layer.mask {
                mask.cache_key = id();
                for step in &mut mask.steps {
                    step.id = id();
                }
            }
            (old.id.clone(), layer)
        })
        .collect::<HashMap<_, _>>();
    fn emit(
        old: &str,
        snapshot: &Layers,
        clones: &mut HashMap<String, Layer>,
        output: &mut Vec<Layer>,
    ) {
        output.push(clones.remove(old).unwrap());
        for child in snapshot
            .layers
            .iter()
            .filter(|l| l.parent.as_deref() == Some(old))
        {
            emit(&child.id, snapshot, clones, output);
        }
    }
    let roots = snapshot
        .roots
        .iter()
        .map(|root| mapping[root.as_str()].clone())
        .collect();
    let mut output = vec![];
    for root in &snapshot.roots {
        emit(root, snapshot, &mut clones, &mut output);
    }
    (output, roots)
}
fn commit(doc: &mut Document, mut draft: Document) -> Result<(), String> {
    // New content changes the containing folder's composite. Derived caches have no history meaning.
    for layer in &mut draft.layers {
        if layer.kind == "group" {
            layer.effect_key = id();
        }
    }
    draft.ensure_depth();
    crate::psd::validate(&draft)?;
    *doc = draft;
    Ok(())
}

/// Duplicate the active selected image at its document coordinates, or its whole
/// editable layer tree when no pixel selection is active. No OS clipboard is used.
pub fn duplicate_selection(doc: &mut Document, command: &Value) -> Result<Vec<String>, String> {
    let (target, mask) = duplicate_selection_target((&*doc).into(), command)?;
    duplicate_selected_pixels(doc, target, mask)
}

pub(crate) fn duplicate_selection_target<'a>(
    doc: crate::engine::view::DocumentView<'_>,
    command: &'a Value,
) -> Result<(&'a str, bool), String> {
    if command
        .get("document_id")
        .is_some_and(|value| value.as_str() != Some(doc.id()))
        || command
            .get("source_revision")
            .is_some_and(|value| value.as_u64() != Some(doc.revision()))
    {
        return Err("The source document changed before duplication".into());
    }
    if doc.read_only() {
        return Err("This PSD is read-only".into());
    }
    let target = command["layer"]
        .as_str()
        .ok_or("Choose an active layer to duplicate")?;
    let mask = command.get("mask").map_or(Ok(false), |value| {
        value.as_bool().ok_or("Duplicate mask must be true or false")
    })?;
    if mask {
        if doc.selection().is_none() {
            return Err("Select pixels before duplicating a mask".into());
        }
        if doc.find_layer(target).and_then(|layer| layer.mask()).is_none() {
            return Err("Choose a layer with a mask to duplicate".into());
        }
    }
    Ok((target, mask))
}

pub(crate) fn duplicate_selection_index(
    doc: crate::engine::view::DocumentView<'_>,
    at: usize,
    mask: bool,
) -> usize {
    let source = doc.layer(at).unwrap();
    let base = match source.clip_to() {
        Some(_) if !mask => return at,
        clip_to => clip_to.unwrap_or(source.id()),
    };
    // Insert above the clipping unit whenever the new layer is an independent sibling.
    doc.layers().position(|layer| layer.id() == base || layer.clip_to() == Some(base))
        .unwrap_or(at)
}

fn duplicate_selected_pixels(doc: &mut Document, target: &str, mask: bool) -> Result<Vec<String>, String> {
    if doc.selection.is_none() {
        return duplicate(doc, &[target.into()]);
    }
    unlocked(doc, target)?;
    if doc.layers.len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let at = doc
        .layers
        .iter()
        .position(|layer| layer.id == target)
        .ok_or("The active layer no longer exists")?;
    let source = &doc.layers[at];
    if !mask && source.kind == "adjustment" {
        return Err("Choose a paint layer or folder to duplicate selected pixels".into());
    }
    let image = crate::clipboard::Image::copy_in_document(doc, target, mask)?;
    let pixels = if let Some(words) = &image.samples16 {
        Raster::from_rgba16(image.width, image.height, words)?
    } else {
        Raster::from_rgba(image.width, image.height, &image.bytes)?
    };
    let has_pixels = image.samples16.as_ref().map_or_else(
        || image.bytes.chunks_exact(4).any(|pixel| pixel[3] != 0),
        |words| words.chunks_exact(4).any(|pixel| pixel[3] != 0),
    );
    if !has_pixels {
        return Err("The selection contains no visible pixels on this layer".into());
    }
    let origin = image
        .origin
        .ok_or("Selected pixels need document coordinates")?;
    let mut layer = Layer::new(
        &format!("{} {}", source.name, if mask { "mask selection" } else { "selection" }),
        "paint",
        image.width,
        image.height,
    );
    layer.parent = source.parent.clone();
    layer.x = origin[0];
    layer.y = origin[1];
    layer.pixels = pixels;
    let created = layer.id.clone();
    if !mask {
        layer.clip_to = source.clip_to.clone();
    }
    let insert = duplicate_selection_index((&*doc).into(), at, mask);
    let mut draft = doc.clone();
    draft.layers.insert(insert, layer);
    commit(doc, draft)?;
    Ok(vec![created])
}

/// Duplicate each root immediately above its original, retaining its original parent.
pub fn duplicate(doc: &mut Document, ids: &[String]) -> Result<Vec<String>, String> {
    duplicate_with_ids(doc, ids, None)
}

pub(crate) fn checked_duplicate_ids<'a, 'b>(
    doc: crate::engine::view::DocumentView<'_>,
    roots: impl Iterator<Item = &'b str>,
    requested: Option<&'a Value>,
) -> Result<Option<&'a serde_json::Map<String, Value>>, String> {
    let Some(requested) = requested else { return Ok(None) };
    let requested = requested.as_object()
        .filter(|ids| (1..=100).contains(&ids.len()))
        .ok_or("Duplicate root IDs must map the copied roots to new layer UUIDs")?;
    let mut used = HashSet::with_capacity(requested.len());
    for root in roots {
        let copied = requested.get(root).and_then(Value::as_str)
            .filter(|value| value.len() == 36)
            .filter(|value| uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == *value))
            .ok_or("Each copied root needs a canonical new layer UUID")?;
        if doc.find_layer(copied).is_some() || !used.insert(copied) {
            return Err("A duplicate root UUID is already in use".into());
        }
    }
    if used.len() != requested.len() {
        return Err("Duplicate root IDs must match the copied roots".into());
    }
    Ok(Some(requested))
}

pub(crate) fn duplicate_with_ids(
    doc: &mut Document,
    ids: &[String],
    requested: Option<&Value>,
) -> Result<Vec<String>, String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    let snapshot = copy(doc, ids)?;
    for root in &snapshot.roots {
        unlocked(doc, root)?;
    }
    if doc.layers.len() + snapshot.layers.len() > 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let root_ids = checked_duplicate_ids((&*doc).into(), snapshot.roots.iter().map(String::as_str), requested)?;
    let (clones, created) = cloned(&snapshot, None, true, root_ids);
    let mut draft = doc.clone();
    let at = doc
        .layers
        .iter()
        .position(|l| snapshot.roots.contains(&l.id))
        .unwrap();
    if snapshot.layers.iter().any(|l| l.clip_to.is_some()) {
        draft.layers.splice(at..at, clones);
    } else {
        let mut forest = HashMap::new();
        for (old, new) in snapshot.roots.iter().zip(&created) {
            let mut ids = HashSet::from([new.clone()]);
            loop {
                let n = ids.len();
                for l in &clones {
                    if l.parent.as_ref().is_some_and(|p| ids.contains(p)) {
                        ids.insert(l.id.clone());
                    }
                }
                if ids.len() == n {
                    break;
                }
            }
            forest.insert(
                old.as_str(),
                clones
                    .iter()
                    .filter(|l| ids.contains(&l.id))
                    .cloned()
                    .collect::<Vec<_>>(),
            );
        }
        draft.layers.clear();
        for l in &doc.layers {
            if let Some(block) = forest.remove(l.id.as_str()) {
                draft.layers.extend(block)
            }
            draft.layers.push(l.clone());
        }
    }
    commit(doc, draft)?;
    Ok(created)
}

pub(crate) fn plan_paste(
    doc: &Document,
    count: usize,
    target: &str,
) -> Result<crate::clipboard::paste::Plan, String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    crate::clipboard::paste::Plan::layers(
        target,
        doc.layers.len(),
        count,
        |i| {
            let l = &doc.layers[i];
            crate::clipboard::paste::Layer {
                id: &l.id,
                parent: l.parent.as_deref(),
                kind: &l.kind,
                clip_to: l.clip_to.as_deref(),
                locked: l.locked,
            }
        },
    )
}

/// Paste into a folder/current sibling folder. Top-level paint destinations become a folder.
pub fn paste(doc: &mut Document, snapshot: &Layers, target: &str) -> Result<Vec<String>, String> {
    let plan = plan_paste(doc, snapshot.layers.len(), target)?;
    validate(snapshot)?;
    let mut draft = doc.clone();
    let parent = if let Some(base) = plan.wrap_base {
        let current = &doc.layers[base];
        let mut folder = Layer::new(
            &format!("{} group", current.name),
            "group",
            doc.width,
            doc.height,
        );
        folder.blend = current.blend.clone();
        draft.layers[base].blend = "normal".into();
        for layer in &mut draft.layers {
            if layer.parent == current.parent
                && (layer.id == current.id || layer.clip_to.as_deref() == Some(&current.id))
            {
                layer.parent = Some(folder.id.clone());
            }
        }
        let parent = Some(folder.id.clone());
        draft.layers.insert(plan.index - 1, folder);
        parent
    } else {
        plan.parent
    };
    let (mut clones, created) = cloned(snapshot, parent.as_deref(), false, None);
    for effect in clones.iter_mut().flat_map(|layer| &mut layer.effects) {
        if !crate::effects::styles::global_light_matches(&effect.settings, doc.psd_global_angle) {
            crate::effects::styles::detach_global_light(std::sync::Arc::make_mut(&mut effect.settings));
        }
    }
    draft.layers.splice(plan.index..plan.index, clones);
    draft.selection = None;
    draft.selection_polygon = None;
    draft.selection_coverage = None;
    commit(doc, draft)?;
    Ok(created)
}
