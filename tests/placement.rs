use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{
    engine::{Engine, Layer, Scope},
    raster::{png, Raster},
    server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn encoded() -> String {
    STANDARD.encode(png(2, 1, &[200, 40, 10, 255, 20, 80, 240, 128]).unwrap())
}
fn place(layer: &str, new_layer: bool, area: [i32; 4]) -> Value {
    json!({"op":"image.place","layer":layer,"new_layer":new_layer,"rect":area,"png":encoded()})
}

#[test]
fn agent_image_placement_scales_into_folder_and_undoes_as_one_edit() {
    let mut e = Engine::new();
    let group = Layer::new("Assets", "group", 1024, 768);
    let group_id = group.id.clone();
    e.doc.layers.insert(0, group);
    let result = e
        .edit(
            "artist",
            &[place(&group_id, true, [100, 200, 104, 202])],
            Some(0),
            None,
            "Place generated detail",
        )
        .unwrap();
    let id = result["created"][0].as_str().unwrap();
    let layer = e.doc.layers.iter().find(|l| l.id == id).unwrap();
    assert_eq!(layer.parent.as_deref(), Some(group_id.as_str()));
    assert_eq!(
        (layer.x, layer.y, layer.pixels.width, layer.pixels.height),
        (100, 200, 4, 2)
    );
    assert_eq!(layer.pixels.get(0, 0), [200, 40, 10, 255]);
    assert_eq!(layer.pixels.get(3, 1), [20, 80, 240, 128]);
    assert_eq!(e.ai_change.as_ref().unwrap().tool, "place");
    assert!(e
        .ai_change
        .as_ref()
        .unwrap()
        .scopes
        .iter()
        .any(|s| s.target.as_deref() == Some(id)));
    let placed = layer.pixels.rgba();
    e.undo("artist").unwrap();
    assert_eq!(e.doc.layers.len(), 2);
    e.redo("artist").unwrap();
    assert_eq!(
        e.doc
            .layers
            .iter()
            .find(|l| l.id == id)
            .unwrap()
            .pixels
            .rgba(),
        placed
    );
}

#[test]
fn existing_image_expands_without_losing_pixels_mask_or_editable_effects() {
    let mut e = Engine::new();
    let id = e.doc.layers[0].id.clone();
    e.doc.layers[0].x = 10;
    e.doc.layers[0].y = 20;
    e.doc.layers[0].pixels = Raster::new(4, 4);
    e.doc.layers[0].pixels.set(2, 2, [10, 20, 30, 255]);
    e.edit("human",&[json!({"op":"mask.add","layer":id,"value":255}),json!({"op":"paint","layer":id,"mask":true,"points":[[12.5,22.5]],"radius":0.5,"color":[0,0,0,255]}),json!({"op":"effect.add","layer":id,"kind":"levels"})],None,None,"Prepare source").unwrap();
    let before = e.doc.clone();
    e.edit(
        "artist",
        &[place(&id, false, [8, 19, 10, 20])],
        Some(1),
        None,
        "Place patch",
    )
    .unwrap();
    let layer = &e.doc.layers[0];
    assert_eq!(
        (layer.x, layer.y, layer.pixels.width, layer.pixels.height),
        (8, 19, 6, 5)
    );
    assert_eq!(layer.pixels.get(4, 3), [10, 20, 30, 255]);
    assert_eq!(
        layer.mask_value_raw(4, 3),
        before.layers[0].mask_value_raw(2, 2)
    );
    assert_eq!(layer.effects.len(), 1);
    assert_eq!(layer.pixels.get(0, 0), [200, 40, 10, 255]);
    e.undo("artist").unwrap();
    assert_eq!((e.doc.layers[0].x, e.doc.layers[0].y), (10, 20));
    assert_eq!(
        e.doc.layers[0].pixels.rgba(),
        before.layers[0].pixels.rgba()
    );
}

#[test]
fn replacement_accepts_transparency_and_preserves_surroundings() {
    let mut e = Engine::new();
    let id = e.doc.layers[0].id.clone();
    e.doc.layers[0].pixels = Raster::new(4, 2);
    for x in 0..4 {
        e.doc.layers[0].pixels.set(x, 0, [255, 255, 255, 255]);
    }
    let mut command = place(&id, false, [1, 0, 3, 1]);
    command["mode"] = json!("replace");
    command["png"] = json!(STANDARD.encode(png(2, 1, &[0, 0, 0, 0, 20, 80, 240, 128]).unwrap()));
    e.edit("artist", &[command], Some(0), None, "Replace edited area")
        .unwrap();
    assert_eq!(e.doc.layers[0].pixels.get(0, 0), [255, 255, 255, 255]);
    assert_eq!(e.doc.layers[0].pixels.get(1, 0), [0, 0, 0, 0]);
    assert_eq!(e.doc.layers[0].pixels.get(2, 0), [20, 80, 240, 128]);
    assert_eq!(e.doc.layers[0].pixels.get(3, 0), [255, 255, 255, 255]);
}

#[test]
fn placement_rejects_locks_conflicts_bad_inputs_and_rolls_back_atomic_batch() {
    let mut e = Engine::new();
    let id = e.doc.layers[0].id.clone();
    e.reserve(
        "human",
        "Detail work",
        vec![Scope {
            target: Some(id.clone()),
            rect: Some([0, 0, 10, 10]),
        }],
    )
    .unwrap();
    assert!(e
        .edit(
            "artist",
            &[place(&id, false, [1, 1, 3, 2])],
            Some(0),
            None,
            "Overlap"
        )
        .is_err());
    e.edit(
        "artist",
        &[place(&id, false, [20, 20, 22, 21])],
        Some(0),
        None,
        "Disjoint",
    )
    .unwrap();
    let serial = e.ai_change.as_ref().unwrap().serial;
    let mut invalid = place(&id, false, [20, 20, 22, 21]);
    invalid["path"] = json!("relative.png");
    assert!(e
        .edit("artist", &[invalid], Some(1), None, "Invalid")
        .is_err());
    assert_eq!(e.ai_change.as_ref().unwrap().serial, serial);
    let before = e.doc.layers[0].pixels.rgba();
    assert!(e
        .edit(
            "artist",
            &[
                place(&id, false, [30, 30, 32, 31]),
                json!({"op":"unknown","layer":id})
            ],
            Some(1),
            None,
            "Rollback"
        )
        .is_err());
    assert_eq!(e.doc.layers[0].pixels.rgba(), before);
    e.doc.layers[0].locked = true;
    assert!(e
        .edit(
            "artist",
            &[place(&id, false, [20, 20, 22, 21])],
            None,
            None,
            "Locked"
        )
        .is_err());
}

#[test]
fn typed_mcp_image_tool_returns_pixels_and_exact_placement_manifest() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    let id = shared.lock().unwrap().doc.layers[0].id.clone();
    let response = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_place_image","arguments":{"layer":id,"new_layer":true,"png":encoded(),"rect":[10,20,12,21],"expected_revision":0,"actor":"artist","name":"Generated accent","max_edge":64}}}),
    );
    assert_eq!(response["result"]["isError"], false, "{response}");
    let content = response["result"]["content"].as_array().unwrap();
    assert_eq!(content[1]["type"], "image");
    let manifest: Value = serde_json::from_str(content[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(manifest["placement"]["rect"], json!([10, 20, 12, 21]));
    assert_eq!(
        manifest["views"][0]["document_rect"],
        json!([10, 20, 12, 21])
    );
    assert!(manifest["placement"]["layer"].is_string());
}

#[test]
fn a_small_patch_shares_untouched_tiles_and_mask_sources() {
    let mut e = Engine::new();
    let id = e.doc.layers[0].id.clone();
    e.doc.layers[0].pixels.set(600, 400, [50, 60, 70, 255]);
    e.edit("human",&[json!({"op":"mask.add","layer":id,"value":255}),json!({"op":"paint","layer":id,"mask":true,"points":[[600,400]],"radius":4,"color":[0,0,0,255]})],None,None,"Prepare mask").unwrap();
    let tile = e.doc.layers[0].pixels.tiles[&(2, 1)].clone();
    let mask_tile = e.doc.layers[0].mask.as_ref().unwrap().steps[1].pixels.tiles[&(2, 1)].clone();
    let mask_key = e.doc.layers[0].mask.as_ref().unwrap().cache_key.clone();
    e.edit(
        "artist",
        &[place(&id, false, [20, 20, 22, 21])],
        Some(1),
        None,
        "Small patch",
    )
    .unwrap();
    assert!(Arc::ptr_eq(&tile, &e.doc.layers[0].pixels.tiles[&(2, 1)]));
    assert!(Arc::ptr_eq(
        &mask_tile,
        &e.doc.layers[0].mask.as_ref().unwrap().steps[1].pixels.tiles[&(2, 1)]
    ));
    assert_eq!(e.doc.layers[0].mask.as_ref().unwrap().cache_key, mask_key);
}

#[test]
fn overriding_parent_invalidates_its_prepared_group_effect_image() {
    let mut e = Engine::new();
    let context = e.doc.layers[0].id.clone();
    let folder = Layer::new("Effected destination", "group", 1024, 768);
    let parent = folder.id.clone();
    e.doc.layers.insert(0, folder);
    e.edit(
        "human",
        &[json!({"op":"effect.add","layer":parent,"kind":"invert"})],
        None,
        None,
        "Destination effect",
    )
    .unwrap();
    let before = e
        .doc
        .preview(Some([20, 20, 22, 21]), 32, None, false)
        .unwrap()
        .2;
    let mut command = place(&context, true, [20, 20, 22, 21]);
    command["parent"] = json!(parent);
    let result = e
        .edit(
            "artist",
            &[command],
            Some(1),
            None,
            "Place in another folder",
        )
        .unwrap();
    let layer = e
        .doc
        .layers
        .iter()
        .find(|l| Some(l.id.as_str()) == result["created"][0].as_str())
        .unwrap();
    assert_eq!(layer.parent.as_deref(), Some(parent.as_str()));
    let after = e
        .doc
        .preview(Some([20, 20, 22, 21]), 32, None, false)
        .unwrap()
        .2;
    assert_ne!(before, after);
    assert_eq!(&after[0..4], &[55, 215, 245, 255]);
}
