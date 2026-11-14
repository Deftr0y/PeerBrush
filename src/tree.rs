//! Plans atomic hierarchy edits without mutating the shared document during a drag.
use crate::engine::{
    view::{DocumentView, LayerView},
    Document,
};
use serde_json::{json, Value};
use std::collections::HashSet;

pub fn roots(doc: &Document, selected: &HashSet<String>) -> Vec<String> {
    roots_view(doc.into(), selected)
}
pub(crate) fn roots_view(doc: DocumentView<'_>, selected: &HashSet<String>) -> Vec<String> {
    doc.layers()
        .filter(|layer| {
            selected.contains(layer.id()) && {
                let mut parent = layer.parent();
                let mut contained = false;
                for _ in 0..=16 {
                    let Some(id) = parent else { break };
                    if selected.contains(id) {
                        contained = true;
                        break;
                    }
                    parent = doc.find_layer(id).and_then(|layer| layer.parent());
                }
                !contained
            }
        })
        .map(|layer| layer.id().to_owned())
        .collect()
}
/// Presentation-only layer ordinals and one parent override. It owns no raster,
/// source definition or copied native document metadata.
pub(crate) struct TreeOrder {
    order: Vec<usize>,
    moving: HashSet<usize>,
    parent: Option<String>,
}
impl TreeOrder {
    pub(crate) fn original(doc: DocumentView<'_>) -> Self {
        Self {
            order: (0..doc.layer_count()).collect(),
            moving: HashSet::new(),
            parent: None,
        }
    }
    pub(crate) fn indices(&self) -> &[usize] {
        &self.order
    }
    pub(crate) fn parent<'a>(&'a self, index: usize, layer: LayerView<'a>) -> Option<&'a str> {
        if self.moving.contains(&index) {
            self.parent.as_deref()
        } else {
            layer.parent()
        }
    }
}
pub(crate) fn reparent_view(
    doc: DocumentView<'_>,
    ids: &[String],
    parent: Option<&str>,
    before: Option<&str>,
) -> Result<(TreeOrder, Vec<Value>), String> {
    let moving = roots_view(doc, &ids.iter().cloned().collect());
    if moving.is_empty() {
        return Err("Choose layers to move".into());
    }
    let moving_ids: HashSet<_> = moving.iter().map(String::as_str).collect();
    let mut ancestor = parent;
    let mut depth = 0;
    while let Some(id) = ancestor {
        if moving_ids.contains(id) {
            return Err("A folder cannot be dropped into itself or its children".into());
        }
        let layer = doc
            .find_layer(id)
            .filter(|l| l.kind() == "group")
            .ok_or("Choose an existing folder")?;
        if layer.locked() {
            return Err("Destination folder is locked".into());
        }
        ancestor = layer.parent();
        depth += 1;
        if depth > 16 {
            return Err("Group nesting limit exceeded".into());
        }
    }
    let mut held = Vec::with_capacity(moving.len());
    let mut order = Vec::with_capacity(doc.layer_count());
    for (index, layer) in doc.layers().enumerate() {
        if moving_ids.contains(layer.id()) {
            held.push(index);
        } else {
            order.push(index);
        }
    }
    let at = before
        .and_then(|id| order.iter().position(|&i| doc.layer(i).unwrap().id() == id))
        .unwrap_or_else(|| {
            parent
                .and_then(|id| {
                    order
                        .iter()
                        .rposition(|&i| doc.layer(i).unwrap().parent() == Some(id))
                        .or_else(|| order.iter().position(|&i| doc.layer(i).unwrap().id() == id))
                })
                .map(|i| i + 1)
                .unwrap_or(order.len())
        });
    drop(order.splice(at..at, held.iter().copied()));
    let changed = order.iter().copied().ne(0..doc.layer_count())
        || held.iter().any(|&index| doc.layer(index).unwrap().parent() != parent);
    let plan = TreeOrder {
        order,
        moving: held.into_iter().collect(),
        parent: parent.map(str::to_owned),
    };
    if !changed {
        return Ok((plan, vec![]));
    }
    let mut commands = vec![];
    for id in &moving {
        commands.push(json!({"op":"layer.parent","layer":id,"parent":parent}));
    }
    // Only the selected roots move, preserving unrelated locked siblings.
    for id in &moving {
        commands.push(json!({"op":"layer.reorder","layer":id,"index":doc.layer_count()-1}));
    }
    for (n, id) in moving.iter().enumerate() {
        commands.push(json!({"op":"layer.reorder","layer":id,"index":at+n}));
    }
    Ok((plan, commands))
}
pub fn reparent(
    doc: &Document,
    ids: &[String],
    parent: Option<&str>,
    before: Option<&str>,
) -> Result<(Document, Vec<Value>), String> {
    let (plan, commands) = reparent_view(doc.into(), ids, parent, before)?;
    let mut preview = doc.clone();
    preview.layers = plan
        .indices()
        .iter()
        .map(|&index| {
            let mut layer = doc.layers[index].clone();
            layer.parent = plan
                .parent(index, (&doc.layers[index]).into())
                .map(str::to_owned);
            layer
        })
        .collect();
    Ok((preview, commands))
}

#[cfg(test)]
pub(crate) mod presentation_tests {
    use super::*;
    use crate::{
        engine::{
            native::NativeDocument,
            scene::{Document36, PendingScene, RasterCollector, RasterRef36, RasterRoster},
            view::DocumentSnapshot,
            Layer,
        },
        large_document::{DiskBudget, ResidentBudget},
        psd::memory36::{Admitted, MetadataCharge},
        raster::{Raster, Surface},
    };
    use std::sync::Arc;
    struct ZeroCollector(Vec<Surface>);
    impl RasterCollector for ZeroCollector {
        fn resident(&mut self, pixels: &Raster) -> Result<RasterRef36, String> {
            assert!(pixels.tiles.is_empty() && pixels.samples16.is_empty());
            self.0.push(
                Surface::zero(pixels.width, pixels.height, pixels.depth)
                    .map_err(|e| e.to_string())?,
            );
            Ok(RasterRef36 {
                id: self.0.len() as u32,
                width: pixels.width,
                height: pixels.height,
                depth: pixels.depth,
                retained: None,
            })
        }
        fn surface(&mut self, _: &Surface) -> Result<RasterRef36, String> {
            Err("Unexpected metadata fixture source".into())
        }
    }
    pub(crate) fn native(doc: &Document, pool: &ResidentBudget) -> NativeDocument {
        let charge = MetadataCharge::reserve(pool, 2 * 1024 * 1024, &|| Ok(())).unwrap();
        let mut collector = ZeroCollector(vec![]);
        let metadata = Document36::capture(doc, &mut collector).unwrap();
        NativeDocument::from_pending(
            PendingScene::new(
                Admitted::from_parts(Arc::new(metadata), charge),
                RasterRoster::new(collector.0).unwrap(),
                pool.clone(),
                DiskBudget::new(1024 * 1024, 4).unwrap(),
            )
            .unwrap(),
        )
        .unwrap()
    }
    #[test]
    fn native_tree_metadata_plans_match_atomic_resident_commands_and_keep_shared_snapshot_owners() {
        for depth in [8, 16] {
            let mut doc = Document::new_depth(9, 7, depth).unwrap();
            let outer = Layer::new("Outer", "group", 9, 7);
            let mut inner = Layer::new("Inner", "group", 9, 7);
            inner.parent = Some(outer.id.clone());
            let mut leaf = Layer::new("Leaf", "paint", 9, 7);
            leaf.parent = Some(inner.id.clone());
            let mut sibling = Layer::new("Sibling", "paint", 9, 7);
            sibling.parent = Some(outer.id.clone());
            let loose = Layer::new("Loose", "paint", 9, 7);
            let mut locked = Layer::new("Locked", "group", 9, 7);
            locked.locked = true;
            let ids = [
                outer.id.clone(),
                inner.id.clone(),
                leaf.id.clone(),
                sibling.id.clone(),
                loose.id.clone(),
                locked.id.clone(),
            ];
            doc.layers = vec![outer, inner, leaf, sibling, loose, locked];
            doc.ensure_depth();
            let pool = ResidentBudget::new(16 * 1024 * 1024).unwrap();
            let native = native(&doc, &pool);
            let original_key = native.key();
            let snapshot = DocumentSnapshot::from(native);
            let copy = snapshot.clone();
            let DocumentSnapshot::Native(owner) = &copy else {
                panic!("Native snapshot lost its owner")
            };
            assert_eq!(owner.key(), original_key);
            let admitted = pool.used();
            let cases = [
                (
                    vec![ids[3].clone()],
                    Some(ids[1].as_str()),
                    Some(ids[2].as_str()),
                ),
                (
                    vec![ids[1].clone(), ids[2].clone()],
                    None,
                    Some(ids[4].as_str()),
                ),
                (
                    vec![ids[2].clone()],
                    Some(ids[1].as_str()),
                    Some(ids[3].as_str()),
                ),
                (vec![ids[3].clone()], None, None),
                (
                    vec![ids[4].clone(), ids[3].clone()],
                    Some(ids[0].as_str()),
                    Some(ids[1].as_str()),
                ),
                (vec![ids[4].clone()], Some(ids[0].as_str()), None),
            ];
            for (case, (moving, parent, before)) in cases.into_iter().enumerate() {
                let (plan, commands) =
                    reparent_view(snapshot.view(), &moving, parent, before).unwrap();
                let (resident, old_commands) = reparent(&doc, &moving, parent, before).unwrap();
                assert_eq!(commands, old_commands);
                let expected_order: &[usize] = match case {
                    0 => &[0, 1, 3, 2, 4, 5],
                    1 => &[0, 2, 3, 1, 4, 5],
                    2 => &[0, 1, 2, 3, 4, 5],
                    3 => &[0, 1, 2, 4, 5, 3],
                    4 => &[0, 3, 4, 1, 2, 5],
                    5 => &[0, 1, 2, 3, 4, 5],
                    _ => unreachable!(),
                };
                assert_eq!(plan.indices(), expected_order);
                let expected = match case {
                    0 => vec![
                        json!({"op":"layer.parent","layer":ids[3],"parent":ids[1]}),
                        json!({"op":"layer.reorder","layer":ids[3],"index":5}),
                        json!({"op":"layer.reorder","layer":ids[3],"index":2}),
                    ],
                    1 => vec![
                        json!({"op":"layer.parent","layer":ids[1],"parent":null}),
                        json!({"op":"layer.reorder","layer":ids[1],"index":5}),
                        json!({"op":"layer.reorder","layer":ids[1],"index":3}),
                    ],
                    2 => vec![],
                    3 => vec![
                        json!({"op":"layer.parent","layer":ids[3],"parent":null}),
                        json!({"op":"layer.reorder","layer":ids[3],"index":5}),
                        json!({"op":"layer.reorder","layer":ids[3],"index":5}),
                    ],
                    4 => vec![
                        json!({"op":"layer.parent","layer":ids[3],"parent":ids[0]}),
                        json!({"op":"layer.parent","layer":ids[4],"parent":ids[0]}),
                        json!({"op":"layer.reorder","layer":ids[3],"index":5}),
                        json!({"op":"layer.reorder","layer":ids[4],"index":5}),
                        json!({"op":"layer.reorder","layer":ids[3],"index":1}),
                        json!({"op":"layer.reorder","layer":ids[4],"index":2}),
                    ],
                    5 => vec![
                        json!({"op":"layer.parent","layer":ids[4],"parent":ids[0]}),
                        json!({"op":"layer.reorder","layer":ids[4],"index":5}),
                        json!({"op":"layer.reorder","layer":ids[4],"index":4}),
                    ],
                    _ => unreachable!(),
                };
                assert_eq!(commands, expected);
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = doc.clone();
                if !commands.is_empty() {
                    engine
                        .edit("human", &commands, None, None, "Move layers")
                        .unwrap();
                }
                for (position, &index) in plan.indices().iter().enumerate() {
                    let layer = snapshot.view().layer(index).unwrap();
                    assert_eq!(layer.id(), resident.layers[position].id);
                    assert_eq!(
                        plan.parent(index, layer),
                        resident.layers[position].parent.as_deref()
                    );
                    assert_eq!(resident.layers[position].id, engine.resident_state().expect("resident fixture").document.layers[position].id);
                    assert_eq!(
                        resident.layers[position].parent,
                        engine.resident_state().expect("resident fixture").document.layers[position].parent
                    );
                }
                assert_eq!(pool.used(), admitted);
            }
            for (moving, parent, error) in [
                (
                    vec![ids[0].clone()],
                    Some(ids[1].as_str()),
                    "A folder cannot be dropped into itself or its children",
                ),
                (
                    vec![ids[2].clone()],
                    Some(ids[5].as_str()),
                    "Destination folder is locked",
                ),
                (
                    vec![ids[2].clone()],
                    Some(ids[4].as_str()),
                    "Choose an existing folder",
                ),
                (vec![], None, "Choose layers to move"),
            ] {
                assert_eq!(
                    reparent_view(snapshot.view(), &moving, parent, None)
                        .err()
                        .unwrap(),
                    error
                );
                assert_eq!(reparent(&doc, &moving, parent, None).err().unwrap(), error);
            }
            assert_eq!(pool.used(), admitted);
            assert_eq!(copy.view().depth(), depth);
            assert_eq!(copy.view().layer_count(), 6);
            drop(copy);
            drop(snapshot);
            assert_eq!(pool.used(), 0);
        }
    }
}
