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
