use peerbrush::{
    brush::Settings,
    engine::{Document, Engine},
    preview::Cache,
    psd,
    raster::Raster,
};
use serde_json::{json, Value};
fn fixture(depth: u16, origin: [i32; 2], mask: bool, selected: bool) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(64, 48, depth).unwrap();
    let l = &mut e.doc.layers[0];
    l.x = origin[0];
    l.y = origin[1];
    l.pixels = Raster::new_depth((64 - origin[0]) as u32, (48 - origin[1]) as u32, depth);
    for y in 0..48 {
        for x in 0..64 {
            l.pixels.set16(
                x - l.x,
                y - l.y,
                [10001 + x as u16 * 137, 20003 + y as u16 * 157, 30007, 65535],
            );
        }
    }
    let id = l.id.clone();
    if mask {
        e.edit("human",&[json!({"op":"mask.add","layer":id,"value":255}),json!({"op":"paint","layer":id,"mask":true,"points":[[20.,22.]],"radius":7.,"color":[0,0,0,255]})],None,None,"Mask").unwrap();
    }
    if selected {
        e.edit(
            "human",
            &[json!({"op":"selection","rect":[8,6,58,44],"feather":1.})],
            None,
            None,
            "Selection",
        )
        .unwrap();
    }
    e.undo.clear();
    e
}
fn command(e: &Engine, op: &str, tip: &str, mask: bool) -> Value {
    let mut c = json!({"op":op,"layer":e.doc.layers[0].id,"points":[[16.25,20.5],[28.25,25.5],[44.25,26.5]],"pressures":[0.8,0.6,0.9],"radius":7.,"hardness":0.6,"opacity":0.7,"flow":0.8,"spacing":0.2,"tip":tip,"density":0.42,"grain":2.75,"seed":71,"angle":35.,"color":[180,40,90,255],"mask":mask});
    if op == "clone" || op == "heal" {
        // Keep the healing neighborhood inside content common to both frames;
        // padded masks legitimately have different data beyond the canvas edge.
        c["source"] = json!([21.25, 20.5]);
        c["sample_merged"] = json!(false);
    }
    c
}
#[test]
fn textures_match_equivalent_document_strokes_in_shifted_storage_at_native_depth() {
    for depth in [8, 16] {
        for mask in [false, true] {
            for selected in [false, true] {
                for tip in ["grain", "chalk", "dry", "bristle"] {
                    for op in ["paint", "smudge", "clone", "heal"] {
                        let mut reference = fixture(depth, [0, 0], mask, selected);
                        let c = command(&reference, op, tip, mask);
                        reference
                            .edit("human", &[c], None, None, "Reference stroke")
                            .unwrap();
                        let expected = reference.doc.export_png().unwrap();
                        let mut shifted = fixture(depth, [-13, -11], mask, selected);
                        let before = shifted.doc.clone();
                        let c = command(&shifted, op, tip, mask);
                        let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
                        assert!(
                            preview.export_png().unwrap() == expected,
                            "Document texture differs: {depth}, {mask}, {selected}, {tip}, {op}"
                        );
                        shifted
                            .edit("human", &[c], Some(before.revision), None, "Shifted stroke")
                            .unwrap();
                        assert_eq!(shifted.undo.len(), 1);
                        assert_eq!(shifted.doc.export_png().unwrap(), expected);
                        let saved = psd::decode(&psd::encode(&shifted.doc).unwrap()).unwrap();
                        assert_eq!(saved.bit_depth, depth);
                        assert_eq!(saved.export_png().unwrap(), expected);
                        shifted.undo("human").unwrap();
                        assert_eq!(
                            shifted.doc.export_png().unwrap(),
                            before.export_png().unwrap()
                        );
                    }
                }
            }
        }
    }
}
#[test]
fn incremental_texture_previews_follow_the_document_grid_at_shifted_origins() {
    for depth in [8, 16] {
        for mask in [false, true] {
            for tip in ["grain", "chalk", "dry", "bristle"] {
                let e = fixture(depth, [-13, -11], mask, false);
                let mut c = command(&e, "paint", tip, mask);
                let points = c["points"].as_array().unwrap().clone();
                let pressures = c["pressures"].as_array().unwrap().clone();
                let mut cache = Cache::default();
                for count in 1..=3 {
                    c["points"] = json!(&points[..count]);
                    c["pressures"] = json!(&pressures[..count]);
                    let cached = cache
                        .edit(e.doc.clone(), &[c.clone()], "texture gesture")
                        .unwrap();
                    let full = Engine::preview_edits(e.doc.clone(), &[c.clone()]).unwrap();
                    assert_eq!(cached.export_png().unwrap(), full.export_png().unwrap());
                    for edge in [32, 64, 96] {
                        let shown = cache
                            .render(&cached, "texture gesture", None, edge, None, false)
                            .unwrap();
                        let expected = full.preview(None, edge, None, false).unwrap();
                        assert_eq!(
                            (shown.width, shown.height, shown.bytes),
                            (expected.0, expected.1, expected.2)
                        );
                    }
                }
                assert!(e.undo.is_empty());
            }
        }
    }
}
#[test]
fn derived_texture_origin_does_not_enter_presets_or_settings_serialization() {
    let mut s = Settings::default();
    let before = serde_json::to_value(s).unwrap();
    s.texture_origin = [-13, 27];
    assert_eq!(serde_json::to_value(s).unwrap(), before);
    let decoded: Settings = serde_json::from_value(before).unwrap();
    assert_eq!(decoded.texture_origin, [0, 0]);
}
