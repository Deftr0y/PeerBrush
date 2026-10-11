use peerbrush::{
    effects,
    engine::{Document, Engine},
    psd, server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn settings() -> Value {
    json!({"opacity":1.,"angle":0.,"color_stops":[
        {"position":0.,"color":[12345./65535.,23457./65535.,34569./65535.],"midpoint":0.25},
        {"position":0.5,"color":[45123./65535.,16237./65535.,52649./65535.]},
        {"position":1.,"color":[59713./65535.,38471./65535.,11719./65535.]}
    ],"alpha_stops":[{"position":0.,"opacity":0.},{"position":0.25,"opacity":1.},{"position":1.,"opacity":0.5}]})
}
#[test]
fn native_gradient_stops_interpolate_exact_endpoints_midpoints_and_independent_alpha() {
    let s = effects::normalized("gradient_overlay", &settings()).unwrap();
    assert_eq!(
        effects::gradient::sample(&s, 0.),
        ([12345, 23457, 34569], 0.)
    );
    assert_eq!(
        effects::gradient::sample(&s, 1.),
        ([59713, 38471, 11719], 0.5)
    );
    let (rgb, alpha) = effects::gradient::sample(&s, 0.125);
    assert_eq!(rgb, [28734, 19847, 43609]);
    assert_eq!(alpha, 0.5);
    let (rgb, alpha) = effects::gradient::sample(&s, 0.5);
    assert_eq!(rgb, [45123, 16237, 52649]);
    assert!((alpha - 5. / 6.).abs() < 1e-12);
}
#[test]
fn malformed_gradient_stops_reject_without_partial_edits_or_history() {
    let mut e = Engine::new();
    let layer = e.doc.layers[0].id.clone();
    let before = serde_json::to_value(&e.doc).unwrap();
    for bad in [
        json!(null),
        json!([{"position":0.,"color":[0.,0.,0.]}]),
        json!([{"position":0.,"color":[0.,0.,0.]},{"position":0.,"color":[1.,1.,1.]}]),
        json!([{"position":0.1,"color":[0.,0.,0.]},{"position":1.,"color":[1.,1.,1.]}]),
        json!([{"position":0.,"color":[0.,0.,0.],"midpoint":0.},{"position":1.,"color":[1.,1.,1.]}]),
        json!([{"position":0.,"color":[0.,0.,2.]},{"position":1.,"color":[1.,1.,1.]}]),
        json!([{"position":0.,"color":[0.,0.,0.],"unknown":true},{"position":1.,"color":[1.,1.,1.]}]),
    ] {
        let mut s = settings();
        s["color_stops"] = bad;
        assert!(e
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":s})],
                None,
                None,
                "Invalid gradient"
            )
            .is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert!(e.undo.is_empty());
    }
    let stops: Vec<_> = (0..17)
        .map(|i| json!({"position":i as f64/16.,"opacity":1.}))
        .collect();
    assert!(effects::gradient::validate(&json!(stops), true).is_err());
}
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(16, 8, depth).unwrap();
    for y in 0..8 {
        for x in 0..16 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [
                    12345 + x as u16 * 311,
                    23457 + y as u16 * 719,
                    34569,
                    if x % 3 == 0 { 41317 } else { 65535 },
                ],
            );
        }
    }
    e
}
fn strip(bytes: &[u8]) -> Vec<u8> {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let n = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    [
        bytes[..at].to_vec(),
        vec![0; 4],
        bytes[at + 4 + n..].to_vec(),
    ]
    .concat()
}
#[test]
fn gradient_stops_preserve_native_sources_preview_undo_and_actual_standard_composites() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let before = e.doc.export_png().unwrap();
        let source = e.doc.layers[0].pixels.rgba16();
        let layer = e.doc.layers[0].id.clone();
        let command = json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":settings()});
        let preview = Engine::preview_edits(e.doc.clone(), std::slice::from_ref(&command)).unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before);
        e.edit("human", &[command], None, None, "Gradient stops")
            .unwrap();
        assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
        let current = e.doc.export_png().unwrap();
        assert_eq!(current, preview.export_png().unwrap());
        assert_ne!(current, before);
        assert_eq!(e.undo.len(), 1);
        for bytes in [
            psd::encode(&e.doc).unwrap(),
            psd::encode_psb(&e.doc).unwrap(),
        ] {
            let d = psd::decode(&bytes).unwrap();
            assert!(!d.read_only);
            assert_eq!(d.layers[0].pixels.rgba16(), source);
            assert_eq!(
                d.layers[0].effects[0].settings["color_stops"],
                settings()["color_stops"]
            );
            assert_eq!(d.export_png().unwrap(), current);
        }
        let standard = psd::decode(&strip(&psd::encode(&e.doc).unwrap())).unwrap();
        assert!(!standard.read_only);
        assert_eq!(standard.export_png().unwrap(), current);
        e.undo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), before);
        e.redo("human").unwrap();
        assert_eq!(e.doc.export_png().unwrap(), current);
    }
}
#[test]
fn shared_gradient_stops_feedback_has_actual_native_pixels_and_document_coordinates() {
    let e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let shared = Arc::new(Mutex::new(e));
    let result=server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":settings()}],"feedback":"always","max_edge":32})).unwrap();
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 16, 8]));
    assert!(result["images"][0]["data"].as_str().unwrap().len() > 20);
    assert_eq!(shared.lock().unwrap().undo.len(), 1);
}

#[test]
fn positioned_gradients_match_analytic_native_coordinates_and_preserve_alpha() {
    for depth in [8, 16] {
        for (angle, ox, oy) in [
            (0., 0.125, 0.),
            (180., -0.125, 0.),
            (90., 0., 0.25),
            (-90., 0., -0.25),
        ] {
            for reflected in [false, true] {
                let mut e = Engine::new();
                e.doc = Document::new_depth(4, 2, depth).unwrap();
                for y in 0..2 {
                    for x in 0..4 {
                        e.doc.layers[0]
                            .pixels
                            .set16(x, y, [12347, 23459, 34571, 65535]);
                    }
                }
                let source = e.doc.layers[0].pixels.rgba16();
                let layer = e.doc.layers[0].id.clone();
                e.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":{"opacity":1.,"color":[0.,0.,0.],"color2":[1.,1.,1.],"angle":angle,"offset_x":ox,"offset_y":oy,"style":if reflected{"reflected"}else{"linear"}}})],None,None,"Gradient placement").unwrap();
                let actual = if depth == 16 {
                    peerbrush::depth16::render(&e.doc).unwrap().words
                } else {
                    e.doc
                        .preview(None, 8, None, false)
                        .unwrap()
                        .2
                        .into_iter()
                        .map(|v| u16::from(v) * 257)
                        .collect()
                };
                for y in 0..2 {
                    for x in 0..4 {
                        let px = (f64::from(x) + 0.5) / 4. - 0.5 - ox;
                        let py = (f64::from(y) + 0.5) / 2. - 0.5 - oy;
                        let p = if angle == 0. {
                            px
                        } else if angle == 180. {
                            -px
                        } else if angle == 90. {
                            -py
                        } else {
                            py
                        };
                        let t = if reflected { 2. * p.abs() } else { 0.5 + p };
                        let n = (t.clamp(0., 1.) * 65535.).round() as u16;
                        let n = if depth == 16 {
                            n
                        } else {
                            u16::from(peerbrush::raster::project16(n)) * 257
                        };
                        assert_eq!(
                            &actual[((y * 4 + x) * 4) as usize..][..4],
                            &[n, n, n, 65535]
                        );
                    }
                }
                assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
            }
        }
    }
}
#[test]
fn gradient_placement_shared_feedback_undo_save_and_disabled_sources_remain_exact() {
    for depth in [8, 16] {
        for disabled in [false, true] {
            let mut e = fixture(depth);
            let source = e.doc.layers[0].pixels.rgba16();
            let layer = e.doc.layers[0].id.clone();
            e.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":settings()})],None,None,"Initial gradient").unwrap();
            e.doc.layers[0].effects[0].enabled = !disabled;
            e.undo.clear();
            let effect = e.doc.layers[0].effects[0].id.clone();
            let before = e.doc.export_png().unwrap();
            let mut s = settings();
            s["offset_x"] = json!(0.25);
            s["offset_y"] = json!(-0.375);
            let shared = Arc::new(Mutex::new(e));
            let result=server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"effect.update","layer":layer,"effect":effect,"settings":s,"enabled":!disabled}],"feedback":"always","max_edge":32})).unwrap();
            assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 16, 8]));
            assert!(result["images"][0]["data"].as_str().unwrap().len() > 20);
            let mut e = shared.lock().unwrap();
            assert_eq!(e.undo.len(), 1);
            assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
            let current = e.doc.export_png().unwrap();
            for output in [
                psd::encode(&e.doc).unwrap(),
                psd::encode_psb(&e.doc).unwrap(),
            ] {
                let reopened = psd::decode(&output).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.export_png().unwrap(), current);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                assert_eq!(
                    reopened.layers[0].effects[0].settings["offset_x"],
                    json!(0.25)
                );
                assert_eq!(reopened.layers[0].effects[0].enabled, !disabled);
                let marker = output.windows(4).position(|v| v == b"PBR1").unwrap();
                use std::io::Read;
                let mut raw = Vec::new();
                flate2::read::ZlibDecoder::new(&output[marker + 4..])
                    .read_to_end(&mut raw)
                    .unwrap();
                let raw: Value = serde_json::from_slice(&raw).unwrap();
                assert_eq!(raw["format"], 23);
            }
            e.undo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), before);
            e.redo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), current);
        }
    }
}
#[test]
fn gradient_placement_invalid_or_inapplicable_settings_roll_back_atomically() {
    for depth in [8, 16] {
        for (kind, s) in [
            ("gradient_overlay", json!({"offset_x":1.01})),
            ("gradient_overlay", json!({"offset_y":-1.01})),
            ("gradient_overlay", json!({"offset_x":"0"})),
            ("gradient_overlay", json!({"offset_y":null})),
            ("color_overlay", json!({"offset_x":0.25})),
            ("bevel_emboss", json!({"offset_y":0.25})),
        ] {
            let mut e = fixture(depth);
            let before = serde_json::to_value(&e.doc).unwrap();
            let layer = e.doc.layers[0].id.clone();
            assert!(e
                .edit(
                    "human",
                    &[
                        json!({"op":"layer.update","layer":layer,"opacity":0.2}),
                        json!({"op":"effect.add","layer":layer,"kind":kind,"settings":s})
                    ],
                    None,
                    None,
                    "Invalid placement"
                )
                .is_err());
            assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
            assert!(e.undo.is_empty());
        }
    }
}
