use peerbrush::{
    engine::{Document, Engine},
    raster::blend,
    server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
#[test]
fn additive_and_new_blends_obey_alpha_and_psd_keys() {
    assert_eq!(
        blend([100, 40, 0, 255], [90, 100, 0, 255], 1., "linear_dodge"),
        [190, 140, 0, 255]
    );
    assert_eq!(
        blend([0; 4], [90, 100, 70, 128], 1., "linear_dodge"),
        [90, 100, 70, 128]
    );
    assert_eq!(
        blend([100, 40, 0, 255], [90, 100, 0, 0], 1., "linear_dodge"),
        [100, 40, 0, 255]
    );
    let mut e = Engine::new();
    e.doc = Document::new(3, 3).unwrap();
    let id = e.doc.layers[0].id.clone();
    for &(mode, _) in peerbrush::raster::BLENDS {
        e.edit(
            "human",
            &[json!({"op":"layer.update","layer":id,"blend":mode})],
            None,
            None,
            "blend",
        )
        .unwrap();
        e.doc.layers[0].pixels.set(1, 1, [95, 170, 15, 128]);
        let decoded = peerbrush::psd::decode(&peerbrush::psd::encode(&e.doc).unwrap()).unwrap();
        assert_eq!(decoded.layers[0].blend, mode);
        assert_eq!(
            decoded.preview(None, 3, None, false).unwrap().2,
            e.doc.preview(None, 3, None, false).unwrap().2
        );
    }
}
#[test]
fn explicit_compatible_copy_preserves_original_and_becomes_unsaved_editable_artwork() {
    let mut e = Engine::new();
    e.doc = Document::new(4, 4).unwrap();
    e.doc.read_only = true;
    e.doc.name = "16-bit.psd".into();
    e.doc.layers[0].pixels.set(1, 1, [25, 200, 70, 128]);
    e.path = Some("original.psd".into());
    let shared = Arc::new(Mutex::new(e));
    assert!(server::compatible_copy(&shared, "human", Some(2)).is_err());
    assert!(shared.lock().unwrap().doc.read_only);
    server::compatible_copy(&shared, "human", Some(0)).unwrap();
    let e = shared.lock().unwrap();
    assert!(!e.doc.read_only);
    assert!(e.path.is_none());
    assert_ne!(e.doc.revision, e.saved_revision);
    assert_eq!(e.doc.layers[0].pixels.get(1, 1), [25, 200, 70, 128]);
}
