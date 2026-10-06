use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    layer_clipboard,
};
use serde_json::json;

#[test]
fn engine_paste_and_duplicate_keep_all_roots_in_one_shared_history_entry() {
    let mut e = Engine::new();
    e.doc = Document::new(8, 8).unwrap();
    let mut a = Layer::new("A", "paint", 8, 8);
    a.pixels.set(2, 2, [200, 50, 20, 255]);
    let b = Layer::new("B", "paint", 8, 8);
    let originals = vec![a.id.clone(), b.id.clone()];
    e.doc.layers = vec![a, b];
    let snapshot = layer_clipboard::copy(&e.doc, &originals).unwrap();
    let result = e
        .paste_layers("artist", &snapshot, &originals[0], Some(0), None)
        .unwrap();
    assert_eq!(result["created"].as_array().unwrap().len(), 3);
    assert_eq!(result["created_roots"].as_array().unwrap().len(), 2);
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.doc.revision, 1);
    for root in result["created_roots"].as_array().unwrap() {
        assert_eq!(
            e.doc
                .layers
                .iter()
                .find(|l| l.id == root.as_str().unwrap())
                .unwrap()
                .kind,
            "paint"
        );
    }
    e.undo("artist").unwrap();
    assert_eq!(e.doc.layers.len(), 2);
    e.redo("artist").unwrap();
    assert_eq!(e.doc.layers.len(), 5);
    let roots = result["created_roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect::<Vec<_>>();
    let result = e
        .edit(
            "artist",
            &[json!({"op":"layer.duplicate","layers":roots})],
            Some(e.doc.revision),
            None,
            "Duplicate",
        )
        .unwrap();
    assert_eq!(result["created_roots"].as_array().unwrap().len(), 2);
    assert_eq!(e.undo.len(), 2);
    assert_eq!(e.doc.layers.len(), 7);
}
#[test]
fn typed_paste_obeys_reservations_and_rejects_stale_structural_edits() {
    let mut e = Engine::new();
    e.doc = Document::new(8, 8).unwrap();
    let id = e.doc.layers[0].id.clone();
    let snapshot = layer_clipboard::copy(&e.doc, &[id.clone()]).unwrap();
    let lease = e
        .reserve("artist", "Editing", vec![Scope::layer(&id)])
        .unwrap();
    assert!(e
        .paste_layers("human", &snapshot, &id, Some(0), None)
        .unwrap_err()
        .contains("Reserved"));
    assert_eq!(e.doc.layers.len(), 1);
    assert!(e.undo.is_empty());
    e.leases.retain(|l| l.id != lease.id);
    e.edit(
        "human",
        &[json!({"op":"layer.update","layer":id,"opacity":0.5})],
        Some(0),
        None,
        "Opacity",
    )
    .unwrap();
    assert!(e
        .paste_layers("human", &snapshot, &id, Some(0), None)
        .unwrap_err()
        .contains("Conflicting"));
    assert_eq!(e.doc.layers.len(), 1);
    assert_eq!(e.undo.len(), 1);
}
