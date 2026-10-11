use peerbrush::{
    engine::{Document, Engine, Scope},
    psd, server, transform,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(64, 64, depth).unwrap();
    for y in 16..36 {
        for x in 12..32 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [12345 + x as u16 * 19, 23457 + y as u16 * 31, 45679, 65535],
            );
        }
    }
    e
}
#[test]
fn snapped_move_rotation_and_scale_match_explicit_commands_at_both_depths_and_reopen() {
    for depth in [8, 16] {
        for (command, mut explicit) in [
            (
                json!({"op":"move","dx":13.4,"dy":-3.1,"snap":true}),
                json!({"op":"move","dx":16.,"dy":0.}),
            ),
            (
                json!({"op":"transform","angle":22.4,"snap":true}),
                json!({"op":"transform","angle":15.}),
            ),
            (
                json!({"op":"transform","scale_x":1.46,"scale_y":0.84,"snap":true}),
                json!({"op":"transform","scale_x":1.5,"scale_y":0.8}),
            ),
        ] {
            let mut e = fixture(depth);
            let before = e.doc.clone();
            let layer = e.doc.layers[0].id.clone();
            let mut c = command.clone();
            c["layer"] = json!(layer);
            explicit["layer"] = json!(layer);
            let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
            let reference = Engine::preview_edits(before.clone(), &[explicit]).unwrap();
            assert_eq!(
                preview.export_png().unwrap(),
                reference.export_png().unwrap()
            );
            assert_eq!(e.doc.revision, before.revision);
            assert!(e.undo.is_empty());
            e.edit(
                "human",
                &[c],
                Some(before.revision),
                None,
                "Snapped transform",
            )
            .unwrap();
            assert_eq!(e.undo.len(), 1);
            assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
            assert_eq!(e.doc.bit_depth, depth);
            let saved = psd::encode(&e.doc).unwrap();
            let reopened = psd::decode(&saved).unwrap();
            assert_eq!(
                reopened.export_png().unwrap(),
                preview.export_png().unwrap()
            );
            e.undo("human").unwrap();
            assert_eq!(
                e.doc.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
            e.redo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
        }
    }
}
#[test]
fn snapping_rejects_invalid_values_atomically_and_respects_selection_locks_and_reservations() {
    for sign in [-1., 1.] {
        let boundary = transform::resolve_command(&json!({"op":"transform","angle":sign*100000.,"scale_x":0.05,"scale_y":20.,"snap":true})).unwrap();
        assert_eq!(boundary["angle"], sign * 99990.);
        assert_eq!(boundary["scale_x"], 0.1);
        assert_eq!(boundary["scale_y"], 20.);
    }
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let source = e.doc.layers[0].pixels.rgba16();
    let rev = e.doc.revision;
    for fields in [
        json!({"snap":"true"}),
        json!({"snap":true,"dx":"bad"}),
        json!({"snap":true,"dx":100001.}),
        json!({"op":"transform","snap":true,"scale_x":0.}),
        json!({"op":"transform","snap":true,"scale_x":20.01}),
    ] {
        let mut c = json!({"op":"move","layer":layer});
        for (k, v) in fields.as_object().unwrap() {
            c[k] = v.clone();
        }
        assert!(Engine::preview_edits(e.doc.clone(), &[c.clone()]).is_err());
        assert!(e.edit("human", &[c], None, None, "Invalid").is_err());
        assert_eq!(e.doc.revision, rev);
        assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
        assert!(e.undo.is_empty());
    }
    let command = json!({"op":"move","layer":layer,"dx":13.4,"snap":true});
    e.doc.layers[0].locked = true;
    assert!(e
        .edit("human", &[command.clone()], None, None, "Locked")
        .is_err());
    e.doc.layers[0].locked = false;
    e.reserve("other", "Protected layer", vec![Scope::layer(&layer)])
        .unwrap();
    assert!(e
        .edit("human", &[command.clone()], None, None, "Reserved")
        .is_err());
    e.leases.clear();
    e.edit(
        "human",
        &[json!({"op":"selection","rect":[12,16,22,26],"feather":1.})],
        None,
        None,
        "Selection",
    )
    .unwrap();
    e.undo.clear();
    e.reserve(
        "other",
        "Protected selection",
        vec![Scope::layer("@selection")],
    )
    .unwrap();
    let before = e.doc.clone();
    assert!(e
        .edit(
            "human",
            &[command.clone()],
            None,
            None,
            "Reserved selection"
        )
        .is_err());
    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
    e.leases.clear();
    let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
    e.edit(
        "human",
        &[command],
        Some(before.revision),
        None,
        "Selected snap",
    )
    .unwrap();
    assert_eq!(e.undo.len(), 1);
    assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
    e.undo("human").unwrap();
    assert_eq!(e.doc.selection, before.selection);
    assert_eq!(
        e.doc.layers[0].pixels.rgba16(),
        before.layers[0].pixels.rgba16()
    );
}
#[test]
fn protocol_snapping_returns_actual_image_content_and_document_coordinates() {
    let e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let rev = e.doc.revision;
    let shared = Arc::new(Mutex::new(e));
    let result:Value=server::dispatch(&shared,"edit",&json!({"actor":"snap-agent","expected_revision":rev,"commands":[{"op":"move","layer":layer,"dx":13.4,"dy":-3.1,"snap":true}],"max_edge":64})).unwrap();
    assert!(!result["images"][0]["data"].as_str().unwrap().is_empty());
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 64, 64]));
    let e = shared.lock().unwrap();
    assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (16, 0));
    assert_eq!(e.doc.bit_depth, 16);
    assert_eq!(e.undo.len(), 1);
}
