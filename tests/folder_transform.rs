use peerbrush::{
    effects::Effect,
    engine::{id, Document, Engine, Layer, Mask, MaskStep, Scope},
    psd,
    raster::Raster,
    server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
fn fixture(depth: u16, effects: bool) -> (Engine, String) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(40, 32, depth).unwrap();
    e.doc.layers.clear();
    let mut root = Layer::new("Transform folder", "group", 40, 32);
    root.pixels = Raster::new_depth(40, 32, depth);
    let root_id = root.id.clone();
    let mut paint = Raster::new_depth(40, 32, depth);
    paint.set16(7, 9, [0, 0, 0, 65535]);
    root.mask = Some(Mask {
        enabled: true,
        cache_key: id(),
        steps: vec![
            MaskStep {
                id: id(),
                kind: "fill".into(),
                enabled: true,
                value: 255.,
                pixels: Raster::new_depth(40, 32, depth),
                settings: Value::Null,
            },
            MaskStep {
                id: id(),
                kind: "paint".into(),
                enabled: true,
                value: 0.,
                pixels: paint,
                settings: Value::Null,
            },
        ],
    });
    if effects {
        root.effects.push(Effect {
            id: id(),
            kind: "invert".into(),
            enabled: true,
            settings: json!({}),
        });
    }
    let mut nested = Layer::new("Nested folder", "group", 40, 32);
    nested.pixels = Raster::new_depth(40, 32, depth);
    nested.parent = Some(root_id.clone());
    let nested_id = nested.id.clone();
    e.doc.layers.push(root);
    e.doc.layers.push(nested);
    for (n, parent) in [root_id.clone(), nested_id].iter().enumerate() {
        let mut child = Layer::new("Child", "paint", 40, 32);
        child.parent = Some(parent.clone());
        child.pixels = Raster::new_depth(40, 32, depth);
        child.visible = n == 0;
        child.pixels.set16(30, 30, [10001, 20003, 30007, 0]);
        for y in 8 + n as i32 * 3..12 + n as i32 * 3 {
            for x in 5 + n as i32 * 7..10 + n as i32 * 7 {
                child
                    .pixels
                    .set16(x, y, [12345 + n as u16, 23457, 34569, 65535]);
            }
        }
        e.doc.layers.push(child);
    }
    (e, root_id)
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], None, None, "Folder transform")
        .unwrap();
}
#[test]
fn folder_rotation_keeps_native_pixels_hidden_children_masks_sources_and_one_undo() {
    for depth in [8, 16] {
        let (mut e, root) = fixture(depth, false);
        let before = e.doc.clone();
        let snapshot = serde_json::to_vec(&before).unwrap();
        let command = json!({"op":"transform","layer":root,"angle":90,"pivot":[16,12],"selection_only":false});
        let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
        assert_eq!(e.doc.revision, 0);
        edit(&mut e, command);
        assert_eq!(preview.export_png().unwrap(), e.doc.export_png().unwrap());
        for (old, new) in before.layers.iter().zip(&e.doc.layers) {
            assert_eq!(new.kind, old.kind);
            assert_eq!(new.parent, old.parent);
            assert_eq!(new.visible, old.visible);
            assert_eq!(new.pixels.depth, depth);
            if old.kind == "paint" {
                for y in 0..32 {
                    for x in 0..40 {
                        let p = old.pixels.get16(x, y);
                        if p != [0; 4] {
                            assert_eq!(new.pixels.get16(27 - y - new.x, x - 4 - new.y), p);
                        }
                    }
                }
            }
        }
        let group = &e.doc.layers[0];
        assert_eq!(group.kind, "group");
        assert_eq!(group.mask_value(18 - group.x, 3 - group.y), 0.);
        let saved = psd::encode(&e.doc).unwrap();
        let start = saved.windows(4).position(|p| p == b"PBR1").unwrap() + 4;
        let mut source = Vec::new();
        std::io::Read::read_to_end(
            &mut flate2::read::ZlibDecoder::new(&saved[start..]),
            &mut source,
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&source).unwrap()["format"],
            7
        );
        let restored = psd::decode(&saved).unwrap();
        assert!(!restored.read_only);
        assert_eq!(restored.export_png().unwrap(), e.doc.export_png().unwrap());
        let mut standard = saved;
        let at = standard.windows(4).position(|p| p == b"PBR1").unwrap();
        standard[at + 3] = b'X';
        let standard = psd::decode(&standard).unwrap();
        assert!(!standard.read_only);
        assert_eq!(
            standard.preview(None, 40, None, false).unwrap().2,
            e.doc.preview(None, 40, None, false).unwrap().2
        );
        e.undo("human").unwrap();
        assert_eq!(
            serde_json::to_vec(&e.doc.layers).unwrap(),
            serde_json::to_vec(&before.layers).unwrap()
        );
        assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
        e.redo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
        assert_eq!(serde_json::to_vec(&before).unwrap(), snapshot);
    }
}
#[test]
fn moved_effected_folder_uses_world_colors_and_local_mask_and_standard_psd_channels() {
    for depth in [8, 16] {
        let (mut e, root) = fixture(depth, true);
        let before = e.doc.preview(None, 40, None, false).unwrap().2;
        edit(
            &mut e,
            json!({"op":"move","layer":root,"dx":11,"dy":6,"selection_only":false}),
        );
        let after = e.doc.preview(None, 40, None, false).unwrap().2;
        assert_eq!(&after[(15 * 40 + 18) * 4..(15 * 40 + 18) * 4 + 4], &[0; 4]);
        for y in 8..12 {
            for x in 5..10 {
                assert_eq!(
                    &after[((y + 6) * 40 + x + 11) * 4..((y + 6) * 40 + x + 11) * 4 + 4],
                    &before[(y * 40 + x) * 4..(y * 40 + x) * 4 + 4]
                );
            }
        }
        let saved = psd::encode(&e.doc).unwrap();
        let restored = psd::decode(&saved).unwrap();
        assert_eq!(restored.preview(None, 40, None, false).unwrap().2, after);
        let mut standard = saved;
        let at = standard.windows(4).position(|p| p == b"PBR1").unwrap();
        standard[at + 3] = b'X';
        let standard = psd::decode(&standard).unwrap();
        assert!(!standard.read_only);
        assert_eq!(standard.preview(None, 40, None, false).unwrap().2, after);
    }
}
#[test]
fn folder_edits_are_atomic_for_locks_reservations_late_failures_and_selection_scope() {
    let (mut e, root) = fixture(16, false);
    let command =
        json!({"op":"transform","layer":root,"angle":20,"scale_x":1.2,"selection_only":false});
    e.doc.layers[3].locked = true;
    let original = serde_json::to_vec(&e.doc).unwrap();
    assert!(e
        .edit("human", &[command.clone()], None, None, "Locked tree")
        .is_err());
    assert_eq!(serde_json::to_vec(&e.doc).unwrap(), original);
    e.doc.layers[3].locked = false;
    let child = e.doc.layers[3].id.clone();
    e.reserve(
        "agent",
        "Child work",
        vec![Scope {
            target: Some(child),
            rect: Some([20, 20, 25, 25]),
        }],
    )
    .unwrap();
    e.doc.selection = Some([0, 0, 2, 2]);
    assert!(e
        .edit("human", &[command.clone()], None, None, "Reserved tree")
        .is_err());
    e.leases.clear();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"move","layer":root,"dx":1})],
            None,
            None,
            "Ambiguous selection"
        )
        .is_err());
    let original = serde_json::to_vec(&e.doc).unwrap();
    e.doc.layers[3].x = 100000;
    let invalid = serde_json::to_vec(&e.doc).unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"move","layer":root,"dx":5,"selection_only":false})],
            None,
            None,
            "Late failure"
        )
        .is_err());
    assert_eq!(serde_json::to_vec(&e.doc).unwrap(), invalid);
    e.doc.layers[3].x = 0;
    assert_eq!(serde_json::to_vec(&e.doc).unwrap(), original);
    edit(&mut e, command);
    assert_eq!(e.doc.selection, Some([0, 0, 2, 2]));
}
#[test]
fn protocol_folder_move_returns_rendered_image_coordinates_and_shared_undo() {
    let (e, root) = fixture(8, true);
    let shared = Arc::new(Mutex::new(e));
    let before = shared.lock().unwrap().doc.export_png().unwrap();
    let result=server::dispatch(&shared,"edit",&json!({"actor":"artist","commands":[{"op":"move","layer":root,"dx":5,"dy":4,"selection_only":false}],"max_edge":40,"feedback":"always"})).unwrap();
    assert_eq!(result["revision"], 1);
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 40, 32]));
    server::dispatch(
        &shared,
        "history",
        &json!({"actor":"artist","action":"undo","expected_revision":1}),
    )
    .unwrap();
    assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), before);
}

#[test]
fn default_folder_pivot_uses_descendants_and_adjustment_masks_retain_native_frames() {
    let (mut e, root) = fixture(16, true);
    let mut adjustment = Layer::new("Native adjustment", "adjustment", 40, 32);
    adjustment.parent = Some(root.clone());
    adjustment.pixels = Raster::new_depth(40, 32, 16);
    adjustment.effects.push(Effect {
        id: id(),
        kind: "levels".into(),
        enabled: true,
        settings: json!({"gamma":1.1}),
    });
    adjustment.mask = e.doc.layers[0].mask.clone();
    e.doc.layers.insert(2, adjustment);
    let bounds = peerbrush::transform::tree_bounds(&e.doc, &[root.clone()]).unwrap();
    let pivot = [
        (bounds[0] as f64 + bounds[2] as f64) / 2.,
        (bounds[1] as f64 + bounds[3] as f64) / 2.,
    ];
    let implicit =
        json!({"op":"transform","layer":root,"angle":90,"scale_x":1.2,"selection_only":false});
    let mut explicit = implicit.clone();
    explicit["pivot"] = json!(pivot);
    let expected = Engine::preview_edits(e.doc.clone(), &[explicit]).unwrap();
    edit(&mut e, implicit);
    assert_eq!(e.doc.export_png().unwrap(), expected.export_png().unwrap());
    psd::validate(&e.doc).unwrap();
    let encoded = psd::encode(&e.doc).unwrap();
    let restored = psd::decode(&encoded).unwrap();
    assert!(!restored.read_only);
    assert_eq!(
        restored.export_png().unwrap(),
        expected.export_png().unwrap()
    );
    let adjustment = e
        .doc
        .layers
        .iter()
        .find(|l| l.kind == "adjustment")
        .unwrap();
    assert_ne!((adjustment.x, adjustment.y), (0, 0));
    assert_eq!(adjustment.mask.as_ref().unwrap().steps[1].pixels.depth, 16);
}

#[test]
fn smart_mask_after_folder_move_samples_world_point_in_the_retained_local_frame() {
    for depth in [8, 16] {
        let (mut e, root) = fixture(depth, true);
        edit(
            &mut e,
            json!({"op":"move","layer":root,"dx":11,"dy":6,"selection_only":false}),
        );
        edit(
            &mut e,
            json!({"op":"mask.from_color","layer":root,"point":[16,14],"tolerance":0,"mode":"replace"}),
        );
        let group = e.doc.layers.iter().find(|l| l.id == root).unwrap();
        assert_eq!(group.mask_value(16 - group.x, 14 - group.y), 1.);
        assert_eq!(group.mask_value(35 - group.x, 25 - group.y), 0.);
        psd::validate(&e.doc).unwrap();
        assert_eq!(
            psd::decode(&psd::encode(&e.doc).unwrap())
                .unwrap()
                .export_png()
                .unwrap(),
            e.doc.export_png().unwrap()
        );
    }
}
