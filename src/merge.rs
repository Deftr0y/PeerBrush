//! Bake selected sibling layers or a folder into one undoable raster layer.
use crate::{
    engine::{Document, Layer, Scope},
    raster::{Raster, TILE},
    server::Shared,
};
use serde_json::{json, Value};
use std::{collections::HashSet, sync::Arc};

pub(crate) mod input;

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
        if ids.is_empty() || ids.len() > 100 {
            return Err("Choose 1–100 layers to merge".into());
        }
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
    input::resolve(doc.layers.len(), |i| (&doc.layers[i]).into(), ids).map(|roots| {
        roots
            .into_iter()
            .map(|i| doc.layers[i].id.clone())
            .collect()
    })
}

pub fn prepare(doc: &Document, ids: &[String], name: Option<&str>) -> Result<Prepared, String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    let plan = input::Plan::new(doc.layers.len(), |i| (&doc.layers[i]).into(), ids)?;
    let roots: Vec<_> = plan
        .roots
        .iter()
        .map(|&i| doc.layers[i].id.clone())
        .collect();
    let removed: HashSet<_> = plan
        .removed
        .iter()
        .map(|&i| doc.layers[i].id.clone())
        .collect();
    let top = &doc.layers[plan.roots[0]];
    let mut isolated = doc.clone();
    // Merging source layers must not bake the document's final filter stack twice.
    isolated.filters.clear();
    isolated.layers.retain(|l| removed.contains(&l.id));
    for l in &mut isolated.layers {
        if roots.contains(&l.id) {
            l.parent = None;
        }
    }
    let mut bounds: Option<[i64; 4]> = None;
    for l in &isolated.layers {
        if let Some(area) = input::bounds(
            [doc.width, doc.height],
            [l.x, l.y],
            [l.pixels.width, l.pixels.height],
            &l.kind,
            l.effects
                .iter()
                .map(|e| (e.kind.as_str(), e.enabled, e.weight)),
            || Ok(l.pixels.content_bounds()),
        )? {
            bounds = input::union(bounds, area);
        }
    }
    let (bounds, [w, h]) = input::frame(bounds, crate::raster::CanvasLimits::Resident)?;
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
    let folder_only = plan.folder_only;
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
    edit_guarded(shared, actor, ids, expected, task, name, None)
}
pub fn edit_guarded(
    shared: &Shared,
    actor: &str,
    ids: &[String],
    expected: Option<u64>,
    task: Option<&str>,
    name: Option<&str>,
    document: Option<&str>,
) -> Result<Value, String> {
    let doc = {
        let mut e = shared.lock().unwrap();
        if document.is_some_and(|id| id != crate::engine::view::current(&e).id()) {
            return Err("Project source changed before merging".into());
        }
        e.check(
            actor,
            &[Scope {
                target: None,
                rect: None,
            }],
        )?;
        if expected.is_some_and(|r| r != crate::engine::view::current(&e).revision()) {
            return Err("Conflicting document change. Observe the layers before merging.".into());
        }
        e.resident()?.clone()
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
        e.resident_state_mut().expect("resident fixture").document = Document::new(4, 4).unwrap();
        let first = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
        e.resident_state_mut().expect("resident fixture").document.layers.push(Layer::new("Second", "paint", 4, 4));
        let prepared = prepare(&e.resident_state().expect("resident fixture").document, &[first.clone()], None).unwrap();
        e.edit(
            "human",
            &[json!({"op":"layer.update","layer":first,"name":"Human work"})],
            None,
            None,
            "Rename",
        )
        .unwrap();
        let before = serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap();
        let history = e.resident_state().expect("resident fixture").undo.len();
        assert!(e
            .edit_prepared_merge(
                "artist",
                json!({"op":"layer.merge","layers":[first]}),
                None,
                None,
                prepared
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.resident_state().expect("resident fixture").document).unwrap(), before);
        assert_eq!(e.resident_state().expect("resident fixture").undo.len(), history);
    }
}
