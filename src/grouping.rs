//! Atomic folder creation around selected roots, retaining complete subtrees.
use crate::engine::{Document, Layer};
use serde_json::Value;
mod plan;
pub(crate) use plan::Plan;

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
    let plan = Plan::new((&*doc).into(), ids)?;
    // Preserve the resident clipboard/source validation before reordering.
    if !ids.is_empty() {
        crate::layer_clipboard::cut_commands(doc, ids)?;
    }
    let mut folder = Layer::new(name, "group", doc.width, doc.height);
    folder.pixels = crate::raster::Raster::new_depth(doc.width, doc.height, doc.bit_depth);
    folder.parent = plan.parent.map(|index| doc.layers[index].id.clone());
    let group = folder.id.clone();
    let mut draft = doc.clone();
    for (index, layer) in draft.layers.iter_mut().enumerate() {
        if plan.root(index) {
            layer.parent = Some(group.clone());
        }
    }
    plan.apply(&mut draft.layers, folder)?;
    crate::psd::validate(&draft)?;
    *doc = draft;
    Ok(group)
}
