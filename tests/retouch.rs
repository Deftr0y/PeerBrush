use peerbrush::{
    engine::{Document, Engine, Scope},
    psd, selection,
};
use serde_json::{json, Value};
fn fixture(depth: u16) -> (Engine, String) {
    let mut e = Engine::new();
    e.doc = Document::new_depth(520, 40, depth).unwrap();
    let layer = e.doc.layers[0].id.clone();
    (e, layer)
}
fn edit(e: &mut Engine, c: Value) {
    e.edit("human", &[c], Some(e.doc.revision), None, "Retouch")
        .unwrap();
}
#[test]
fn clone_reads_a_frozen_source_and_preserves_native_words_undo_and_psd() {
    let (mut e, layer) = fixture(16);
    for x in 0..520 {
        for y in 0..40 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [113 + x as u16 * 37, 271 + y as u16 * 113, 539, 65535],
            );
        }
    }
    let before = e.doc.clone();
    let png = before.export_png().unwrap();
    let command = json!({"op":"clone","layer":layer,"source":[250,20],"points":[[256,20],[264,20]],"radius":4,"hardness":1,"spacing":0.1});
    let preview = Engine::preview_edits(before.clone(), &[command.clone()]).unwrap();
    edit(&mut e, command);
    assert_eq!(
        e.doc.layers[0].pixels.get16(260, 20),
        before.layers[0].pixels.get16(254, 20)
    );
    assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
    assert_eq!(
        e.doc.layers[0].pixels.get16(20, 20),
        before.layers[0].pixels.get16(20, 20)
    );
    let decoded = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
    assert!(!decoded.read_only);
    assert_eq!(decoded.export_png().unwrap(), e.doc.export_png().unwrap());
    e.undo("human").unwrap();
    assert_eq!(e.doc.export_png().unwrap(), png);
}
#[test]
fn clone_maps_other_layer_document_origins_and_fractional_premultiplied_samples() {
    let (mut e, destination) = fixture(16);
    let source = e
        .edit(
            "human",
            &[json!({"op":"layer.add","name":"Source"})],
            None,
            None,
            "Source",
        )
        .unwrap()["created"][0]
        .as_str()
        .unwrap()
        .to_string();
    e.doc.layers[0].x = 30;
    e.doc.layers[0].y = 5;
    e.doc.layers[0].pixels.set16(2, 3, [123, 271, 539, 65535]);
    e.doc.layers[0].pixels.set16(3, 3, [65123, 1, 65530, 0]);
    edit(
        &mut e,
        json!({"op":"clone","layer":destination,"source_layer":source,"source":[32.5,8],"points":[[100,20]],"radius":2,"hardness":1}),
    );
    assert_eq!(
        e.doc.layers[1].pixels.get16(100, 20),
        [123, 271, 539, 32768]
    );
    assert_eq!(e.doc.layers[0].pixels.get16(2, 3), [123, 271, 539, 65535]);
    assert_eq!(e.doc.layers[0].pixels.get16(3, 3), [65123, 1, 65530, 0]);
}
#[test]
fn healing_removes_a_defect_and_adapts_texture_to_destination_tone_across_tiles() {
    let (mut e, layer) = fixture(16);
    for y in 0..40 {
        for x in 0..520 {
            let base = if x < 200 { 10000 } else { 40000 };
            let texture = if x % 2 == 0 { 1000 } else { 0 };
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [
                    base + texture,
                    base + texture + 123,
                    base + texture + 271,
                    if x < 200 { 65535 } else { 45123 },
                ],
            );
        }
    }
    e.doc.layers[0]
        .pixels
        .set16(256, 20, [60000, 60123, 60271, 45123]);
    let before = e.doc.clone();
    edit(
        &mut e,
        json!({"op":"heal","layer":layer,"source":[100,20],"points":[[252,20],[260,20]],"radius":3,"heal_radius":8,"hardness":1}),
    );
    let healed = e.doc.layers[0].pixels.get16(256, 20);
    assert!(
        (healed[0] as i32 - 41000).abs() < 6000,
        "Healed value {} should approach destination tone while retaining sampled texture",
        healed[0]
    );
    assert_eq!(healed[3], 45123);
    assert_eq!(healed[1] - healed[0], 123);
    assert_eq!(healed[2] - healed[0], 271);
    for x in [253, 255, 257, 259] {
        let p = e.doc.layers[0].pixels.get16(x, 20);
        assert_eq!(p[3], 45123);
        assert!(p[0] < 44000);
    }
    assert_eq!(
        e.doc.layers[0].pixels.get16(100, 20),
        before.layers[0].pixels.get16(100, 20)
    );
    e.undo("human").unwrap();
    assert_eq!(e.doc.export_png().unwrap(), before.export_png().unwrap());
}
#[test]
fn clone_honors_soft_selection_holes_and_native_mask_grayscale() {
    for depth in [8, 16] {
        let (mut e, layer) = fixture(depth);
        for y in 0..40 {
            for x in 0..40 {
                e.doc.layers[0]
                    .pixels
                    .set16(x, y, [12345, 23567, 45679, 65535]);
            }
        }
        edit(
            &mut e,
            json!({"op":"selection","kind":"rectangle","rect":[250,12,262,28],"feather":2}),
        );
        edit(
            &mut e,
            json!({"op":"selection","kind":"rectangle","rect":[255,18,258,23],"mode":"subtract"}),
        );
        let coverage = selection::current(&e.doc).unwrap();
        edit(
            &mut e,
            json!({"op":"clone","layer":layer,"source":[20,20],"points":[[256,20]],"radius":10,"hardness":1}),
        );
        assert_eq!(e.doc.layers[0].pixels.get16(256, 20)[3], 0);
        assert_eq!(e.doc.layers[0].pixels.get16(253, 20)[3], 65535);
        let value = coverage.value(250, 20);
        assert!(value > 0. && value < 1.);
        let alpha = e.doc.layers[0].pixels.get16(250, 20)[3] as f32;
        assert!((alpha - value * 65535.).abs() <= if depth == 16 { 1. } else { 129. });
    }
    let (mut e, layer) = fixture(16);
    edit(&mut e, json!({"op":"mask.add","layer":layer,"value":0}));
    e.doc.layers[0]
        .mask
        .as_mut()
        .unwrap()
        .steps
        .last_mut()
        .unwrap()
        .pixels
        .set16(5, 5, [123, 123, 123, 65535]);
    e.doc.layers[0].mask.as_mut().unwrap().cache_key = peerbrush::engine::id();
    edit(
        &mut e,
        json!({"op":"clone","layer":layer,"mask":true,"source":[5,5],"points":[[20,20]],"radius":2}),
    );
    assert_eq!(
        e.doc.layers[0]
            .mask
            .as_ref()
            .unwrap()
            .steps
            .last()
            .unwrap()
            .pixels
            .get16(20, 20),
        [123, 123, 123, 65535]
    );
}
#[test]
fn malformed_locked_reserved_and_stale_retouch_is_atomic() {
    let (mut e, layer) = fixture(16);
    let before = e.doc.export_png().unwrap();
    for extra in [
        json!({"source":null}),
        json!({"source":[0]}),
        json!({"source_layer":"missing"}),
        json!({"heal_radius":65}),
        json!({"source_revision":999}),
        json!({"document_id":"different"}),
        json!({"radius":99999}),
        json!({"mask":true}),
        json!({"sample_merged":"yes"}),
        json!({"source_step":"unknown"}),
    ] {
        let mut c =
            json!({"op":"clone","layer":layer,"source":[1,1],"points":[[20,20]],"radius":2});
        for (k, v) in extra.as_object().unwrap() {
            c[k] = v.clone();
        }
        let revision = e.doc.revision;
        assert!(e
            .edit("human", &[c], Some(revision), None, "Retouch")
            .is_err());
        assert_eq!(e.doc.revision, revision);
        assert_eq!(e.doc.export_png().unwrap(), before);
    }
    let c = json!({"op":"heal","layer":layer,"source":[1,1],"points":[[20,20]],"radius":2});
    e.doc.layers[0].locked = true;
    assert!(e.edit("human", &[c.clone()], None, None, "Heal").is_err());
    e.doc.layers[0].locked = false;
    e.reserve(
        "other",
        "Retouch region",
        vec![Scope {
            target: Some(layer),
            rect: Some([15, 15, 25, 25]),
        }],
    )
    .unwrap();
    assert!(e.edit("human", &[c], None, None, "Heal").is_err());
    assert_eq!(e.doc.export_png().unwrap(), before);
}

#[test]
fn merged_clone_samples_native_render_and_heal_preserves_hidden_transparent_words() {
    let (mut e, layer) = fixture(16);
    for y in 0..40 {
        for x in 0..40 {
            e.doc.layers[0].pixels.set16(x, y, [123, 271, 539, 65535]);
        }
    }
    edit(
        &mut e,
        json!({"op":"effect.add","layer":layer,"kind":"invert"}),
    );
    let rendered = peerbrush::depth16::render(&e.doc).unwrap().get(20, 20);
    edit(
        &mut e,
        json!({"op":"clone","layer":layer,"source":[20,20],"points":[[256,20]],"radius":3,"hardness":1,"sample_merged":true}),
    );
    assert_eq!(e.doc.layers[0].pixels.get16(256, 20), rendered);
    e.doc.layers[0].pixels.set16(300, 20, [234, 456, 789, 0]);
    edit(
        &mut e,
        json!({"op":"heal","layer":layer,"source":[20,20],"points":[[300,20]],"radius":3,"hardness":1}),
    );
    assert_eq!(e.doc.layers[0].pixels.get16(300, 20), [234, 456, 789, 0]);
}
