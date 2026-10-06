use peerbrush::{
    engine::{Document, Engine},
    tree,
};
use serde_json::json;
#[test]
fn multi_layer_reparent_keeps_order_previews_and_undoes_atomically() {
    let mut e = Engine::new();
    e.doc = Document::new(32, 24).unwrap();
    e.edit(
        "human",
        &[
            json!({"op":"layer.add","kind":"group"}),
            json!({"op":"layer.add","kind":"paint"}),
            json!({"op":"layer.add","kind":"paint"}),
        ],
        None,
        None,
        "setup",
    )
    .unwrap();
    let a = e.doc.layers[0].id.clone();
    let b = e.doc.layers[1].id.clone();
    let folder = e.doc.layers[2].id.clone();
    let ids = vec![a.clone(), b.clone()];
    let before = e.doc.clone();
    e.undo.clear();
    let (preview, commands) = tree::reparent(&e.doc, &ids, Some(&folder), None).unwrap();
    assert_eq!(e.doc.layers[0].parent, None);
    e.edit("human", &commands, None, None, "Move layers")
        .unwrap();
    assert!(e
        .doc
        .layers
        .iter()
        .map(|l| (&l.id, &l.parent))
        .eq(preview.layers.iter().map(|l| (&l.id, &l.parent))));
    assert_eq!(e.undo.len(), 1);
    assert_eq!(
        e.doc
            .layers
            .iter()
            .filter(|l| l.parent.as_deref() == Some(&folder))
            .map(|l| &l.id)
            .collect::<Vec<_>>(),
        vec![&a, &b]
    );
    let (_, out) = tree::reparent(&e.doc, &ids, None, Some(&folder)).unwrap();
    e.edit("human", &out, None, None, "Move out").unwrap();
    assert!(e
        .doc
        .layers
        .iter()
        .find(|l| l.id == a)
        .unwrap()
        .parent
        .is_none());
    e.undo("human").unwrap();
    e.undo("human").unwrap();
    assert!(e
        .doc
        .layers
        .iter()
        .map(|l| (&l.id, &l.parent))
        .eq(before.layers.iter().map(|l| (&l.id, &l.parent))));
}
#[test]
fn folder_drops_reject_cycles_and_destination_locks_in_the_shared_engine() {
    let mut e = Engine::new();
    e.edit(
        "human",
        &[json!({"op":"layer.add","kind":"group"})],
        None,
        None,
        "group",
    )
    .unwrap();
    let parent = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"layer.add","kind":"group","parent":parent})],
        None,
        None,
        "child",
    )
    .unwrap();
    let child = e.doc.layers[0].id.clone();
    assert!(tree::reparent(&e.doc, &[parent.clone()], Some(&child), None).is_err());
    e.doc.layers[0].locked = true;
    let paint = e.doc.layers.last().unwrap().id.clone();
    assert!(tree::reparent(&e.doc, &[paint.clone()], Some(&child), None).is_err());
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.parent","layer":paint,"parent":child})],
            None,
            None,
            "drop"
        )
        .is_err());
}
