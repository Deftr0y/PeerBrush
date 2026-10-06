use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    merge, psd, server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

fn engine(layers: Vec<Layer>) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new(8, 6).unwrap();
    e.doc.layers = layers;
    e
}
fn paint(name: &str, color: [u8; 4]) -> Layer {
    let mut l = Layer::new(name, "fill", 8, 6);
    l.color = color;
    l
}
fn view(e: &Engine) -> Vec<u8> {
    e.doc.preview(None, 8192, None, false).unwrap().2
}
fn near(a: &[u8], b: &[u8]) {
    assert_eq!(a.len(), b.len());
    for (a, b) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        assert!(
            (a[3] as i16 - b[3] as i16).abs() <= 1,
            "alpha: {a:?} vs {b:?}"
        );
        for c in 0..3 {
            let ap = a[c] as f32 * a[3] as f32 / 255.;
            let bp = b[c] as f32 * b[3] as f32 / 255.;
            assert!((ap - bp).abs() <= 2., "premultiplied: {a:?} vs {b:?}");
        }
    }
}

#[test]
fn merge_bakes_masks_effects_opacity_and_preserves_undo_redo_psd() {
    let top = paint("Top", [40, 130, 220, 220]);
    let bottom = paint("Bottom", [225, 75, 20, 255]);
    let ids = vec![top.id.clone(), bottom.id.clone()];
    let mut e = engine(vec![top, bottom]);
    e.edit(
        "human",
        &[
            json!({"op":"mask.add","layer":ids[0],"value":180}),
            json!({"op":"effect.add","layer":ids[0],"kind":"invert"}),
            json!({"op":"layer.update","layer":ids[0],"opacity":0.6}),
        ],
        None,
        None,
        "Prepare",
    )
    .unwrap();
    let before = e.doc.clone();
    let pixels = view(&e);
    let n = e.undo.len();
    let r = e
        .edit(
            "artist",
            &[json!({"op":"layer.merge","layers":ids,"name":"Baked"})],
            Some(e.doc.revision),
            None,
            "Merge",
        )
        .unwrap();
    assert_eq!(e.undo.len(), n + 1);
    assert_eq!(e.doc.layers.len(), 1);
    let merged = &e.doc.layers[0];
    assert_eq!(r["created"][0], merged.id);
    assert_eq!(merged.kind, "paint");
    assert!(merged.effects.is_empty() && merged.mask.is_none());
    assert_eq!(merged.opacity, 1.);
    assert_eq!(view(&e), pixels);
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(decoded.preview(None, 8192, None, false).unwrap().2, pixels);
    e.undo("artist").unwrap();
    assert_eq!(e.doc.layers.len(), before.layers.len());
    assert_eq!(
        e.doc.layers[0].effects[0].id,
        before.layers[0].effects[0].id
    );
    assert!(e.doc.layers[0].mask.is_some());
    assert_eq!(view(&e), pixels);
    e.redo("artist").unwrap();
    assert_eq!(e.doc.layers.len(), 1);
    assert_eq!(view(&e), pixels);
    assert_eq!(e.ai_change.as_ref().unwrap().tool, "history");
}

#[test]
fn single_paint_merges_down_in_its_folder_and_keeps_siblings() {
    let group = Layer::new("Folder", "group", 8, 6);
    let mut top = paint("Top", [255, 0, 0, 128]);
    top.parent = Some(group.id.clone());
    let mut down = paint("Below", [0, 100, 220, 255]);
    down.parent = Some(group.id.clone());
    let outside = paint("Outside", [25, 25, 25, 255]);
    let id = top.id.clone();
    let folder = group.id.clone();
    let outside_id = outside.id.clone();
    let mut e = engine(vec![top, outside, down, group]);
    let pixels = view(&e);
    e.edit(
        "human",
        &[json!({"op":"layer.merge","layer":id})],
        None,
        None,
        "Merge down",
    )
    .unwrap();
    assert_eq!(e.doc.layers.len(), 3);
    assert_eq!(e.doc.layers[0].parent.as_deref(), Some(folder.as_str()));
    assert!(e.doc.layers.iter().any(|l| l.id == outside_id));
    assert_eq!(view(&e), pixels);
}

#[test]
fn flatten_folder_bakes_nested_stacks_and_keeps_external_blend() {
    let mut outer = Layer::new("Folder", "group", 8, 6);
    outer.opacity = 0.7;
    outer.blend = "screen".into();
    let mut inner = Layer::new("Nested", "group", 8, 6);
    inner.parent = Some(outer.id.clone());
    let mut child = paint("Content", [80, 150, 210, 220]);
    child.parent = Some(inner.id.clone());
    let mut hidden = paint("Hidden", [255, 0, 0, 255]);
    hidden.parent = Some(inner.id.clone());
    hidden.visible = false;
    let backdrop = paint("Backdrop", [25, 40, 65, 255]);
    let folder = outer.id.clone();
    let nested = inner.id.clone();
    let mut e = engine(vec![child, hidden, inner, outer, backdrop]);
    e.edit(
        "human",
        &[
            json!({"op":"effect.add","layer":nested,"kind":"invert"}),
            json!({"op":"effect.add","layer":folder,"kind":"adjust","settings":{"brightness":0.1}}),
            json!({"op":"mask.add","layer":folder,"value":180}),
        ],
        None,
        None,
        "Stacks",
    )
    .unwrap();
    let pixels = view(&e);
    e.edit(
        "human",
        &[json!({"op":"layer.merge","layers":[folder]})],
        None,
        None,
        "Flatten",
    )
    .unwrap();
    assert_eq!(e.doc.layers.len(), 2);
    assert_eq!(e.doc.layers[0].blend, "screen");
    assert_eq!(e.doc.layers[0].name, "Folder");
    near(&pixels, &view(&e));
}

#[test]
fn merge_keeps_off_canvas_pixels_and_selection() {
    let mut top = Layer::new("Off canvas", "paint", 4, 4);
    top.x = -2;
    top.y = -1;
    top.pixels.set(0, 0, [100, 150, 220, 255]);
    top.pixels.set(3, 3, [240, 60, 20, 255]);
    let bottom = Layer::new("Empty", "paint", 8, 6);
    let id = top.id.clone();
    let mut e = engine(vec![top, bottom]);
    e.doc.selection = Some([1, 1, 4, 4]);
    e.edit(
        "human",
        &[json!({"op":"layer.merge","layer":id})],
        None,
        None,
        "Merge",
    )
    .unwrap();
    let l = &e.doc.layers[0];
    assert_eq!((l.x, l.y), (-2, -1));
    assert_eq!(l.pixels.get(0, 0), [100, 150, 220, 255]);
    assert_eq!(l.pixels.get(3, 3), [240, 60, 20, 255]);
    assert_eq!(e.doc.selection, Some([1, 1, 4, 4]));
}

#[test]
fn merge_rejects_locks_reservations_stale_revisions_and_cross_folders() {
    let top = paint("Top", [255, 0, 0, 255]);
    let down = paint("Below", [0, 0, 255, 255]);
    let id = top.id.clone();
    let mut e = engine(vec![top, down]);
    let before = view(&e);
    e.doc.layers[1].locked = true;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.merge","layer":id})],
            None,
            None,
            "Merge"
        )
        .is_err());
    assert!(e.undo.is_empty());
    assert_eq!(view(&e), before);
    e.doc.layers[1].locked = false;
    e.reserve(
        "other",
        "Painting",
        vec![Scope::layer(&e.doc.layers[1].id.clone())],
    )
    .unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.merge","layer":id})],
            None,
            None,
            "Merge"
        )
        .is_err());
    e.leases.clear();
    let shared: server::Shared = Arc::new(Mutex::new(e));
    assert!(merge::edit(&shared, "human", &[id.clone()], Some(100), None, None).is_err());
    let mut e = shared.lock().unwrap();
    let group = Layer::new("Folder", "group", 8, 6);
    e.doc.layers[1].parent = Some(group.id.clone());
    e.doc.layers.push(group);
    let ids = e.doc.layers[..2]
        .iter()
        .map(|l| l.id.clone())
        .collect::<Vec<_>>();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.merge","layers":ids})],
            None,
            None,
            "Merge"
        )
        .is_err());
    assert!(e.undo.is_empty());
}

#[test]
fn merge_enforces_bounds_and_rolls_back_entire_failed_batch() {
    let mut top = paint("Top", [255, 0, 0, 255]);
    top.x = -4100;
    let mut down = paint("Below", [0, 0, 255, 255]);
    down.x = 4100;
    let id = top.id.clone();
    let mut e = engine(vec![top, down]);
    let before = serde_json::to_value(&e.doc).unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.merge","layer":id})],
            None,
            None,
            "Merge"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert!(e.undo.is_empty());
    e.doc.layers[0].x = 0;
    e.doc.layers[1].x = 0;
    let before = serde_json::to_value(&e.doc).unwrap();
    assert!(e
        .edit(
            "human",
            &[
                json!({"op":"layer.merge","layer":id}),
                json!({"op":"move","layer":"missing","x":3})
            ],
            None,
            None,
            "Failed batch"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert!(e.undo.is_empty());
}

#[test]
fn merge_invalidates_parent_effect_cache_and_reads_earlier_batch_edits() {
    let group = Layer::new("Folder", "group", 8, 6);
    let mut top = paint("Top", [255, 0, 0, 255]);
    top.parent = Some(group.id.clone());
    let mut down = paint("Below", [0, 0, 255, 255]);
    down.parent = Some(group.id.clone());
    let id = top.id.clone();
    let group_id = group.id.clone();
    let mut e = engine(vec![top, down, group]);
    e.edit(
        "human",
        &[json!({"op":"effect.add","layer":group_id,"kind":"invert"})],
        None,
        None,
        "Effect",
    )
    .unwrap();
    let old = view(&e);
    assert_eq!(&old[..4], &[0, 255, 255, 255]);
    e.edit(
        "human",
        &[
            json!({"op":"layer.update","layer":id,"color":[0,255,0,255]}),
            json!({"op":"layer.merge","layer":id}),
        ],
        None,
        None,
        "Update and merge",
    )
    .unwrap();
    let pixels = view(&e);
    assert_eq!(&pixels[..4], &[255, 0, 255, 255]);
    let shared: server::Shared = Arc::new(Mutex::new(e));
    let result = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"peerbrush_observe","arguments":{"max_edge":32}}}),
    );
    assert_eq!(result["result"]["content"][1]["type"], "image");
}

#[test]
fn merge_refuses_gaps_and_backdrop_dependent_blends_without_changing_the_document() {
    let a = paint("A", [255, 0, 0, 128]);
    let b = paint("B", [0, 255, 0, 128]);
    let c = paint("C", [0, 0, 255, 255]);
    let ids = vec![a.id.clone(), c.id.clone()];
    let mut e = engine(vec![a, b, c]);
    let before = view(&e);
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.merge","layers":ids})],
            None,
            None,
            "Merge"
        )
        .unwrap_err()
        .contains("adjacent"));
    assert_eq!(view(&e), before);
    assert_eq!(e.doc.revision, 0);
    assert!(e.undo.is_empty());
    e.doc.layers[0].blend = "multiply".into();
    let ids = vec![e.doc.layers[0].id.clone(), e.doc.layers[1].id.clone()];
    let before = view(&e);
    assert!(e
        .edit(
            "human",
            &[json!({"op":"layer.merge","layers":ids})],
            None,
            None,
            "Merge"
        )
        .unwrap_err()
        .contains("folder"));
    assert_eq!(view(&e), before);
    assert!(e.undo.is_empty());
}
