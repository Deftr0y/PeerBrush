use peerbrush::{
    clipboard::Image,
    engine::{Document, Engine, Scope},
    psd, selection,
};
use serde_json::{json, Value};

fn edit(engine: &mut Engine, command: Value) -> Value {
    engine
        .edit(
            "human",
            &[command],
            Some(engine.doc.revision),
            None,
            "Duplicate selection regression",
        )
        .unwrap()
}
fn fixture(depth: u16) -> (Engine, String) {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(32, 24, depth).unwrap();
    let layer = &mut engine.doc.layers[0];
    layer.x = 3;
    layer.y = 2;
    for y in 0..24 {
        for x in 0..32 {
            layer
                .pixels
                .set16(x, y, [10001 + x as u16, 20003 + y as u16, 30007, 45679]);
        }
    }
    {
        let id = engine.doc.layers[0].id.clone();
        (engine, id)
    }
}

#[test]
fn selected_native_pixels_keep_holes_feather_sources_coordinates_and_psd_undo() {
    for depth in [8, 16] {
        let (mut engine, id) = fixture(depth);
        edit(&mut engine, json!({"op":"mask.add","layer":id,"value":177}));
        edit(
            &mut engine,
            json!({"op":"effect.add","layer":id,"kind":"invert","weight":0.3}),
        );
        edit(
            &mut engine,
            json!({"op":"layer.update","layer":id,"opacity":0.6}),
        );
        edit(
            &mut engine,
            json!({"op":"selection","kind":"ellipse","rect":[5,4,27,22],"feather":2}),
        );
        edit(
            &mut engine,
            json!({"op":"selection","kind":"rectangle","mode":"subtract","rect":[12,10,17,15]}),
        );
        engine.undo.clear();
        let before = engine.doc.clone();
        let selected = Image::copy(&before, &id, false, false).unwrap();
        let result = edit(
            &mut engine,
            json!({"op":"layer.duplicate_selection","layer":id,"document_id":before.id,"source_revision":before.revision}),
        );
        assert_eq!(result["created_roots"].as_array().unwrap().len(), 1);
        let copy = &engine.doc.layers[0];
        assert_eq!(copy.id, result["created_roots"][0].as_str().unwrap());
        assert_eq!([copy.x, copy.y], selected.origin.unwrap());
        assert_eq!(copy.kind, "paint");
        assert!(copy.parent.is_none());
        assert!(copy.mask.is_none() && copy.effects.is_empty());
        assert_eq!(copy.opacity, 1.0);
        assert_eq!(copy.pixels.depth, depth);
        assert_eq!(
            copy.pixels.get16(14 - copy.x, 12 - copy.y),
            [0; 4],
            "Selected hole stays transparent"
        );
        let words = copy.pixels.rgba16();
        if depth == 16 {
            assert_eq!(words, selected.samples16.unwrap());
            assert!(words
                .chunks_exact(4)
                .any(|pixel| pixel[3] > 0 && pixel[3] < 45679));
            assert!(words.iter().any(|word| word % 257 != 0));
        } else {
            assert_eq!(copy.pixels.rgba(), selected.bytes);
        }
        assert_eq!(
            engine.doc.layers[1].pixels.rgba16(),
            before.layers[0].pixels.rgba16()
        );
        assert_eq!(
            serde_json::to_value(&engine.doc.layers[1].mask).unwrap(),
            serde_json::to_value(&before.layers[0].mask).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&engine.doc.layers[1].effects).unwrap(),
            serde_json::to_value(&before.layers[0].effects).unwrap()
        );
        assert_eq!(selection::current(&engine.doc).unwrap().value(14, 12), 0.0);
        assert_eq!(engine.undo.len(), 1);
        let rendered = engine.doc.export_png().unwrap();
        let reopened = psd::decode(&psd::encode(&engine.doc).unwrap()).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(reopened.export_png().unwrap(), rendered);
        assert_eq!(reopened.layers[0].pixels.rgba16(), words);
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers.len(), 1);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
        assert_eq!(
            engine.doc.layers[0].pixels.rgba16(),
            before.layers[0].pixels.rgba16()
        );
        engine.redo("human").unwrap();
        assert_eq!(engine.doc.export_png().unwrap(), rendered);
    }
}

#[test]
fn unselected_layer_copy_retains_native_editable_sources_and_only_copies_active_layer() {
    let (mut engine, id) = fixture(16);
    edit(&mut engine, json!({"op":"mask.add","layer":id,"value":129}));
    edit(
        &mut engine,
        json!({"op":"effect.add","layer":id,"kind":"levels","settings":{"gamma":1.3}}),
    );
    let source = engine.doc.layers[0].clone();
    edit(&mut engine, json!({"op":"layer.add","name":"Other layer"}));
    engine.undo.clear();
    let result = edit(
        &mut engine,
        json!({"op":"layer.duplicate_selection","layer":id}),
    );
    assert_eq!(engine.doc.layers.len(), 3);
    let copied = engine
        .doc
        .layers
        .iter()
        .find(|layer| layer.id == result["created_roots"][0].as_str().unwrap())
        .unwrap();
    assert_eq!(copied.pixels.rgba16(), source.pixels.rgba16());
    assert_eq!((copied.x, copied.y), (source.x, source.y));
    assert_eq!(copied.effects[0].settings, source.effects[0].settings);
    assert_ne!(copied.effects[0].id, source.effects[0].id);
    assert_eq!(
        copied.mask.as_ref().unwrap().steps[0].pixels.rgba16(),
        source.mask.as_ref().unwrap().steps[0].pixels.rgba16()
    );
    assert_eq!(engine.undo.len(), 1);
}

#[test]
fn selected_copies_keep_clipping_units_contiguous_and_their_original_base() {
    let (mut engine, base) = fixture(16);
    let added = edit(&mut engine, json!({"op":"layer.add","name":"Clipped ink"}));
    let clip = added["created"][0].as_str().unwrap().to_string();
    engine.doc.layers[0]
        .pixels
        .set16(4, 4, [41237, 12347, 33559, 65535]);
    edit(
        &mut engine,
        json!({"op":"layer.clip","layer":clip,"base":base}),
    );
    edit(
        &mut engine,
        json!({"op":"selection","kind":"rectangle","rect":[3,2,20,18]}),
    );
    let base_copy = edit(
        &mut engine,
        json!({"op":"layer.duplicate_selection","layer":base}),
    );
    assert_eq!(
        engine.doc.layers[0].id,
        base_copy["created_roots"][0].as_str().unwrap()
    );
    assert!(engine.doc.layers[0].clip_to.is_none());
    assert_eq!(engine.doc.layers[1].id, clip);
    assert_eq!(engine.doc.layers[2].id, base);
    let clip_copy = edit(
        &mut engine,
        json!({"op":"layer.duplicate_selection","layer":clip}),
    );
    let copy = engine
        .doc
        .layers
        .iter()
        .find(|layer| layer.id == clip_copy["created_roots"][0].as_str().unwrap())
        .unwrap();
    assert_eq!(copy.clip_to.as_deref(), Some(base.as_str()));
    assert_eq!(
        copy.pixels.get16(4 - copy.x, 4 - copy.y),
        [41237, 12347, 33559, 65535]
    );
    psd::validate(&engine.doc).unwrap();
    let reopened = psd::decode(&psd::encode(&engine.doc).unwrap()).unwrap();
    assert_eq!(
        reopened.export_png().unwrap(),
        engine.doc.export_png().unwrap()
    );
}

#[test]
fn duplicate_refuses_empty_locked_reserved_stale_and_wrong_document_sources_atomically() {
    for failure in ["empty", "locked", "reserved", "stale", "document"] {
        let (mut engine, id) = fixture(16);
        edit(
            &mut engine,
            json!({"op":"selection","kind":"rectangle","rect":[5,4,20,18]}),
        );
        let mut command = json!({"op":"layer.duplicate_selection","layer":id,"document_id":engine.doc.id,"source_revision":engine.doc.revision});
        match failure {
            "empty" => {
                edit(
                    &mut engine,
                    json!({"op":"selection","kind":"rectangle","mode":"intersect","rect":[25,20,31,24]}),
                );
                command["source_revision"] = json!(engine.doc.revision);
            }
            "locked" => engine.doc.layers[0].locked = true,
            "reserved" => {
                engine
                    .reserve("agent", "Working", vec![Scope::layer(&id)])
                    .unwrap();
            }
            "stale" => command["source_revision"] = json!(0),
            "document" => command["document_id"] = json!("different-document"),
            _ => unreachable!(),
        }
        let before = engine.doc.clone();
        let history = engine.undo.len();
        assert!(
            engine
                .edit(
                    "human",
                    &[command],
                    Some(before.revision),
                    None,
                    "Rejected duplicate"
                )
                .is_err(),
            "{failure}"
        );
        assert_eq!(engine.doc.layers.len(), 1);
        assert_eq!(engine.doc.revision, before.revision);
        assert_eq!(
            engine.doc.layers[0].pixels.rgba16(),
            before.layers[0].pixels.rgba16()
        );
        assert_eq!(engine.doc.selection, before.selection);
        assert_eq!(engine.undo.len(), history);
    }
}
