//! Atomic folder creation around selected roots, retaining complete subtrees.
use crate::engine::{Document, Layer};
use crate::engine::view::DocumentView;
use serde_json::Value;
mod plan;
pub(crate) use plan::Plan;

pub(crate) fn layer_insert_slot(
    doc: DocumentView<'_>,
    command: &Value,
) -> Result<(usize, Option<String>), String> {
    let mut index = 0;
    let parent = if let Some(relative) = command.get("relative_to") {
        if command.get("parent").is_some() {
            return Err("Choose a relative layer or an explicit parent".into());
        }
        let relative = relative.as_str().ok_or("Choose a valid relative layer ID")?;
        let (slot, layer) = doc.layers().enumerate().find(|(_, l)| l.id() == relative)
            .ok_or("The relative layer no longer exists")?;
        if layer.kind() == "group" {
            index = doc.layers().position(|child| child.parent() == Some(relative)).unwrap_or(slot + 1);
            Some(relative)
        } else {
            index = slot;
            layer.parent()
        }
    } else {
        command["parent"].as_str()
    };
    let mut ancestor = parent;
    for _ in 0..=16 {
        let Some(id) = ancestor else {
            return Ok((index, parent.map(str::to_owned)));
        };
        let group = doc.find_layer(id).filter(|layer| layer.kind() == "group")
            .ok_or("Parent must be an existing group")?;
        if group.locked() {
            return Err("Parent group is locked".into());
        }
        ancestor = group.parent();
    }
    Err("Layer nesting exceeds the supported depth".into())
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::Engine;
    use serde_json::json;

    #[test]
    fn relative_layer_creation_uses_the_selected_folder_and_rejects_locked_parents() {
        let mut engine = Engine::from_document(Document::new_depth(12, 12, 16).unwrap());
        engine.edit("human", &[json!({"op":"layer.add","kind":"group","name":"Outer"})],
            None, None, "Create outer").unwrap();
        let outer = engine.resident_state().unwrap().document.layers[0].id.clone();
        engine.edit("human", &[json!({"op":"layer.add","kind":"group","parent":outer,"name":"Inner"})],
            None, None, "Create inner").unwrap();
        let inner = engine.resident_state().unwrap().document.layers[0].id.clone();
        let add = |id: &str, name: &str| json!({"op":"layer.add","relative_to":id,"name":name});
        engine.edit("human", &[add(&inner, "Child")], None, None, "Child").unwrap();
        let child = engine.resident_state().unwrap().document.layers.iter()
            .find(|layer| layer.name == "Child").unwrap().id.clone();
        engine.edit("human", &[add(&child, "Above")], None, None, "Sibling").unwrap();
        let layers = &engine.resident_state().unwrap().document.layers;
        let index = layers.iter().position(|layer| layer.id == child).unwrap();
        assert_eq!(layers[index - 1].name, "Above");
        assert_eq!(layers[index - 1].parent.as_deref(), Some(inner.as_str()));
        assert_eq!(layers[index].parent.as_deref(), Some(inner.as_str()));
        engine.undo("human").unwrap();
        engine.edit("human", &[json!({"op":"layer.update","layer":outer,"locked":true})],
            None, None, "Lock ancestor").unwrap();
        assert!(engine.edit("human", &[add(&inner, "Refused")], None, None, "Locked insertion").is_err());
        assert!(engine.resident_state().unwrap().document.layers.iter().all(|layer| layer.name != "Refused"));
    }
}
