use peerbrush::{
    effects::Effect,
    engine::{self, Document, Layer, Mask, MaskStep},
    layer_clipboard,
    raster::{Raster, TILE},
};
use serde_json::{json, Value};
use std::sync::Arc;

fn rich_tree() -> (Document, String, String) {
    let mut doc = Document::new(32, 24).unwrap();
    let folder = Layer::new("Folder", "group", 32, 24);
    let mut nested = Layer::new("Nested", "group", 32, 24);
    nested.parent = Some(folder.id.clone());
    let mut child = Layer::new("Ink", "paint", 16, 12);
    child.parent = Some(nested.id.clone());
    child.x = -2;
    child.y = 3;
    child.opacity = 0.6;
    child.blend = "multiply".into();
    child.pixels.set(4, 5, [123, 75, 19, 200]);
    let mut mask_pixels = Raster::new(16, 12);
    mask_pixels.set(4, 5, [60, 60, 60, 255]);
    child.mask = Some(Mask {
        enabled: true,
        cache_key: engine::id(),
        steps: vec![
            MaskStep {
                id: engine::id(),
                kind: "fill".into(),
                enabled: true,
                value: 255.,
                pixels: Raster::new(16, 12),
                settings: json!({}),
            },
            MaskStep {
                id: engine::id(),
                kind: "paint".into(),
                enabled: true,
                value: 255.,
                pixels: mask_pixels,
                settings: json!({}),
            },
        ],
    });
    child.effects.push(Effect {
        id: engine::id(),
        kind: "invert".into(),
        enabled: true,
        settings: json!({}),
    });
    let root = folder.id.clone();
    let ink = child.id.clone();
    // Native documents need not store the hierarchy in preorder.
    doc.layers = vec![
        child,
        nested,
        folder,
        Layer::new("Outside", "paint", 32, 24),
    ];
    (doc, root, ink)
}
fn value(doc: &Document) -> Value {
    serde_json::to_value(doc).unwrap()
}

#[test]
fn copied_folders_include_descendants_once_share_tiles_and_roundtrip_metadata() {
    let (doc, folder, ink) = rich_tree();
    let copied = layer_clipboard::copy(&doc, &[ink.clone(), folder.clone()]).unwrap();
    assert_eq!(copied.roots, vec![folder]);
    assert_eq!(copied.layers.len(), 3);
    let source = doc.layers.iter().find(|l| l.id == ink).unwrap();
    let snapshot = copied.layers.iter().find(|l| l.id == ink).unwrap();
    assert!(Arc::ptr_eq(
        source.pixels.tiles.values().next().unwrap(),
        snapshot.pixels.tiles.values().next().unwrap()
    ));
    assert!(Arc::ptr_eq(
        source.mask.as_ref().unwrap().steps[1]
            .pixels
            .tiles
            .values()
            .next()
            .unwrap(),
        snapshot.mask.as_ref().unwrap().steps[1]
            .pixels
            .tiles
            .values()
            .next()
            .unwrap()
    ));
    let decoded: layer_clipboard::Layers =
        serde_json::from_value(serde_json::to_value(&copied).unwrap()).unwrap();
    layer_clipboard::validate(&decoded).unwrap();
    let decoded = decoded.layers.iter().find(|l| l.id == ink).unwrap();
    assert_eq!(decoded.pixels.get(4, 5), [123, 75, 19, 200]);
    assert_eq!(decoded.effects[0].kind, "invert");
    assert_eq!(
        decoded.mask.as_ref().unwrap().steps[1].pixels.get(4, 5),
        [60, 60, 60, 255]
    );
}

#[test]
fn duplication_remaps_every_identity_and_detaches_only_modified_tiles() {
    let (mut doc, folder, ink) = rich_tree();
    let before = doc.clone();
    let created = layer_clipboard::duplicate(&mut doc, &[folder.clone(), ink.clone()]).unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(doc.layers.len(), 7);
    let clone_root = doc.layers.iter().find(|l| l.id == created[0]).unwrap();
    assert_eq!(clone_root.name, "Folder copy");
    assert!(clone_root.parent.is_none());
    let old_child = before.layers.iter().find(|l| l.id == ink).unwrap();
    let clone_at = doc
        .layers
        .iter()
        .position(|l| l.name == "Ink" && l.id != ink)
        .unwrap();
    let clone = &doc.layers[clone_at];
    assert_ne!(clone.id, old_child.id);
    assert_ne!(clone.parent, old_child.parent);
    assert_ne!(clone.effect_key, old_child.effect_key);
    assert_ne!(clone.effects[0].id, old_child.effects[0].id);
    assert_ne!(
        clone.mask.as_ref().unwrap().cache_key,
        old_child.mask.as_ref().unwrap().cache_key
    );
    for (new, old) in clone
        .mask
        .as_ref()
        .unwrap()
        .steps
        .iter()
        .zip(&old_child.mask.as_ref().unwrap().steps)
    {
        assert_ne!(new.id, old.id);
    }
    assert!(Arc::ptr_eq(
        clone.pixels.tiles.values().next().unwrap(),
        old_child.pixels.tiles.values().next().unwrap()
    ));
    doc.layers[clone_at].pixels.set(4, 5, [255, 0, 0, 255]);
    assert_eq!(
        doc.layers
            .iter()
            .find(|l| l.id == ink)
            .unwrap()
            .pixels
            .get(4, 5),
        [123, 75, 19, 200]
    );
    assert_eq!(old_child.pixels.get(4, 5), [123, 75, 19, 200]);
}

#[test]
fn multi_duplicate_keeps_stable_sibling_order_and_original_parents() {
    let mut doc = Document::new(8, 8).unwrap();
    let folder = Layer::new("Folder", "group", 8, 8);
    let mut a = Layer::new("A", "paint", 8, 8);
    a.parent = Some(folder.id.clone());
    let mut b = Layer::new("B", "paint", 8, 8);
    b.parent = a.parent.clone();
    let ids = vec![b.id.clone(), a.id.clone()];
    let parent = folder.id.clone();
    doc.layers = vec![a, b, folder];
    let created = layer_clipboard::duplicate(&mut doc, &ids).unwrap();
    assert_eq!(created.len(), 2);
    let children = doc
        .layers
        .iter()
        .filter(|l| l.parent.as_deref() == Some(parent.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        children.iter().map(|l| l.name.as_str()).collect::<Vec<_>>(),
        ["A copy", "A", "B copy", "B"]
    );
    assert_eq!(children[0].id, created[0]);
    assert_eq!(children[2].id, created[1]);
}

#[test]
fn copied_roots_follow_visible_hierarchy_order_even_when_storage_is_not_preorder() {
    let mut doc = Document::new(8, 8).unwrap();
    let a = Layer::new("Folder A", "group", 8, 8);
    let b = Layer::new("Folder B", "group", 8, 8);
    let mut child_a = Layer::new("Child A", "paint", 8, 8);
    child_a.parent = Some(a.id.clone());
    let mut child_b = Layer::new("Child B", "paint", 8, 8);
    child_b.parent = Some(b.id.clone());
    let a_id = child_a.id.clone();
    let b_id = child_b.id.clone();
    // Folder A displays first, although B's child occupies the first storage slot.
    doc.layers = vec![child_b, a, child_a, b];
    let snapshot = layer_clipboard::copy(&doc, &[b_id.clone(), a_id.clone()]).unwrap();
    assert_eq!(snapshot.roots, vec![a_id, b_id]);
    let mut pasted = Document::new(8, 8).unwrap();
    pasted.layers[0].kind = "group".into();
    let target = pasted.layers[0].id.clone();
    let roots = layer_clipboard::paste(&mut pasted, &snapshot, &target).unwrap();
    assert_eq!(
        pasted
            .layers
            .iter()
            .filter(|l| l.parent.as_deref() == Some(target.as_str()))
            .map(|l| l.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Child A copy", "Child B copy"]
    );
    assert_eq!(
        roots,
        vec![pasted.layers[1].id.clone(), pasted.layers[2].id.clone()]
    );
}

#[test]
fn paste_wraps_top_level_destination_and_places_editable_tree_inside_selected_folder() {
    let (source, folder, _) = rich_tree();
    let snapshot = layer_clipboard::copy(&source, &[folder]).unwrap();
    let mut doc = Document::new(64, 48).unwrap();
    doc.layers[0].blend = "screen".into();
    let old = doc.layers[0].id.clone();
    let created = layer_clipboard::paste(&mut doc, &snapshot, &old).unwrap();
    assert_eq!(created.len(), 1);
    assert_eq!(doc.layers.len(), 5);
    let wrapper = &doc.layers[0];
    assert_eq!(wrapper.kind, "group");
    assert_eq!(wrapper.blend, "screen");
    assert_eq!(
        doc.layers.iter().find(|l| l.id == old).unwrap().blend,
        "normal"
    );
    assert_eq!(
        doc.layers
            .iter()
            .find(|l| l.id == created[0])
            .unwrap()
            .parent
            .as_ref(),
        Some(&wrapper.id)
    );
    assert_eq!(
        doc.layers
            .iter()
            .find(|l| l.id == old)
            .unwrap()
            .parent
            .as_ref(),
        Some(&wrapper.id)
    );
    let wrapper_id = wrapper.id.clone();
    let next = layer_clipboard::paste(&mut doc, &snapshot, &wrapper_id).unwrap();
    assert_eq!(
        doc.layers
            .iter()
            .find(|l| l.id == next[0])
            .unwrap()
            .parent
            .as_deref(),
        Some(wrapper_id.as_str())
    );
    assert_eq!(doc.layers.iter().filter(|l| l.parent.is_none()).count(), 1);
}

#[test]
fn paste_into_empty_document_handles_cut_target_and_malformed_snapshots_atomically() {
    let (source, folder, _) = rich_tree();
    let snapshot = layer_clipboard::copy(&source, &[folder]).unwrap();
    let mut doc = Document::new(8, 8).unwrap();
    doc.layers.clear();
    let roots = layer_clipboard::paste(&mut doc, &snapshot, "deleted-cut-target").unwrap();
    assert_eq!(doc.layers.len(), 3);
    assert!(doc
        .layers
        .iter()
        .find(|l| l.id == roots[0])
        .unwrap()
        .parent
        .is_none());
    let before = value(&doc);
    let mut invalid = snapshot.clone();
    invalid.layers[0].parent = Some("missing-parent".into());
    assert!(layer_clipboard::paste(&mut doc, &invalid, "deleted-cut-target").is_err());
    assert_eq!(value(&doc), before);
    let mut invalid = snapshot.clone();
    invalid.roots.push(invalid.roots[0].clone());
    assert!(layer_clipboard::paste(&mut doc, &invalid, "").is_err());
    assert_eq!(value(&doc), before);
    let mut invalid = snapshot.clone();
    invalid.layers[0].pixels.width = 100000;
    assert!(layer_clipboard::paste(&mut doc, &invalid, "").is_err());
    assert_eq!(value(&doc), before);
}

#[test]
fn cut_checks_locked_descendants_and_ancestors_while_copy_remains_read_only() {
    let (mut doc, folder, ink) = rich_tree();
    doc.layers.iter_mut().find(|l| l.id == ink).unwrap().locked = true;
    assert!(layer_clipboard::copy(&doc, &[folder.clone()]).is_ok());
    assert!(layer_clipboard::cut_commands(&doc, &[folder.clone()]).is_err());
    doc.layers.iter_mut().find(|l| l.id == ink).unwrap().locked = false;
    doc.layers
        .iter_mut()
        .find(|l| l.id == folder)
        .unwrap()
        .locked = true;
    assert!(layer_clipboard::cut_commands(&doc, &[ink.clone()]).is_err());
    let before = value(&doc);
    assert!(layer_clipboard::duplicate(&mut doc, &[ink.clone()]).is_err());
    assert_eq!(value(&doc), before);
    doc.layers
        .iter_mut()
        .find(|l| l.id == folder)
        .unwrap()
        .locked = false;
    let commands = layer_clipboard::cut_commands(&doc, &[folder.clone(), ink]).unwrap();
    assert_eq!(commands, vec![json!({"op":"layer.delete","layer":folder})]);
}

#[test]
fn explicit_missing_paste_target_never_redirects_to_an_unrelated_existing_layer() {
    let (source, folder, _) = rich_tree();
    let snapshot = layer_clipboard::copy(&source, &[folder]).unwrap();
    let mut doc = Document::new(8, 8).unwrap();
    let before = value(&doc);
    let error = layer_clipboard::paste(&mut doc, &snapshot, "deleted-target").unwrap_err();
    assert!(error.contains("destination no longer exists"));
    assert_eq!(value(&doc), before);
    assert!(layer_clipboard::paste(&mut doc, &snapshot, "").is_ok());
}

#[test]
fn layer_count_depth_and_raster_budgets_reject_before_any_document_mutation() {
    let mut source = Document::new(8, 8).unwrap();
    source.layers = (0..100)
        .map(|i| Layer::new(&format!("L{i}"), "paint", 8, 8))
        .collect();
    let ids = source
        .layers
        .iter()
        .map(|l| l.id.clone())
        .collect::<Vec<_>>();
    let snapshot = layer_clipboard::copy(&source, &ids).unwrap();
    let mut doc = Document::new(8, 8).unwrap();
    let before = value(&doc);
    let target = doc.layers[0].id.clone();
    assert!(layer_clipboard::paste(&mut doc, &snapshot, &target).is_err());
    assert_eq!(value(&doc), before);
    let mut source = Document::new(8, 8).unwrap();
    source.layers.clear();
    let mut parent = None;
    for i in 0..16 {
        let mut group = Layer::new(&format!("Folder{i}"), "group", 8, 8);
        group.parent = parent;
        parent = Some(group.id.clone());
        source.layers.push(group);
    }
    let mut leaf = Layer::new("Leaf", "paint", 8, 8);
    leaf.parent = parent;
    source.layers.push(leaf);
    let snapshot = layer_clipboard::copy(&source, &[source.layers[0].id.clone()]).unwrap();
    let mut doc = Document::new(8, 8).unwrap();
    doc.layers[0].kind = "group".into();
    let before = value(&doc);
    let target = doc.layers[0].id.clone();
    assert!(layer_clipboard::paste(&mut doc, &snapshot, &target).is_err());
    assert_eq!(value(&doc), before);
    // Hundreds of logical tiles share one allocation; test the budget without allocating 512 MiB.
    let mut full = Raster::new(8192, 4096);
    let tile = Arc::new(vec![1; (TILE * TILE * 4) as usize]);
    for y in 0..4096 / TILE {
        for x in 0..8192 / TILE {
            full.tiles.insert((x, y), tile.clone());
        }
    }
    let mut doc = Document::new(8192, 4096).unwrap();
    doc.layers = (0..4)
        .map(|i| {
            let mut l = Layer::new(&format!("L{i}"), "paint", 8192, 4096);
            l.pixels = full.clone();
            l
        })
        .collect();
    let target = doc.layers[0].id.clone();
    let before_ids = doc.layers.iter().map(|l| l.id.clone()).collect::<Vec<_>>();
    assert!(layer_clipboard::duplicate(&mut doc, &[target]).is_err());
    assert_eq!(
        doc.layers.iter().map(|l| l.id.clone()).collect::<Vec<_>>(),
        before_ids
    );
    assert_eq!(doc.layers.len(), 4);
}

#[test]
fn pasted_content_invalidates_cached_destination_folder_effects() {
    let mut source = Document::new(4, 4).unwrap();
    source.layers[0].kind = "fill".into();
    source.layers[0].color = [60, 90, 120, 255];
    let snapshot = layer_clipboard::copy(&source, &[source.layers[0].id.clone()]).unwrap();
    let mut doc = Document::new(4, 4).unwrap();
    doc.layers[0].kind = "group".into();
    doc.layers[0].effects.push(Effect {
        id: engine::id(),
        kind: "invert".into(),
        enabled: true,
        settings: json!({}),
    });
    let target = doc.layers[0].id.clone();
    assert_eq!(doc.preview(None, 4, None, false).unwrap().2[3], 0);
    layer_clipboard::paste(&mut doc, &snapshot, &target).unwrap();
    assert_eq!(
        &doc.preview(None, 4, None, false).unwrap().2[..4],
        &[195, 165, 135, 255]
    );
}
