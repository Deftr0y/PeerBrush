use peerbrush::{
    engine::{Document, Engine, Scope},
    psd,
    raster::{blend, Raster},
    server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};

#[test]
fn sparse_history_keeps_unmodified_tiles_shared() {
    let mut r = Raster::new(1024, 1024);
    assert_eq!(r.bytes(), 0);
    r.set(10, 10, [255, 0, 0, 255]);
    let before = r.clone();
    let old = before.tiles.values().next().unwrap().clone();
    r.set(10, 10, [0, 255, 0, 255]);
    assert_eq!(before.get(10, 10), [255, 0, 0, 255]);
    assert_eq!(r.get(10, 10), [0, 255, 0, 255]);
    assert!(!Arc::ptr_eq(&old, r.tiles.values().next().unwrap()));
}

#[test]
fn blending_preserves_transparency_without_dark_fringes() {
    assert_eq!(
        blend([0, 0, 0, 0], [240, 60, 20, 128], 1.0, "normal"),
        [240, 60, 20, 128]
    );
    assert_eq!(
        blend([255, 255, 255, 255], [100, 150, 200, 255], 1.0, "multiply"),
        [100, 150, 200, 255]
    );
}

#[test]
fn failed_batch_is_atomic() {
    let mut e = Engine::new();
    let layer = e.doc.layers[0].id.clone();
    let r = e.edit(
        "agent",
        &[
            json!({"op":"fill","layer":layer,"color":[255,0,0,255]}),
            json!({"op":"unknown","layer":layer}),
        ],
        Some(0),
        None,
        "bad batch",
    );
    assert!(r.is_err());
    assert_eq!(e.doc.revision, 0);
    assert_eq!(e.doc.layers[0].pixels.bytes(), 0);
    assert!(e.undo.is_empty());
}

#[test]
fn region_reservation_blocks_overlap_but_not_elsewhere() {
    let mut e = Engine::new();
    let layer = e.doc.layers[0].id.clone();
    e.reserve(
        "agent",
        "mask work",
        vec![Scope {
            target: Some(layer.clone()),
            rect: Some([0, 0, 100, 100]),
        }],
    )
    .unwrap();
    assert!(e.edit("human",&[json!({"op":"paint","layer":layer,"points":[[50,50]],"radius":5,"color":[255,0,0,255]})],None,None,"overlap").is_err());
    assert!(e.edit("human",&[json!({"op":"paint","layer":layer,"points":[[200,200]],"radius":5,"color":[255,0,0,255]})],None,None,"outside").is_ok());
}

#[test]
fn optimistic_edits_reject_conflicts_and_allow_disjoint_regions() {
    let mut e = Engine::new();
    let layer = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"paint","layer":layer,"points":[[50,50]],"radius":5,"color":[255,0,0,255]})],
        Some(0),
        None,
        "human",
    )
    .unwrap();
    assert!(e.edit("ai",&[json!({"op":"paint","layer":layer,"points":[[200,200]],"radius":5,"color":[0,255,0,255]})],Some(0),None,"disjoint").is_ok());
    assert!(e.edit("ai",&[json!({"op":"paint","layer":layer,"points":[[50,50]],"radius":5,"color":[0,255,0,255]})],Some(0),None,"conflict").is_err());
}

#[test]
fn takeover_rejects_late_task_writes() {
    let mut e = Engine::new();
    let layer = e.doc.layers[0].id.clone();
    let task = e
        .reserve("agent", "work", vec![Scope::layer(&layer)])
        .unwrap();
    e.leases.clear();
    assert!(e
        .edit(
            "agent",
            &[json!({"op":"layer.update","layer":layer,"name":"changed"})],
            Some(0),
            Some(&task.id),
            "late"
        )
        .is_err());
}

#[test]
fn undo_redo_and_mask_paint_roundtrip() {
    let mut e = Engine::new();
    let layer = e.doc.layers[0].id.clone();
    e.edit("human",&[json!({"op":"fill","layer":layer,"rect":[0,0,20,20],"color":[233,84,32,255]}),json!({"op":"mask.add","layer":layer,"value":255}),json!({"op":"paint","layer":layer,"mask":true,"points":[[10,10]],"radius":3,"color":[0,0,0,255]})],None,None,"mask").unwrap();
    assert!(e.doc.layers[0].mask_value(10, 10) < 0.05);
    e.undo("human").unwrap();
    assert!(e.doc.layers[0].mask.is_none());
    e.redo("human").unwrap();
    assert!(e.doc.layers[0].mask.is_some());
}

#[test]
fn psd_preserves_sources_and_actual_composite() {
    let mut e = Engine::new();
    e.doc = Document::new(64, 48).unwrap();
    let layer = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[
            json!({"op":"fill","layer":layer,"color":[233,84,32,255]}),
            json!({"op":"mask.add","layer":layer,"value":128}),
            json!({"op":"mask.step.add","layer":layer,"kind":"levels","value":1.2}),
        ],
        None,
        None,
        "art",
    )
    .unwrap();
    let bytes = psd::encode(&e.doc).unwrap();
    assert_eq!(&bytes[..4], b"8BPS");
    let reopened = psd::decode(&bytes).unwrap();
    assert_eq!(reopened.layers[0].id, layer);
    assert_eq!(reopened.layers[0].mask.as_ref().unwrap().steps.len(), 3);
    assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
}

#[test]
fn transparent_psd_fallback_unmattes_the_merged_preview() {
    let mut doc = Document::new(16, 16).unwrap();
    doc.layers[0].pixels.set(3, 4, [233, 84, 32, 128]);
    doc.layers[0].pixels.set(5, 6, [80, 170, 220, 40]);
    let mut bytes = psd::encode(&doc).unwrap();
    // An external editor's unsupported blend invalidates PeerBrush sources.
    // The read-only view must use standard merged pixels, including alpha.
    let index = bytes.windows(8).position(|w| w == b"8BIMnorm").unwrap();
    bytes[index + 4..index + 8].copy_from_slice(b"hue ");
    let reopened = psd::decode(&bytes).unwrap();
    assert!(reopened.read_only);
    for (x, y) in [(3, 4), (5, 6), (0, 0)] {
        let expected = doc.layers[0].pixels.get(x, y);
        let actual = reopened.layers[0].pixels.get(x, y);
        assert_eq!(actual[3], expected[3]);
        for c in 0..3 {
            let difference =
                (actual[c] as f32 - expected[c] as f32).abs() * actual[3] as f32 / 255.0;
            assert!(difference <= 1.0, "Premultiplied color error {difference}");
        }
    }
}
#[test]
fn external_changes_do_not_restore_stale_sources() {
    let mut e = Engine::new();
    e.doc = Document::new(32, 32).unwrap();
    let mut bytes = psd::encode(&e.doc).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 1;
    let reopened = psd::decode(&bytes).unwrap();
    assert_ne!(reopened.layers[0].id, e.doc.layers[0].id);
    assert!(reopened.warnings.iter().any(|w| w.contains("externally")));
}

#[test]
fn parser_rejects_truncated_and_oversized_documents() {
    let doc = Document::new(16, 16).unwrap();
    let bytes = psd::encode(&doc).unwrap();
    for cut in [0, 4, 20, 30, bytes.len() - 1] {
        assert!(psd::decode(&bytes[..cut]).is_err());
    }
    let mut bad = bytes;
    bad[14..18].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(psd::decode(&bad).is_err());
}

#[test]
fn observation_crop_and_unchanged_feedback() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    let v = server::dispatch(
        &shared,
        "observe",
        &json!({"rect":[100,150,300,250],"max_edge":100}),
    )
    .unwrap();
    assert_eq!(v["images"][0]["width"], 100);
    assert_eq!(v["images"][0]["height"], 50);
    assert_eq!(v["images"][0]["document_rect"], json!([100, 150, 300, 250]));
    let v = server::dispatch(&shared, "observe", &json!({"since_revision":0})).unwrap();
    assert!(v["images"].as_array().unwrap().is_empty());
    let v = server::dispatch(&shared, "observe", &json!({"rect":[2000,2000,2100,2100]}));
    assert!(v.is_ok());
}

#[test]
fn mcp_returns_image_blocks_and_typed_errors() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    let v = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_observe","arguments":{"max_edge":32}}}),
    );
    assert_eq!(v["result"]["content"][1]["type"], "image");
    let v = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"peerbrush_edit","arguments":{"commands":[{"op":"unknown"}]}}}),
    );
    assert_eq!(v["result"]["isError"], true);
}

#[test]
fn ai_can_iterate_but_cannot_undo_interleaved_human_work() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    let layer = shared.lock().unwrap().doc.layers[0].id.clone();
    server::dispatch(&shared,"edit",&json!({"actor":"artist","commands":[{"op":"layer.update","layer":layer,"name":"Try one"}],"feedback":"request"})).unwrap();
    let undone = server::dispatch(
        &shared,
        "history",
        &json!({"actor":"artist","action":"undo","expected_revision":1,"max_edge":32}),
    )
    .unwrap();
    assert_eq!(undone["revision"], 2);
    assert_eq!(undone["images"][0]["revision"], 2);
    assert!(server::dispatch(
        &shared,
        "history",
        &json!({"actor":"artist","action":"redo","expected_revision":1})
    )
    .is_err());
    server::dispatch(
        &shared,
        "history",
        &json!({"actor":"artist","action":"redo","expected_revision":2,"feedback":"request"}),
    )
    .unwrap();
    shared
        .lock()
        .unwrap()
        .edit(
            "human",
            &[json!({"op":"layer.update","layer":layer,"name":"Human idea"})],
            None,
            None,
            "human idea",
        )
        .unwrap();
    assert!(server::dispatch(
        &shared,
        "history",
        &json!({"actor":"artist","action":"undo","expected_revision":4})
    )
    .is_err());
    assert_eq!(shared.lock().unwrap().doc.layers[0].name, "Human idea");
}

#[test]
fn transform_resamples_pixels_and_masks_and_undo_restores_original() {
    let mut e = Engine::new();
    e.doc = Document::new(32, 32).unwrap();
    let id = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[
            json!({"op":"fill","layer":id,"rect":[2,2,6,4],"color":[255,100,20,255]}),
            json!({"op":"mask.add","layer":id,"value":128}),
        ],
        None,
        None,
        "source",
    )
    .unwrap();
    let original = e.doc.export_png().unwrap();
    e.edit(
        "human",
        &[json!({"op":"transform","layer":id,"angle":90,"pivot":[4,3]})],
        None,
        None,
        "rotate",
    )
    .unwrap();
    let l = &e.doc.layers[0];
    assert_eq!((l.pixels.width, l.pixels.height), (2, 4));
    assert_eq!((l.x, l.y), (3, 1));
    assert_eq!(l.pixels.get(0, 0), [255, 100, 20, 255]);
    assert!((l.mask_value(0, 0) - 128.0 / 255.0).abs() < 0.001);
    let reopened = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert_eq!(reopened.export_png().unwrap(), e.doc.export_png().unwrap());
    e.undo("human").unwrap();
    assert_eq!(e.doc.export_png().unwrap(), original);
}

#[test]
fn image_imports_are_atomic_and_share_undo_history() {
    let path = std::env::temp_dir().join(format!("peerbrush-test-{}.png", uuid::Uuid::new_v4()));
    std::fs::write(&path, peerbrush::raster::png(2, 2, &[40; 16]).unwrap()).unwrap();
    let mut e = Engine::new();
    assert!(e
        .edit(
            "human",
            &[
                json!({"op":"image.import","path":path}),
                json!({"op":"image.import","path":"missing.png"})
            ],
            None,
            None,
            "drop"
        )
        .is_err());
    assert_eq!(e.doc.layers.len(), 1);
    assert_eq!(e.doc.revision, 0);
    e.edit(
        "human",
        &[json!({"op":"image.import","path":path})],
        None,
        None,
        "drop",
    )
    .unwrap();
    assert_eq!(e.doc.layers.len(), 2);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers.len(), 1);
    std::fs::remove_file(path).unwrap();
}

#[test]
fn mcp_presence_and_activity_follow_live_clients_and_tasks() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    server::mcp(
        &shared,
        &json!({"id":1,"method":"initialize","_client":"test-agent"}),
    );
    assert!(shared
        .lock()
        .unwrap()
        .mcp_clients
        .contains_key("test-agent"));
    let task = server::dispatch(
        &shared,
        "task",
        &json!({"action":"begin","description":"Choosing a softer edge","scopes":[]}),
    )
    .unwrap();
    server::dispatch(
        &shared,
        "task",
        &json!({"action":"update","task":task["id"],"description":"Painting the softer edge"}),
    )
    .unwrap();
    assert_eq!(shared.lock().unwrap().activity, "Painting the softer edge");
    server::dispatch(
        &shared,
        "client_state",
        &json!({"client":"test-agent","connected":false}),
    )
    .unwrap();
    assert!(shared.lock().unwrap().mcp_clients.is_empty());
}

#[test]
fn group_reservations_protect_descendants() {
    let mut e = Engine::new();
    let child = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"layer.add","kind":"group"})],
        None,
        None,
        "group",
    )
    .unwrap();
    let group = e.doc.layers[0].id.clone();
    e.edit(
        "human",
        &[json!({"op":"layer.parent","layer":child,"parent":group})],
        None,
        None,
        "parent",
    )
    .unwrap();
    e.reserve("artist", "group mask", vec![Scope::layer(&group)])
        .unwrap();
    assert!(e
        .edit(
            "human",
            &[json!({"op":"paint","layer":child,"points":[[10,10]],"radius":2})],
            None,
            None,
            "conflict"
        )
        .is_err());
}

#[test]
fn extreme_agent_coordinates_fail_without_corrupting_state() {
    let mut e = Engine::new();
    let layer = e.doc.layers[0].id.clone();
    for command in [
        json!({"op":"move","layer":layer,"dx":1e100}),
        json!({"op":"shape","layer":layer,"rect":[-2147483648,0,2147483647,20]}),
        json!({"op":"paint","layer":layer,"points":[[1e100,20]]}),
    ] {
        assert!(e.edit("artist", &[command], None, None, "invalid").is_err());
        assert_eq!(e.doc.revision, 0);
    }
}

#[test]
fn a_workspace_allows_one_server_and_focuses_the_existing_instance() {
    let path = std::env::temp_dir().join(format!("peerbrush-instance-{}", uuid::Uuid::new_v4()));
    let shared = Arc::new(Mutex::new(Engine::new()));
    let connection = server::start(shared.clone(), path.clone()).unwrap();
    assert!(server::start(Arc::new(Mutex::new(Engine::new())), path.clone()).is_err());
    assert!(server::focus_existing(&path));
    assert!(shared.lock().unwrap().focus_requested);
    drop(connection);
}
