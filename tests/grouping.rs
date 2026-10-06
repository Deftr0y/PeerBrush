use peerbrush::engine::{Document, Engine, Layer, Scope};
use serde_json::json;
fn setup() -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new(8, 8).unwrap();
    e.doc.layers = (0..3)
        .map(|i| Layer::new(&format!("Layer {i}"), "paint", 8, 8))
        .collect();
    e
}
#[test]
fn selected_layers_enter_new_folder_in_order_with_one_undo() {
    let mut e = setup();
    let ids = vec![e.doc.layers[0].id.clone(), e.doc.layers[2].id.clone()];
    let result = e
        .edit(
            "human",
            &[json!({"op":"group.create_selected","layers":ids})],
            Some(0),
            None,
            "Group",
        )
        .unwrap();
    let folder = result["created"][0].as_str().unwrap();
    assert_eq!(e.doc.layers[0].id, folder);
    assert_eq!(e.doc.layers[1].id, ids[0]);
    assert_eq!(e.doc.layers[2].id, ids[1]);
    assert_eq!(e.doc.layers[1].parent.as_deref(), Some(folder));
    assert_eq!(e.doc.layers[2].parent.as_deref(), Some(folder));
    assert_eq!(e.undo.len(), 1);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers.len(), 3);
    assert!(e.doc.layers.iter().all(|l| l.parent.is_none()));
    e.redo("human").unwrap();
    assert_eq!(e.doc.layers.len(), 4);
}
#[test]
fn folders_keep_children_and_nested_sources_use_common_ancestor() {
    let mut e = setup();
    let mut a = Layer::new("A", "group", 8, 8);
    let b = Layer::new("B", "group", 8, 8);
    a.parent = Some(b.id.clone());
    e.doc.layers[0].parent = Some(a.id.clone());
    let child = e.doc.layers[0].id.clone();
    let outside = e.doc.layers[2].id.clone();
    let a_id = a.id.clone();
    e.doc.layers.insert(0, a);
    e.doc.layers.insert(0, b);
    let r = e
        .edit(
            "human",
            &[json!({"op":"group.create_selected","layers":[a_id,child,outside]})],
            None,
            None,
            "Group",
        )
        .unwrap();
    let group = r["created"][0].as_str().unwrap();
    assert!(e
        .doc
        .layers
        .iter()
        .find(|l| l.id == group)
        .unwrap()
        .parent
        .is_none());
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.id == a_id)
            .unwrap()
            .parent
            .as_deref(),
        Some(group)
    );
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.id == child)
            .unwrap()
            .parent
            .as_deref(),
        Some(a_id.as_str())
    );
}
#[test]
fn grouping_rejects_locked_descendants_and_reservations_atomically() {
    let mut e = setup();
    let id = e.doc.layers[0].id.clone();
    e.doc.layers[0].locked = true;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"group.create_selected","layers":[id]})],
            None,
            None,
            "Group"
        )
        .is_err());
    assert!(e.undo.is_empty());
    assert_eq!(e.doc.layers.len(), 3);
    e.doc.layers[0].locked = false;
    e.reserve("artist", "Painting", vec![Scope::layer(&id)])
        .unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"group.create_selected","layers":[id]})],
            None,
            None,
            "Group"
        )
        .unwrap_err()
        .contains("Reserved"));
    assert!(e.undo.is_empty());
}

#[test]
fn grouping_uses_visible_hierarchy_order_even_when_storage_is_not_preorder() {
    let mut e = setup();
    let a = Layer::new("Folder A", "group", 8, 8);
    let b = Layer::new("Folder B", "group", 8, 8);
    let mut child_a = Layer::new("Child A", "paint", 8, 8);
    child_a.parent = Some(a.id.clone());
    let mut child_b = Layer::new("Child B", "paint", 8, 8);
    child_b.parent = Some(b.id.clone());
    let ids = vec![child_b.id.clone(), child_a.id.clone()];
    e.doc.layers = vec![child_b, a, child_a, b];
    let r = e
        .edit(
            "human",
            &[json!({"op":"group.create_selected","layers":ids})],
            None,
            None,
            "Group",
        )
        .unwrap();
    let group = r["created"][0].as_str().unwrap();
    let children = e
        .doc
        .layers
        .iter()
        .filter(|l| l.parent.as_deref() == Some(group))
        .map(|l| l.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(children, ["Child A", "Child B"]);
}
