use flate2::read::ZlibDecoder;
use peerbrush::{
    engine::{Document, Engine},
    psd,
};
use serde_json::{json, Value};
use std::io::Read;

fn source_format(bytes: &[u8]) -> u64 {
    let start = bytes.windows(4).position(|w| w == b"PBR1").unwrap() + 4;
    let mut json = Vec::new();
    ZlibDecoder::new(&bytes[start..])
        .read_to_end(&mut json)
        .unwrap();
    serde_json::from_slice::<Value>(&json).unwrap()["format"]
        .as_u64()
        .unwrap()
}

#[test]
fn newer_selection_and_curve_sources_are_versioned_and_roundtrip_at_both_depths() {
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.doc = Document::new_depth(12, 10, depth).unwrap();
        let layer = e.doc.layers[0].id.clone();
        e.doc.layers[0]
            .pixels
            .set16(5, 5, [12345, 23456, 34567, 65535]);
        let plain = psd::encode(&e.doc).unwrap();
        assert_eq!(source_format(&plain), if depth == 16 { 5 } else { 1 });
        e.edit("human", &[
            json!({"op":"effect.add","layer":layer,"kind":"curves","settings":{"points":[[0,0],[0.3,0.7],[1,1]],"interpolation":"smooth"}}),
            json!({"op":"selection","kind":"ellipse","rect":[2,2,10,8],"feather":2}),
        ], None, None, "Smooth selected artwork").unwrap();
        let expected = e.doc.preview(None, 12, None, false).unwrap().2;
        let saved = psd::encode(&e.doc).unwrap();
        assert_eq!(
            source_format(&saved),
            6,
            "Older readers must reject these source semantics"
        );
        let loaded = psd::decode(&saved).unwrap();
        assert!(!loaded.read_only);
        assert_eq!(loaded.bit_depth, depth);
        assert_eq!(
            loaded.layers[0].pixels.rgba16(),
            e.doc.layers[0].pixels.rgba16()
        );
        assert_eq!(
            loaded.layers[0].effects[0].settings,
            e.doc.layers[0].effects[0].settings
        );
        assert_eq!(
            loaded.selection_coverage.as_ref().unwrap().mask.rgba(),
            e.doc.selection_coverage.as_ref().unwrap().mask.rgba()
        );
        assert_eq!(loaded.preview(None, 12, None, false).unwrap().2, expected);
        let revision = e.doc.revision;
        assert!(e.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":e.doc.layers[0].effects[0].id,"settings":{"points":[[0,0],[1,1]],"interpolation":"unknown"}})], None, None, "Invalid curve").is_err());
        assert_eq!(e.doc.revision, revision);
    }
}
