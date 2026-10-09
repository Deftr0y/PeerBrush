//! Atomic folder creation around selected roots, retaining complete subtrees.
use crate::engine::{Document, Layer};
use serde_json::Value;
use std::collections::HashSet;

pub fn requested(command: &Value) -> Result<Vec<String>, String> {
    if let Some(ids) = command.get("layers") {
        ids.as_array()
            .ok_or("Layers must be an array of IDs")?
            .iter()
            .map(|id| {
                id.as_str()
                    .filter(|id| !id.is_empty())
                    .map(String::from)
                    .ok_or_else(|| "Choose valid layer IDs".into())
            })
            .collect()
    } else if let Some(id) = command["layer"].as_str().filter(|id| !id.is_empty()) {
        Ok(vec![id.into()])
    } else {
        Ok(vec![])
    }
}

pub fn create(doc: &mut Document, ids: &[String], name: &str) -> Result<String, String> {
    if doc.read_only {
        return Err("This PSD is read-only".into());
    }
    if doc.layers.len() >= 100 {
        return Err("Initial version supports up to 100 layers".into());
    }
    if ids.is_empty() {
        let mut folder = Layer::new(name, "group", doc.width, doc.height);
        folder.pixels = crate::raster::Raster::new_depth(doc.width, doc.height, doc.bit_depth);
        let id = folder.id.clone();
        doc.layers.insert(0, folder);
        return Ok(id);
    }
    // Checks source descendants and locked ancestors before changing anything.
    crate::layer_clipboard::cut_commands(doc, ids)?;
    let snapshot = crate::layer_clipboard::copy(doc, ids)?;
    let included: HashSet<_> = snapshot.layers.iter().map(|l| l.id.as_str()).collect();
    let roots: HashSet<_> = snapshot.roots.iter().map(|id| id.as_str()).collect();
    let ancestors = |id: &str| {
        let mut result = vec![];
        let mut parent = doc
            .layers
            .iter()
            .find(|l| l.id == id)
            .and_then(|l| l.parent.clone());
        while let Some(id) = parent {
            parent = doc
                .layers
                .iter()
                .find(|l| l.id == id)
                .and_then(|l| l.parent.clone());
            result.push(id);
        }
        result
    };
    let chains: Vec<_> = snapshot.roots.iter().map(|id| ancestors(id)).collect();
    let parent = chains[0]
        .iter()
        .find(|id| chains.iter().all(|chain| chain.contains(id)))
        .cloned();
    // Insert at the first selected branch under the common containing folder.
    let mut anchor = snapshot.roots[0].clone();
    while let Some(current) = doc.layers.iter().find(|l| l.id == anchor) {
        if current.parent == parent {
            break;
        }
        let Some(next) = &current.parent else {
            break;
        };
        anchor = next.clone();
    }
    let before = doc.layers.iter().position(|l| l.id == anchor).unwrap();
    let at = doc.layers[..before]
        .iter()
        .filter(|l| !included.contains(l.id.as_str()))
        .count();
    let mut folder = Layer::new(name, "group", doc.width, doc.height);
    folder.pixels = crate::raster::Raster::new_depth(doc.width, doc.height, doc.bit_depth);
    folder.parent = parent;
    let group = folder.id.clone();
    let mut draft = doc.clone();
    draft.layers.retain(|l| !included.contains(l.id.as_str()));
    fn emit(id: &str, layers: &[Layer], roots: &HashSet<&str>, group: &str, out: &mut Vec<Layer>) {
        let mut layer = layers.iter().find(|l| l.id == id).unwrap().clone();
        if roots.contains(layer.id.as_str()) {
            layer.parent = Some(group.into());
        }
        out.push(layer);
        for child in layers.iter().filter(|l| l.parent.as_deref() == Some(id)) {
            emit(&child.id, layers, roots, group, out);
        }
    }
    let mut moved = vec![folder];
    for root in &snapshot.roots {
        emit(root, &snapshot.layers, &roots, &group, &mut moved);
    }
    draft.layers.splice(at..at, moved);
    crate::psd::validate(&draft)?;
    *doc = draft;
    Ok(group)
}
