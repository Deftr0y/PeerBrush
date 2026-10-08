use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{
    engine::{Document, Engine},
    psd, raster, selection, server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
fn mask() -> String {
    STANDARD.encode(raster::png(2, 1, &[0, 0, 0, 17, 255, 255, 255, 149]).unwrap())
}
fn apply(e: &mut Engine, c: Value) {
    e.edit(
        "human",
        &[c],
        Some(e.doc.revision),
        None,
        "Import confidence",
    )
    .unwrap();
}
#[test]
fn confidence_coordinates_soft_edges_native_sources_and_psd_roundtrip() {
    let mut e = Engine::new();
    e.doc = Document::new_depth(12, 8, 16).unwrap();
    e.doc.layers[0].pixels.set16(5, 3, [123, 271, 539, 65535]);
    let source = e.doc.export_png().unwrap();
    apply(
        &mut e,
        json!({"op":"selection.import","rect":[4,2,8,4],"png":mask()}),
    );
    let m = selection::current(&e.doc).unwrap();
    assert_eq!(
        (4..8).map(|x| m.mask.get(x, 2)[0]).collect::<Vec<_>>(),
        vec![0, 64, 191, 255]
    );
    assert_eq!(m.value(8, 2), 0.);
    assert_eq!(m.value(7, 4), 0.);
    assert_eq!(e.doc.export_png().unwrap(), source);
    let copy = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(copy.bit_depth, 16);
    assert_eq!(copy.export_png().unwrap(), source);
    assert_eq!(selection::current(&copy).unwrap().mask.get(6, 2)[0], 191);
    e.undo("human").unwrap();
    assert!(e.doc.selection.is_none());
    assert_eq!(e.doc.export_png().unwrap(), source);
}
#[test]
fn alpha_modes_and_reselect_keep_confidence() {
    let mut e = Engine::new();
    e.doc = Document::new(12, 8).unwrap();
    apply(
        &mut e,
        json!({"op":"selection.import","rect":[4,2,6,3],"png":mask(),"channel":"alpha"}),
    );
    assert_eq!(selection::current(&e.doc).unwrap().mask.get(4, 2)[0], 17);
    apply(
        &mut e,
        json!({"op":"selection.import","rect":[4,2,6,3],"png":mask(),"channel":"alpha","mode":"subtract"}),
    );
    assert_eq!(selection::current(&e.doc).unwrap().mask.get(5, 2)[0], 62);
    apply(&mut e, json!({"op":"selection.clear"}));
    apply(&mut e, json!({"op":"selection.reselect"}));
    assert_eq!(selection::current(&e.doc).unwrap().mask.get(5, 2)[0], 62);
}
#[test]
fn malformed_or_stale_import_is_atomic_and_mcp_feedback_has_actual_mask_coordinates() {
    let mut e = Engine::new();
    e.doc = Document::new(12, 8).unwrap();
    for extra in [
        json!({"png":"invalid"}),
        json!({"rect":[0,0,13,1]}),
        json!({"source_revision":999}),
        json!({"document_id":"different"}),
        json!({"channel":"rgb"}),
        json!({"mode":"invalid"}),
        json!({"path":"missing.png"}),
    ] {
        let mut c = json!({"op":"selection.import","rect":[4,2,8,4],"png":mask()});
        for (k, v) in extra.as_object().unwrap() {
            c[k] = v.clone();
        }
        let revision = e.doc.revision;
        assert!(e
            .edit("human", &[c], Some(revision), None, "Import")
            .is_err());
        assert_eq!(e.doc.revision, revision);
        assert!(e.doc.selection.is_none());
    }
    apply(
        &mut e,
        json!({"op":"selection.import","rect":[4,2,8,4],"png":mask()}),
    );
    let shared = Arc::new(Mutex::new(e));
    let reply = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_observe","arguments":{"rect":[4,2,8,4],"max_edge":32,"selection_view":"mask"}}}),
    );
    assert_eq!(reply["result"]["isError"], false);
    let content = reply["result"]["content"].as_array().unwrap();
    assert_eq!(content[1]["type"], "image");
    let metadata: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["views"][0]["document_rect"], json!([4, 2, 8, 4]));
    let bytes = STANDARD
        .decode(content[1]["data"].as_str().unwrap())
        .unwrap();
    let image = image::load_from_memory(&bytes).unwrap().to_rgba8();
    assert_eq!(image.get_pixel(2, 0).0, [191, 191, 191, 255]);
    assert!(server::tools()
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["name"] == "peerbrush_segment"));
    assert!(server::capabilities()["segmentation"]["configured"].is_boolean());
    let mut e = shared.lock().unwrap();
    let revision = e.doc.revision;
    e.edit(
        "selection-agent",
        &[json!({"op":"selection.import","rect":[4,2,8,4],"png":mask()})],
        Some(revision),
        None,
        "Select subject",
    )
    .unwrap();
    assert_eq!(e.ai_change.as_ref().unwrap().tool, "selection");
}
