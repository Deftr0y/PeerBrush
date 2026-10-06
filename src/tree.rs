//! Plans atomic hierarchy edits without mutating the shared document during a drag.
use crate::engine::Document;
use serde_json::{json, Value};
use std::collections::HashSet;

pub fn roots(doc: &Document, selected: &HashSet<String>) -> Vec<String> {
    doc.layers
        .iter()
        .filter(|l| {
            selected.contains(&l.id) && {
                let mut parent = l.parent.as_deref();
                let mut contained = false;
                for _ in 0..=16 {
                    let Some(id) = parent else { break };
                    if selected.contains(id) {
                        contained = true;
                        break;
                    }
                    parent = doc
                        .layers
                        .iter()
                        .find(|l| l.id == id)
                        .and_then(|l| l.parent.as_deref());
                }
                !contained
            }
        })
        .map(|l| l.id.clone())
        .collect()
}
pub fn reparent(
    doc: &Document,
    ids: &[String],
    parent: Option<&str>,
    before: Option<&str>,
) -> Result<(Document, Vec<Value>), String> {
    let moving = roots(doc, &ids.iter().cloned().collect());
    if moving.is_empty() {
        return Err("Choose layers to move".into());
    }
    let mut ancestor = parent;
    let mut depth = 0;
    while let Some(id) = ancestor {
        if moving.iter().any(|s| s == id) {
            return Err("A folder cannot be dropped into itself or its children".into());
        }
        let layer = doc
            .layers
            .iter()
            .find(|l| l.id == id && l.kind == "group")
            .ok_or("Choose an existing folder")?;
        if layer.locked {
            return Err("Destination folder is locked".into());
        }
        ancestor = layer.parent.as_deref();
        depth += 1;
        if depth > 16 {
            return Err("Group nesting limit exceeded".into());
        }
    }
    let mut preview = doc.clone();
    let mut held = vec![];
    preview.layers.retain(|l| {
        if moving.contains(&l.id) {
            held.push(l.clone());
            false
        } else {
            true
        }
    });
    let at = before
        .and_then(|id| preview.layers.iter().position(|l| l.id == id))
        .unwrap_or_else(|| {
            parent
                .and_then(|id| {
                    preview
                        .layers
                        .iter()
                        .rposition(|l| l.parent.as_deref() == Some(id))
                        .or_else(|| preview.layers.iter().position(|l| l.id == id))
                })
                .map(|i| i + 1)
                .unwrap_or(preview.layers.len())
        });
    for (n, mut layer) in held.into_iter().enumerate() {
        layer.parent = parent.map(String::from);
        preview.layers.insert(at + n, layer);
    }
    if doc
        .layers
        .iter()
        .map(|l| (&l.id, &l.parent))
        .eq(preview.layers.iter().map(|l| (&l.id, &l.parent)))
    {
        return Ok((preview, vec![]));
    }
    let mut commands = vec![];
    for id in &moving {
        commands.push(json!({"op":"layer.parent","layer":id,"parent":parent}));
    }
    // Collect the moving block at the end, then insert it in order. Only selected
    // layers are reordered, so unrelated locked siblings need not be unlocked.
    for id in &moving {
        commands.push(json!({"op":"layer.reorder","layer":id,"index":doc.layers.len()-1}));
    }
    for (n, id) in moving.iter().enumerate() {
        commands.push(json!({"op":"layer.reorder","layer":id,"index":at+n}));
    }
    Ok((preview, commands))
}
