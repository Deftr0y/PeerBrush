use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    layer_clipboard, psd,
    raster::{blend, Pixel, Raster},
};
use serde_json::{json, Value};
use std::io::Read;

fn paint(name: &str, color: Pixel) -> Layer {
    let mut layer = Layer::new(name, "paint", 4, 4);
    layer.pixels = Raster::from_rgba(4, 4, &[color; 16].concat()).unwrap();
    layer
}
fn fixture() -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new(4, 4).unwrap();
    e
}
fn pixel(doc: &Document, x: i32, y: i32) -> Pixel {
    doc.preview(Some([x, y, x + 1, y + 1]), 1, None, false)
        .unwrap()
        .2[..4]
        .try_into()
        .unwrap()
}
fn add(e: &mut Engine, target: Value) -> String {
    let mut command = json!({"op":"adjustment.add","kind":"invert","name":"Invert"});
    if let Some(target) = target.as_str() {
        command["layer"] = json!(target);
    } else {
        command["layers"] = target;
    }
    e.edit("human", &[command], None, None, "Add adjustment")
        .unwrap();
    e.doc
        .layers
        .iter()
        .find(|l| l.kind == "adjustment")
        .unwrap()
        .id
        .clone()
}

#[test]
fn clipped_adjustment_isolates_base_and_preserves_partial_alpha_opacity_and_both_masks() {
    let mut e = fixture();
    let mut base = paint("Partial base", [200, 40, 20, 128]);
    base.pixels = Raster::from_rgba(2, 2, &[[200, 40, 20, 128]; 4].concat()).unwrap();
    base.x = 2;
    base.y = 2;
    base.opacity = 0.5;
    let base_id = base.id.clone();
    let bg = paint("Backdrop", [10, 90, 170, 255]);
    e.doc.layers = vec![base, bg];
    e.edit(
        "human",
        &[json!({"op":"mask.add","layer":base_id,"value":128})],
        None,
        None,
        "Base mask",
    )
    .unwrap();
    let adjustment = add(&mut e, json!(base_id));
    e.edit(
        "human",
        &[
            json!({"op":"layer.update","layer":adjustment,"opacity":0.5}),
            json!({"op":"mask.add","layer":adjustment,"value":128}),
        ],
        None,
        None,
        "Adjustment amount",
    )
    .unwrap();
    let t = 0.5 * 128.0 / 255.0;
    let base_color = [200, 40, 20];
    let inverted = [55, 215, 235];
    let changed: Pixel = std::array::from_fn(|c| {
        if c == 3 {
            128
        } else {
            (base_color[c] as f32 * (1.0 - t) + inverted[c] as f32 * t).round() as u8
        }
    });
    assert_eq!(
        pixel(&e.doc, 2, 2),
        blend([10, 90, 170, 255], changed, 0.5 * 128.0 / 255.0, "normal")
    );
    assert_eq!(pixel(&e.doc, 0, 0), [10, 90, 170, 255]);
    let adj = e.doc.layers.iter().find(|l| l.id == adjustment).unwrap();
    assert_eq!(adj.clip_to.as_deref(), Some(base_id.as_str()));
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.id == base_id)
            .unwrap()
            .pixels
            .get(0, 0),
        [200, 40, 20, 128]
    );
    // Isolating the adjustment exposes its derived base, preserving the base's alpha.
    let isolated = e
        .doc
        .preview(Some([2, 2, 3, 3]), 1, Some(&adjustment), false)
        .unwrap()
        .2;
    assert_eq!(isolated, [55, 215, 235, 128]);
}
#[test]
fn standalone_adjustment_changes_the_composited_backdrop_once_without_raising_alpha() {
    let mut e = fixture();
    e.doc.layers = vec![
        paint("Top", [200, 20, 80, 100]),
        paint("Bottom", [20, 180, 60, 80]),
    ];
    let before = pixel(&e.doc, 1, 1);
    let id = add(&mut e, json!([]));
    assert_eq!(
        pixel(&e.doc, 1, 1),
        [255 - before[0], 255 - before[1], 255 - before[2], before[3]]
    );
    e.edit(
        "human",
        &[json!({"op":"layer.update","layer":id,"opacity":0.5})],
        None,
        None,
        "Half adjustment",
    )
    .unwrap();
    let after = pixel(&e.doc, 1, 1);
    assert_eq!(after[3], before[3]);
    for c in 0..3 {
        assert_eq!(after[c], 128);
    }
}
#[test]
fn folder_adjustment_stays_inside_the_folder_and_parent_mask_is_applied_once() {
    let mut e = fixture();
    let mut group = Layer::new("Subject", "group", 4, 4);
    group.opacity = 0.5;
    let group_id = group.id.clone();
    let mut top = paint("Top", [200, 40, 20, 100]);
    top.parent = Some(group_id.clone());
    let mut bottom = paint("Bottom", [20, 180, 60, 80]);
    bottom.parent = Some(group_id.clone());
    let backdrop = paint("Backdrop", [25, 45, 220, 255]);
    let original = blend([20, 180, 60, 80], [200, 40, 20, 100], 1.0, "normal");
    e.doc.layers = vec![group, top, bottom, backdrop];
    e.edit(
        "human",
        &[json!({"op":"mask.add","layer":group_id,"value":128})],
        None,
        None,
        "Parent mask",
    )
    .unwrap();
    let adjustment = add(&mut e, json!(group_id));
    let changed = [
        255 - original[0],
        255 - original[1],
        255 - original[2],
        original[3],
    ];
    assert_eq!(
        pixel(&e.doc, 1, 1),
        blend([25, 45, 220, 255], changed, 0.5 * 128.0 / 255.0, "normal")
    );
    let layer = e.doc.layers.iter().find(|l| l.id == adjustment).unwrap();
    assert_eq!(layer.parent.as_deref(), Some(group_id.as_str()));
    assert!(layer.clip_to.is_none());
}
#[test]
fn multi_layer_adjustment_groups_selected_roots_and_is_one_undoable_edit() {
    let mut e = fixture();
    let a = paint("A", [200, 20, 80, 100]);
    let b = paint("B", [20, 180, 60, 80]);
    let bg = paint("Backdrop", [40, 50, 200, 255]);
    let selected = vec![a.id.clone(), b.id.clone()];
    let bg_id = bg.id.clone();
    e.doc.layers = vec![a, b, bg];
    let original = e.doc.clone();
    let content = blend([20, 180, 60, 80], [200, 20, 80, 100], 1.0, "normal");
    let adjustment = add(&mut e, json!(selected));
    let group = e.doc.layers.iter().find(|l| l.kind == "group").unwrap();
    let group_id = group.id.clone();
    assert!(selected.iter().all(|id| e
        .doc
        .layers
        .iter()
        .find(|l| l.id == *id)
        .unwrap()
        .parent
        .as_deref()
        == Some(group_id.as_str())));
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.id == adjustment)
            .unwrap()
            .parent
            .as_deref(),
        Some(group_id.as_str())
    );
    assert!(e
        .doc
        .layers
        .iter()
        .find(|l| l.id == bg_id)
        .unwrap()
        .parent
        .is_none());
    let inverted = [
        255 - content[0],
        255 - content[1],
        255 - content[2],
        content[3],
    ];
    assert_eq!(
        pixel(&e.doc, 0, 0),
        blend([40, 50, 200, 255], inverted, 1.0, "normal")
    );
    assert_eq!(e.undo.len(), 1);
    e.undo("human").unwrap();
    assert_eq!(
        e.doc
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>(),
        original
            .layers
            .iter()
            .map(|l| l.id.clone())
            .collect::<Vec<_>>()
    );
    e.redo("human").unwrap();
    assert!(e.doc.layers.iter().any(|l| l.id == adjustment));
}
#[test]
fn creating_adjustment_uses_exact_selection_as_editable_mask_and_keeps_selection() {
    let mut e = fixture();
    let base = paint("Subject", [200, 40, 20, 255]);
    let base_id = base.id.clone();
    e.doc.layers = vec![base];
    let polygon = vec![[2.0, 0.0], [4.0, 2.0], [2.0, 4.0], [0.0, 2.0]];
    e.doc.selection = Some([0, 0, 4, 4]);
    e.doc.selection_polygon = Some(polygon.clone());
    let adjustment = add(&mut e, json!(base_id));
    assert_eq!(e.doc.selection, Some([0, 0, 4, 4]));
    assert_eq!(e.doc.selection_polygon, Some(polygon.clone()));
    let layer = e.doc.layers.iter().find(|l| l.id == adjustment).unwrap();
    let mask = layer.mask.as_ref().unwrap();
    assert_eq!(
        mask.steps
            .iter()
            .map(|s| s.kind.as_str())
            .collect::<Vec<_>>(),
        ["fill", "paint"]
    );
    for y in 0..4 {
        for x in 0..4 {
            let inside = peerbrush::selection::contains(&polygon, x as f32 + 0.5, y as f32 + 0.5);
            assert_eq!(
                pixel(&e.doc, x, y),
                if inside {
                    [55, 215, 235, 255]
                } else {
                    [200, 40, 20, 255]
                }
            );
            assert_eq!(layer.mask_value_raw(x, y), if inside { 1.0 } else { 0.0 });
        }
    }
    assert_eq!(e.undo.len(), 1);
    e.edit(
        "human",
        &[json!({"op":"mask.toggle","layer":adjustment,"enabled":false})],
        None,
        None,
        "Reveal adjustment",
    )
    .unwrap();
    assert_eq!(pixel(&e.doc, 0, 0), [55, 215, 235, 255]);
}
#[test]
fn selection_outside_canvas_produces_empty_mask_without_expanding_adjustment() {
    let mut e = fixture();
    let id = e.doc.layers[0].id.clone();
    e.doc.selection = Some([10, 10, 20, 20]);
    let adjustment = add(&mut e, json!(id));
    let layer = e.doc.layers.iter().find(|l| l.id == adjustment).unwrap();
    assert_eq!(
        (layer.x, layer.y, layer.pixels.width, layer.pixels.height),
        (0, 0, 4, 4)
    );
    assert_eq!(layer.mask_value_raw(1, 1), 0.0);
    assert_eq!(layer.mask.as_ref().unwrap().steps[1].pixels.bytes(), 0);
    assert_eq!(e.doc.selection, Some([10, 10, 20, 20]));
}
#[test]
fn locked_targets_and_ai_reservations_reject_adjustment_creation_atomically() {
    for parent_lock in [false, true] {
        let mut e = fixture();
        let mut group = Layer::new("Locked folder", "group", 4, 4);
        let group_id = group.id.clone();
        let mut child = paint("Child", [120, 60, 20, 255]);
        let child_id = child.id.clone();
        child.parent = Some(group_id);
        group.locked = parent_lock;
        child.locked = !parent_lock;
        e.doc.layers = vec![group, child];
        let before = serde_json::to_value(&e.doc).unwrap();
        assert!(e
            .edit(
                "human",
                &[json!({"op":"adjustment.add","kind":"invert","layer":child_id})],
                None,
                None,
                "Adjust locked"
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert!(e.undo.is_empty());
    }
    let mut e = fixture();
    let id = e.doc.layers[0].id.clone();
    e.reserve("artist", "Working on subject", vec![Scope::layer(&id)])
        .unwrap();
    let before = serde_json::to_value(&e.doc).unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"adjustment.add","kind":"invert","layers":[id]})],
            None,
            None,
            "Adjust reserved"
        )
        .unwrap_err()
        .contains("Reserved"));
    assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
    assert!(e.undo.is_empty());
}
#[test]
fn copied_clipping_units_remap_the_adjustment_base_and_keep_sources_editable() {
    let mut e = fixture();
    let base = paint("Base", [200, 40, 20, 128]);
    let base_id = base.id.clone();
    e.doc.layers = vec![base];
    let adjustment = add(&mut e, json!(base_id));
    let snapshot = layer_clipboard::copy(&e.doc, &[adjustment.clone(), base_id.clone()]).unwrap();
    let expected = e.doc.preview(None, 4, Some(&adjustment), false).unwrap().2;
    let result = e
        .paste_layers("human", &snapshot, &base_id, Some(e.doc.revision), None)
        .unwrap();
    let roots = result["created_roots"].as_array().unwrap();
    assert_eq!(roots.len(), 2);
    let new_adj = roots[0].as_str().unwrap();
    let new_base = roots[1].as_str().unwrap();
    let layer = e.doc.layers.iter().find(|l| l.id == new_adj).unwrap();
    assert_eq!(layer.clip_to.as_deref(), Some(new_base));
    assert_ne!(new_base, base_id);
    assert_ne!(new_adj, adjustment);
    assert_eq!(layer.kind, "adjustment");
    assert_eq!(
        e.doc.preview(None, 4, Some(new_adj), false).unwrap().2,
        expected
    );
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.id == new_base)
            .unwrap()
            .pixels
            .get(0, 0),
        [200, 40, 20, 128]
    );
}
#[test]
fn psd_v4_restores_adjustment_and_clipping_sources_and_standard_layers_match_the_composite() {
    let mut e = fixture();
    let base = paint("Source", [200, 40, 20, 128]);
    let id = base.id.clone();
    e.doc.layers = vec![base, paint("Backdrop", [20, 80, 200, 255])];
    let adjustment = add(&mut e, json!(id));
    let expected = e.doc.preview(None, 4, None, false).unwrap().2;
    let encoded = psd::encode(&e.doc).unwrap();
    let at = encoded.windows(4).position(|p| p == b"PBR1").unwrap();
    let mut embedded = vec![];
    flate2::read::ZlibDecoder::new(&encoded[at + 4..])
        .read_to_end(&mut embedded)
        .unwrap();
    let private: Value = serde_json::from_slice(&embedded).unwrap();
    assert_eq!(private["format"], 4);
    let reopened = psd::decode(&encoded).unwrap();
    assert!(!reopened.read_only);
    assert_eq!(
        reopened
            .layers
            .iter()
            .find(|l| l.id == adjustment)
            .unwrap()
            .clip_to
            .as_deref(),
        Some(id.as_str())
    );
    assert_eq!(
        reopened
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .pixels
            .get(0, 0),
        [200, 40, 20, 128]
    );
    assert_eq!(reopened.preview(None, 4, None, false).unwrap().2, expected);
    let mode = u32::from_be_bytes(encoded[26..30].try_into().unwrap()) as usize;
    let resource_at = 30 + mode;
    let resources =
        u32::from_be_bytes(encoded[resource_at..resource_at + 4].try_into().unwrap()) as usize;
    let mut standard = encoded[..resource_at].to_vec();
    standard.extend_from_slice(&0u32.to_be_bytes());
    standard.extend_from_slice(&encoded[resource_at + 4 + resources..]);
    let outside = psd::decode(&standard).unwrap();
    assert_eq!(outside.layers.len(), 1);
    assert_eq!(outside.layers[0].kind, "paint");
    assert_eq!(outside.preview(None, 4, None, false).unwrap().2, expected);
}
