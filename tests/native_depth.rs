use peerbrush::{
    engine::{Document, Engine, Layer},
    raster::{blend16, Raster},
    server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
fn fixture() -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(8, 6, 16).unwrap();
    for y in 0..6 {
        for x in 0..8 {
            e.doc.layers[0].pixels.set16(
                x,
                y,
                [30001 + x as u16 * 17, 17003 + y as u16 * 23, 8127, 65535],
            );
        }
    }
    e
}
fn apply(e: &mut Engine, c: serde_json::Value) {
    e.edit("human", &[c], None, None, "Native precision")
        .unwrap();
}
#[test]
fn selection_translation_and_identity_resampling_keep_native_samples_and_history() {
    let mut e = fixture();
    let id = e.doc.layers[0].id.clone();
    let before = e.doc.layers[0].pixels.rgba16();
    apply(
        &mut e,
        json!({"op":"transform","layer":id,"angle":0,"scale_x":1,"scale_y":1,"selection_only":false}),
    );
    assert_eq!(e.doc.layers[0].pixels.rgba16(), before);
    e.undo("human").unwrap();
    apply(&mut e, json!({"op":"selection","rect":[0,0,2,2]}));
    let selected = e.doc.layers[0].pixels.get16(0, 0);
    apply(&mut e, json!({"op":"move","layer":id,"dx":3,"dy":2}));
    assert_eq!(e.doc.layers[0].pixels.get16(3, 2), selected);
    assert_eq!(e.doc.layers[0].pixels.get16(0, 0), [0; 4]);
    assert_eq!(
        e.doc.layers[0].pixels.get16(7, 5),
        [30120, 17118, 8127, 65535]
    );
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers[0].pixels.rgba16(), before);
    e.redo("human").unwrap();
    assert_eq!(e.doc.layers[0].pixels.get16(3, 2), selected);
}
#[test]
fn merge_and_png_export_quantize_at_sixteen_bits_and_are_reversible() {
    let mut e = fixture();
    let base = e.doc.layers[0].id.clone();
    let original = e.doc.layers[0].pixels.get16(0, 0);
    let mut top = Layer::new("Native tint", "paint", 8, 6);
    top.pixels = Raster::new_depth(8, 6, 16);
    top.pixels.set16(0, 0, [12345, 54321, 19283, 23457]);
    let top_id = top.id.clone();
    e.doc.layers.insert(0, top);
    let expected = blend16(original, [12345, 54321, 19283, 23457], 1.0, "normal");
    apply(&mut e, json!({"op":"layer.merge","layers":[top_id,base]}));
    assert_eq!(e.doc.layers[0].pixels.get16(0, 0), expected);
    assert_eq!(e.doc.layers[0].pixels.depth, 16);
    let bytes = e.doc.export_png().unwrap();
    assert_eq!(bytes[24], 16, "PNG IHDR bit depth");
    let image = image::load_from_memory(&bytes).unwrap().to_rgba16();
    assert_eq!(image.get_pixel(0, 0).0, expected);
    e.undo("human").unwrap();
    assert_eq!(e.doc.layers.len(), 2);
    assert_eq!(e.doc.layers[1].pixels.get16(0, 0), original);
}
#[test]
fn protected_copy_preserves_native_channels_and_requires_a_new_save_path() {
    let mut e = fixture();
    e.doc.read_only = true;
    e.doc.name = "Precision.psd".into();
    let exact = e.doc.layers[0].pixels.rgba16();
    let shared = Arc::new(Mutex::new(e));
    server::compatible_copy(&shared, "human", Some(0)).unwrap();
    let e = shared.lock().unwrap();
    assert_eq!(e.doc.bit_depth, 16);
    assert!(!e.doc.read_only);
    assert!(e.path.is_none());
    assert_eq!(e.doc.layers[0].pixels.rgba16(), exact);
    assert_ne!(e.saved_revision, e.doc.revision);
}
#[test]
fn document_api_and_gradient_keep_depth_and_sub_byte_coverage() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    server::dispatch(
        &shared,
        "document",
        &json!({"action":"new","width":9,"height":3,"bit_depth":16}),
    )
    .unwrap();
    assert_eq!(shared.lock().unwrap().doc.bit_depth, 16);
    let mut e = shared.lock().unwrap();
    let id = e.doc.layers[0].id.clone();
    apply(
        &mut e,
        json!({"op":"gradient","layer":id,"rect":[0,0,9,3],"color":[120,130,140,255]}),
    );
    let p = e.doc.layers[0].pixels.get16(1, 1);
    assert_eq!(p[3], 58253);
    assert_ne!(p[3] % 257, 0);
    assert_eq!(e.state()["document"]["bit_depth"], 16);
}
