use peerbrush::{
    engine::{Document, Engine},
    tree,
};
use serde_json::json;
#[test]
fn multi_layer_reparent_keeps_order_previews_and_undoes_atomically() {
    let mut e = Engine::new();
    e.resident_state_mut().expect("resident fixture").document = Document::new(32, 24).unwrap();
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
    let a = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
    let b = e.resident_state().expect("resident fixture").document.layers[1].id.clone();
    let folder = e.resident_state().expect("resident fixture").document.layers[2].id.clone();
    let ids = vec![a.clone(), b.clone()];
    let before = e.resident_state().expect("resident fixture").document.clone();
    e.resident_state_mut().expect("resident fixture").undo.clear();
    let (preview, commands) = tree::reparent(&e.resident_state().expect("resident fixture").document, &ids, Some(&folder), None).unwrap();
    assert_eq!(e.resident_state().expect("resident fixture").document.layers[0].parent, None);
    e.edit("human", &commands, None, None, "Move layers")
        .unwrap();
    assert!(e
        .resident_state().expect("resident fixture").document
        .layers
        .iter()
        .map(|l| (&l.id, &l.parent))
        .eq(preview.layers.iter().map(|l| (&l.id, &l.parent))));
    assert_eq!(e.resident_state().expect("resident fixture").undo.len(), 1);
    assert_eq!(
        e.resident_state().expect("resident fixture").document
            .layers
            .iter()
            .filter(|l| l.parent.as_deref() == Some(&folder))
            .map(|l| &l.id)
            .collect::<Vec<_>>(),
        vec![&a, &b]
    );
    let (_, out) = tree::reparent(&e.resident_state().expect("resident fixture").document, &ids, None, Some(&folder)).unwrap();
    e.edit("human", &out, None, None, "Move out").unwrap();
    assert!(e
        .resident_state().expect("resident fixture").document
        .layers
        .iter()
        .find(|l| l.id == a)
        .unwrap()
        .parent
        .is_none());
    e.undo("human").unwrap();
    e.undo("human").unwrap();
    assert!(e
        .resident_state().expect("resident fixture").document
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
    let parent = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"layer.add","kind":"group","parent":parent})],
        None,
        None,
        "child",
    )
    .unwrap();
    let child = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
    assert!(tree::reparent(&e.resident_state().expect("resident fixture").document, &[parent.clone()], Some(&child), None).is_err());
    e.resident_state_mut().expect("resident fixture").document.layers[0].locked = true;
    let paint = e.resident_state().expect("resident fixture").document.layers.last().unwrap().id.clone();
    assert!(tree::reparent(&e.resident_state().expect("resident fixture").document, &[paint.clone()], Some(&child), None).is_err());
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
