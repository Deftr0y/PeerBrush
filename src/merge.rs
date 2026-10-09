//! Bake selected sibling layers or a folder into one undoable raster layer.
use crate::{
    engine::{Document, Layer, Scope},
    raster::{check_size, Raster, TILE},
    server::Shared,
};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc};

pub struct Prepared {
    document: String,
    revision: u64,
    removed: HashSet<String>,
    before: String,
    layer: Layer,
}

pub fn requested(command: &Value) -> Result<Vec<String>, String> {
    if let Some(ids) = command.get("layers") {
        let ids = ids
            .as_array()
            .ok_or("Merge layers must be an array of layer IDs")?;
        ids.iter()
            .map(|v| {
                v.as_str()
                    .filter(|s| !s.is_empty())
                    .map(String::from)
                    .ok_or_else(|| "Merge needs valid layer IDs".into())
            })
            .collect()
    } else {
        Ok(vec![command["layer"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("Choose layers to merge")?
            .into()])
    }
}

pub(crate) fn resolve(doc: &Document, ids: &[String]) -> Result<Vec<String>, String> {
    if ids.is_empty() || ids.len() > 100 {
        return Err("Choose 1–100 layers to merge".into());
    }
    for id in ids {
        if !doc.layers.iter().any(|l| l.id == *id) {
            return Err("A selected layer no longer exists".into());
        }
    }
    let mut roots = crate::tree::roots(doc, &ids.iter().cloned().collect());
    if roots.len() == 1 {
        let at = doc.layers.iter().position(|l| l.id == roots[0]).unwrap();
        let selected = &doc.layers[at];
        if selected.kind != "group" {
            let below = doc
                .layers
                .iter()
                .skip(at + 1)
                .find(|l| l.parent == selected.parent)
                .ok_or("There is no layer below this one to merge")?;
            roots.push(below.id.clone());
        }
    }
    let parent = &doc.layers.iter().find(|l| l.id == roots[0]).unwrap().parent;
    if roots
        .iter()
        .any(|id| doc.layers.iter().find(|l| l.id == *id).unwrap().parent != *parent)
    {
        return Err("Choose layers in the same folder to merge".into());
    }
    if roots
        .iter()
        .any(|id| !doc.layers.iter().find(|l| l.id == *id).unwrap().visible)
    {
        return Err("Show the selected layers before merging".into());
    }
    if roots.len() > 1 {
        let siblings: Vec<_> = doc.layers.iter().filter(|l| l.parent == *parent).collect();
        let positions: Vec<_> = siblings
            .iter()
            .enumerate()
            .filter_map(|(i, l)| roots.contains(&l.id).then_some(i))
            .collect();
        if positions.last().unwrap() - positions[0] + 1 != roots.len() {
            return Err("Choose adjacent layers to preserve the canvas when merging".into());
        }
        if siblings
            .iter()
            .filter(|l| roots.contains(&l.id))
            .any(|l| l.blend != "normal")
        {
            return Err(
                "Put these layers in a folder and merge the folder to preserve their blend modes"
                    .into(),
            );
        }
    }
    Ok(roots)
}

pub fn prepare(doc: &Document, ids: &[String], name: Option<&str>) -> Result<Prepared, String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    let roots = resolve(doc, ids)?;
    if roots.iter().any(|id| {
        doc.layers
            .iter()
            .any(|l| l.id == *id && l.blend == "pass_through")
    }) {
        return Err("Choose an isolated folder blend before merging a pass-through folder".into());
    }
    let top = doc.layers.iter().find(|l| l.id == roots[0]).unwrap();
    let mut removed: HashSet<String> = roots.iter().cloned().collect();
    loop {
        let count = removed.len();
        for l in &doc.layers {
            if l.parent.as_ref().is_some_and(|p| removed.contains(p)) {
                removed.insert(l.id.clone());
            }
        }
        if removed.len() == count {
            break;
        }
    }
    for layer in doc.layers.iter().filter(|l| removed.contains(&l.id)) {
        if layer.locked {
            return Err("Unlock the selected layers and their contents before merging".into());
        }
        let mut parent = layer.parent.as_deref();
        for _ in 0..=16 {
            let Some(id) = parent else { break };
            let l = doc
                .layers
                .iter()
                .find(|l| l.id == id)
                .ok_or("Invalid layer folder")?;
            if l.locked {
                return Err("Unlock the containing folder before merging".into());
            }
            parent = l.parent.as_deref();
        }
    }
    if doc.layers.iter().any(|l| {
        l.clip_to
            .as_ref()
            .is_some_and(|base| removed.contains(&l.id) != removed.contains(base))
    }) {
        return Err("Merge the complete clipping group, or its containing folder".into());
    }
    if roots.iter().any(|id| {
        doc.layers
            .iter()
            .any(|l| l.id == *id && l.kind == "adjustment" && l.clip_to.is_none())
    }) {
        return Err(
            "Put a shared adjustment and its artwork in a folder, then merge the folder".into(),
        );
    }
    let mut isolated = doc.clone();
    isolated.layers.retain(|l| removed.contains(&l.id));
    for l in &mut isolated.layers {
        if roots.contains(&l.id) {
            l.parent = None;
        }
    }
    let mut bounds: Option<[i64; 4]> = None;
    for l in &isolated.layers {
        let area = if l.kind == "group" {
            l.effects.iter().any(|e| e.enabled).then_some([
                0,
                0,
                doc.width as i32,
                doc.height as i32,
            ])
        } else if l.kind == "fill"
            || l.effects
                .iter()
                .any(|e| e.enabled && ["blur", "bloom", "liquify"].contains(&e.kind.as_str()))
            || l.kind == "adjustment"
        {
            Some([0, 0, l.pixels.width as i32, l.pixels.height as i32])
        } else {
            l.pixels.content_bounds()
        };
        if let Some(a) = area {
            let a = [
                a[0] as i64 + l.x as i64,
                a[1] as i64 + l.y as i64,
                a[2] as i64 + l.x as i64,
                a[3] as i64 + l.y as i64,
            ];
            bounds = Some(if let Some(b) = bounds {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            } else {
                a
            });
        }
    }
    let bounds = bounds.unwrap_or([0, 0, 1, 1]);
    if bounds.iter().any(|v| i32::try_from(*v).is_err()) {
        return Err("Merged layer coordinates exceed the supported range".into());
    }
    let w = u32::try_from(bounds[2] - bounds[0]).map_err(|_| "Merged layer is too wide")?;
    let h = u32::try_from(bounds[3] - bounds[1]).map_err(|_| "Merged layer is too tall")?;
    check_size(w, h)?;
    crate::mask::validate_budget(&isolated.layers)?;
    let mut pixels = if doc.bit_depth == 16 {
        let image = crate::depth16::render_crop(
            &isolated,
            [
                bounds[0] as i32,
                bounds[1] as i32,
                bounds[2] as i32,
                bounds[3] as i32,
            ],
        )?;
        Raster::from_rgba16(w, h, &image.words)?
    } else {
        Raster::new(w, h)
    };
    if doc.bit_depth == 8 {
        let masks: Vec<_> = isolated
            .layers
            .iter()
            .map(|l| {
                l.mask
                    .as_ref()
                    .and_then(|m| m.prepare(l.pixels.width, l.pixels.height))
            })
            .collect();
        let colors = crate::effects::prepare(&isolated, &masks)?;
        let plan = crate::compositor::Plan::new(&isolated, &masks, &colors);
        for ty in 0..h.div_ceil(TILE) {
            for tx in 0..w.div_ceil(TILE) {
                let mut tile = vec![0; (TILE * TILE * 4) as usize];
                let mut nonempty = false;
                for y in 0..TILE.min(h - ty * TILE) {
                    for x in 0..TILE.min(w - tx * TILE) {
                        let p = plan.sample(
                            0,
                            bounds[0] as i32 + (tx * TILE + x) as i32,
                            bounds[1] as i32 + (ty * TILE + y) as i32,
                        );
                        let at = ((y * TILE + x) * 4) as usize;
                        tile[at..at + 4].copy_from_slice(&p);
                        nonempty |= p[3] != 0;
                    }
                }
                if nonempty {
                    pixels.tiles.insert((tx, ty), Arc::new(tile));
                }
            }
        }
    }
    let folder_only = roots.len() == 1 && top.kind == "group";
    let default_name = if folder_only {
        top.name.clone()
    } else {
        format!("{} merged", top.name)
    };
    let mut layer = Layer::new(name.unwrap_or(&default_name), "paint", w, h);
    layer.parent = top.parent.clone();
    layer.x = bounds[0] as i32;
    layer.y = bounds[1] as i32;
    layer.pixels = pixels;
    // A single folder remains one compositing unit; preserve its external blend.
    if folder_only {
        layer.blend = top.blend.clone();
    }
    Ok(Prepared {
        document: doc.id.clone(),
        revision: doc.revision,
        removed,
        before: roots[0].clone(),
        layer,
    })
}

impl Prepared {
    pub(crate) fn apply(self, doc: &mut Document) -> Result<(), String> {
        if doc.id != self.document || doc.revision != self.revision {
            return Err("Canvas changed while merging. Inspect the layers and try again.".into());
        }
        let at = doc
            .layers
            .iter()
            .position(|l| l.id == self.before)
            .ok_or("Merge destination no longer exists")?;
        let insertion = doc.layers[..at]
            .iter()
            .filter(|l| !self.removed.contains(&l.id))
            .count();
        doc.layers.retain(|l| !self.removed.contains(&l.id));
        doc.layers.insert(insertion, self.layer);
        Ok(())
    }
}

/// Bake outside the shared lock, then atomically commit only to the unchanged document.
pub fn edit(
    shared: &Shared,
    actor: &str,
    ids: &[String],
    expected: Option<u64>,
    task: Option<&str>,
    name: Option<&str>,
) -> Result<Value, String> {
    let doc = {
        let mut e = shared.lock().unwrap();
        e.check(
            actor,
            &[Scope {
                target: None,
                rect: None,
            }],
        )?;
        if expected.is_some_and(|r| r != e.doc.revision) {
            return Err("Conflicting document change. Observe the layers before merging.".into());
        }
        e.doc.clone()
    };
    let prepared = prepare(&doc, ids, name)?;
    let command = json!({"op":"layer.merge","layers":ids,"name":name});
    shared
        .lock()
        .unwrap()
        .edit_prepared_merge(actor, command, Some(doc.revision), task, prepared)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    #[test]
    fn prepared_merge_never_overwrites_an_intervening_human_edit() {
        let mut e = Engine::new();
        e.doc = Document::new(4, 4).unwrap();
        let first = e.doc.layers[0].id.clone();
        e.doc.layers.push(Layer::new("Second", "paint", 4, 4));
        let prepared = prepare(&e.doc, &[first.clone()], None).unwrap();
        e.edit(
            "human",
            &[json!({"op":"layer.update","layer":first,"name":"Human work"})],
            None,
            None,
            "Rename",
        )
        .unwrap();
        let before = serde_json::to_value(&e.doc).unwrap();
        let history = e.undo.len();
        assert!(e
            .edit_prepared_merge(
                "artist",
                json!({"op":"layer.merge","layers":[first]}),
                None,
                None,
                prepared
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert_eq!(e.undo.len(), history);
    }
}
