//! Whole-layer copy-on-write snapshots. The shared Engine owns undo and reservations.
use crate::{
    engine::{id, Document, Layer},
    raster::{check_size, Raster},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Layers {
    pub width: u32,
    pub height: u32,
    pub roots: Vec<String>,
    pub layers: Vec<Layer>,
}

pub fn copy(doc: &Document, ids: &[String]) -> Result<Layers, String> {
    if ids.is_empty() || ids.len() > 100 {
        return Err("Choose 1–100 layers to copy".into());
    }
    let selected = ids.iter().cloned().collect::<HashSet<_>>();
    if selected
        .iter()
        .any(|id| !doc.layers.iter().any(|l| l.id == *id))
    {
        return Err("A selected layer no longer exists".into());
    }
    let mut roots = crate::tree::roots(doc, &selected);
    // Storage may place children before folders. Match the visible hierarchy's
    // preorder so moving copied roots into one folder retains their visible order.
    fn hierarchy_order(
        doc: &Document,
        parent: Option<&str>,
        depth: usize,
        order: &mut HashMap<String, usize>,
    ) {
        if depth > 16 {
            return;
        }
        for layer in doc
            .layers
            .iter()
            .filter(|layer| layer.parent.as_deref() == parent)
        {
            order.insert(layer.id.clone(), order.len());
            if layer.kind == "group" {
                hierarchy_order(doc, Some(&layer.id), depth + 1, order);
            }
        }
    }
    let mut order = HashMap::new();
    hierarchy_order(doc, None, 0, &mut order);
    roots.sort_by_key(|id| order.get(id).copied().unwrap_or(usize::MAX));
    let mut included = roots.iter().cloned().collect::<HashSet<_>>();
    loop {
        let old = included.len();
        for l in &doc.layers {
            if l.parent.as_ref().is_some_and(|p| included.contains(p)) {
                included.insert(l.id.clone());
            }
        }
        if old == included.len() {
            break;
        }
    }
    if doc.layers.iter().any(|l| {
        l.clip_to
            .as_ref()
            .is_some_and(|base| included.contains(&l.id) != included.contains(base))
    }) {
        return Err(
            "Copy the base and its clipped layers together, or release clipping first".into(),
        );
    }
    let snapshot = Layers {
        width: doc.width,
        height: doc.height,
        roots,
        layers: doc
            .layers
            .iter()
            .filter(|l| included.contains(&l.id))
            .cloned()
            .collect(),
    };
    validate(&snapshot)?;
    Ok(snapshot)
}

pub fn validate(snapshot: &Layers) -> Result<(), String> {
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
    crate::psd::validate(&draft)
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
fn cloned(snapshot: &Layers, parent: Option<&str>, keep_parent: bool) -> (Vec<Layer>, Vec<String>) {
    let mapping = snapshot
        .layers
        .iter()
        .map(|l| (l.id.as_str(), id()))
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
    if command
        .get("document_id")
        .is_some_and(|value| value.as_str() != Some(doc.id.as_str()))
        || command
            .get("source_revision")
            .is_some_and(|value| value.as_u64() != Some(doc.revision))
    {
        return Err("The source document changed before duplication".into());
    }
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    let target = command["layer"]
        .as_str()
        .ok_or("Choose an active layer to duplicate")?;
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
    if source.kind == "adjustment" {
        return Err("Choose a paint layer or folder to duplicate selected pixels".into());
    }
    let image = crate::clipboard::Image::copy(doc, target, false, false)?;
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
        &format!("{} selection", source.name),
        "paint",
        image.width,
        image.height,
    );
    layer.parent = source.parent.clone();
    layer.x = origin[0];
    layer.y = origin[1];
    layer.pixels = pixels;
    let created = layer.id.clone();
    // A clipped source remains clipped to the same base. The base itself must
    // stay adjacent to its followers, so insert a base selection above the unit.
    layer.clip_to = source.clip_to.clone();
    let insert = if source.clip_to.is_none() {
        doc.layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.clip_to.as_deref() == Some(target))
            .map(|(index, _)| index)
            .min()
            .unwrap_or(at)
    } else {
        at
    };
    let mut draft = doc.clone();
    draft.layers.insert(insert, layer);
    commit(doc, draft)?;
    Ok(vec![created])
}

/// Duplicate each root immediately above its original, retaining its original parent.
pub fn duplicate(doc: &mut Document, ids: &[String]) -> Result<Vec<String>, String> {
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
    let (clones, created) = cloned(&snapshot, None, true);
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

/// Paste into a folder/current sibling folder. Top-level paint destinations become a folder.
pub fn paste(doc: &mut Document, snapshot: &Layers, target: &str) -> Result<Vec<String>, String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    validate(snapshot)?;
    let selected = doc.layers.iter().position(|l| l.id == target);
    if selected.is_none() && !target.is_empty() && !doc.layers.is_empty() {
        return Err("Paste destination no longer exists; choose a layer or folder again".into());
    }
    // An empty document after Cut has no destination. An omitted target uses the first layer.
    let selected = selected.or_else(|| (!doc.layers.is_empty()).then_some(0));
    if let Some(index) = selected {
        unlocked(doc, &doc.layers[index].id)?;
    }
    let wrap = selected.is_some_and(|index| {
        doc.layers[index].kind != "group" && doc.layers[index].parent.is_none()
    });
    if doc.layers.len() + snapshot.layers.len() + usize::from(wrap) > 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    let mut draft = doc.clone();
    let (at, parent) = if let Some(index) = selected {
        let current = draft.layers[index].clone();
        if wrap {
            let base = current.clip_to.as_deref().unwrap_or(&current.id);
            let members: Vec<_> = draft
                .layers
                .iter()
                .enumerate()
                .filter(|(_, l)| {
                    l.parent == current.parent
                        && (l.id == base || l.clip_to.as_deref() == Some(base))
                })
                .map(|(i, _)| i)
                .collect();
            let at = *members.iter().min().unwrap();
            let base_at = draft.layers.iter().position(|l| l.id == base).unwrap();
            let mut folder = Layer::new(
                &format!("{} group", draft.layers[base_at].name),
                "group",
                doc.width,
                doc.height,
            );
            folder.blend = draft.layers[base_at].blend.clone();
            draft.layers[base_at].blend = "normal".into();
            for member in members {
                draft.layers[member].parent = Some(folder.id.clone());
            }
            let parent = Some(folder.id.clone());
            draft.layers.insert(at, folder);
            (at + 1, parent)
        } else if current.kind == "group" {
            (index + 1, Some(current.id))
        } else {
            (index, current.parent)
        }
    } else {
        (0, None)
    };
    let (clones, created) = cloned(snapshot, parent.as_deref(), false);
    draft.layers.splice(at..at, clones);
    draft.selection = None;
    draft.selection_polygon = None;
    commit(doc, draft)?;
    Ok(created)
}
