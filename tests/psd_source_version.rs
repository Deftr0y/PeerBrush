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

#[test]
fn text_tracking_requires_new_sources_and_keeps_current_standard_pixels() {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    fn with_format(bytes: &[u8], format: u32) -> Vec<u8> {
        let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
        let offset = 30 + color_len;
        let resource_len =
            u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let resources = &bytes[offset + 4..offset + 4 + resource_len];
        let marker = resources
            .windows(4)
            .position(|value| value == b"PBR1")
            .unwrap();
        let payload_len =
            u32::from_be_bytes(resources[marker - 4..marker].try_into().unwrap()) as usize;
        let mut decoded = Vec::new();
        ZlibDecoder::new(&resources[marker + 4..marker + payload_len])
            .read_to_end(&mut decoded)
            .unwrap();
        let mut embedded: Value = serde_json::from_slice(&decoded).unwrap();
        embedded["format"] = json!(format);
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::fast());
        serde_json::to_writer(&mut encoder, &embedded).unwrap();
        let mut payload = b"PBR1".to_vec();
        payload.write_all(&encoder.finish().unwrap()).unwrap();
        let mut replacement = resources[..marker - 4].to_vec();
        replacement.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        replacement.extend_from_slice(&payload);
        if payload.len() % 2 != 0 {
            replacement.push(0);
        }
        replacement.extend_from_slice(&resources[marker + payload_len + payload_len % 2..]);
        let mut output = bytes[..offset].to_vec();
        output.extend_from_slice(&(replacement.len() as u32).to_be_bytes());
        output.extend_from_slice(&replacement);
        output.extend_from_slice(&bytes[offset + 4 + resource_len..]);
        output
    }
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(96, 80, depth).unwrap();
        for y in 0..80 {
            for x in 0..96 {
                engine.doc.layers[0]
                    .pixels
                    .set16(x, y, [30001, 20003, 40007, 65535]);
            }
        }
        engine.edit("human", &[json!({"op":"source.add","x":3,"y":5,"source":{"width":84,"height":68,"content":{"kind":"text","text":"AV\nTitle","font":"regular","size":21,"line_height":1.2,"align":"center","color":[12345,23457,34569,50001]}}})],None,None,"Text").unwrap();
        assert_eq!(source_format(&psd::encode(&engine.doc).unwrap()), 8);
        let id = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"mask.add","layer":id})],
                None,
                None,
                "Mask",
            )
            .unwrap();
        assert_eq!(source_format(&psd::encode(&engine.doc).unwrap()), 29);
        let mut source = engine.doc.layers[0].source.clone().unwrap();
        if let peerbrush::source::Content::Text { letter_spacing, .. } = &mut source.content {
            *letter_spacing = 2.25;
        }
        engine
            .edit(
                "human",
                &[json!({"op":"source.update","layer":id,"source":source})],
                None,
                None,
                "Tracking",
            )
            .unwrap();
        let expected = engine.doc.export_png().unwrap();
        for bytes in [
            psd::encode(&engine.doc).unwrap(),
            psd::encode_psb(&engine.doc).unwrap(),
        ] {
            assert_eq!(
                source_format(&bytes),
                30,
                "Tracking must take precedence over format-29 masks"
            );
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.layers[0].source, engine.doc.layers[0].source);
            assert_eq!(
                reopened.layers[0].pixels.rgba16(),
                engine.doc.layers[0].pixels.rgba16()
            );
            assert_eq!(reopened.export_png().unwrap(), expected);
            for format in [8, 29, 31] {
                let rejected = psd::decode(&with_format(&bytes, format)).unwrap();
                assert!(rejected.read_only);
                assert!(rejected.layers.iter().all(|layer| layer.source.is_none()));
                assert_eq!(rejected.export_png().unwrap(), expected);
            }
            let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
            let offset = 30 + color_len;
            let resource_len =
                u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let mut standard = bytes[..offset].to_vec();
            standard.extend_from_slice(&0u32.to_be_bytes());
            standard.extend_from_slice(&bytes[offset + 4 + resource_len..]);
            let plain = psd::decode(&standard).unwrap();
            assert_eq!(
                plain.layers[0].pixels.rgba16(),
                engine.doc.layers[0].pixels.rgba16()
            );
            assert_eq!(plain.export_png().unwrap(), expected);
        }
    }
}
