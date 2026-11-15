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
fn levels_sources_version_output_and_exact_mapping_even_when_disabled() {
    for (settings, format) in [
        (json!({"output_black":0.19,"output_white":0.83}), 47),
        (json!({"input_mapping":"exact","black":17./255.,"white":18./255.,"gamma":9.99}), 49),
        (json!({"gamma":9.99}), 49),
        (json!({"input_mapping":"exact","channels":[{"output_black":1.,"output_white":0.},{},{}]}), 50),
        (json!({"gamma":1.8,"channels":[{"gamma":1.2},{},{}]}), 50),
    ] {
        for depth in [8, 16] {
            for target in ["effect.add", "filter.add", "adjustment.add"] {
                let mut engine = Engine::new();
                engine.resident_state_mut().unwrap().document = Document::new_depth(2, 1, depth).unwrap();
                let doc = &mut engine.resident_state_mut().unwrap().document;
                doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
                let layer = doc.layers[0].id.clone();
                engine.edit("human", &[json!({"op":target,"layer":layer,"kind":"levels",
                    "settings":settings})], None, None, "Levels mapping").unwrap();
                for enabled in [true, false] {
                    let doc = &mut engine.resident_state_mut().unwrap().document;
                    for effect in doc.layers.iter_mut().flat_map(|layer| &mut layer.effects).chain(&mut doc.filters) {
                        effect.enabled = enabled;
                    }
                    for bytes in [psd::encode(doc).unwrap(), psd::encode_psb(doc).unwrap()] {
                        assert_eq!(source_format(&bytes), format);
                        let restored = psd::decode(&bytes).unwrap();
                        assert!(!restored.read_only, "{:?}", restored.warnings);
                        assert_eq!(serde_json::to_value(&restored).unwrap(), serde_json::to_value(&*doc).unwrap());
                        assert_eq!(restored.export_png().unwrap(), doc.export_png().unwrap());
                        for format in [15, format as u32 - 1, u32::MAX] {
                            let protected = psd::decode(&with_format(&bytes, format)).unwrap();
                            assert!(protected.read_only);
                            assert_eq!(protected.bit_depth, depth);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn gradient_coverage_requires_format45_and_preserves_native_sources() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(2, 1, depth).unwrap();
        let layer = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
        engine.resident_state_mut().expect("resident fixture").document.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 40001]);
        engine.edit("human", &[json!({"op":"effect.add", "layer":layer, "kind":"gradient_overlay",
            "settings":{"use_gradient_alpha":true,"opacity":0.63,"alpha_stops":[
                {"position":0.,"opacity":0.23},{"position":1.,"opacity":0.87}
            ]}})], None, None, "Gradient coverage").unwrap();
        let doc = &engine.resident_state().expect("resident fixture").document;
        for bytes in [psd::encode(doc).unwrap(), psd::encode_psb(doc).unwrap()] {
            assert_eq!(source_format(&bytes), 45);
            let restored = psd::decode(&bytes).unwrap();
            assert!(!restored.read_only, "{:?}", restored.warnings);
            assert_eq!(restored.layers[0].effects[0].settings, doc.layers[0].effects[0].settings);
            assert_eq!(restored.export_png().unwrap(), doc.export_png().unwrap());
            for format in [27, 44, u32::MAX] {
                let protected = psd::decode(&with_format(&bytes, format)).unwrap();
                assert!(protected.read_only);
                assert_eq!(protected.bit_depth, depth);
            }
        }
    }
}

#[test]
fn standard_invert_layout_requires_format35_and_keeps_native_sources() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(12, 10, depth).unwrap();
        engine.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(5, 5, [12347, 23459, 34571, 65535]);
        engine
            .edit(
                "human",
                &[json!({"op":"adjustment.add","kind":"invert"})],
                None,
                None,
                "Invert",
            )
            .unwrap();
        let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
        for bytes in [
            psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
            psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
        ] {
            assert_eq!(source_format(&bytes), 35);
            let loaded = psd::decode(&bytes).unwrap();
            assert!(!loaded.read_only, "{:?}", loaded.warnings);
            assert_eq!(serde_json::to_value(&loaded).unwrap(), before);
        }
    }
}

#[test]
fn newer_selection_and_curve_sources_are_versioned_and_roundtrip_at_both_depths() {
    for depth in [8, 16] {
        let mut e = Engine::new();
        e.resident_state_mut().expect("resident fixture").document = Document::new_depth(12, 10, depth).unwrap();
        let layer = e.resident_state().expect("resident fixture").document.layers[0].id.clone();
        e.resident_state_mut().expect("resident fixture").document.layers[0]
            .pixels
            .set16(5, 5, [12345, 23456, 34567, 65535]);
        let plain = psd::encode(&e.resident_state().expect("resident fixture").document).unwrap();
        assert_eq!(source_format(&plain), if depth == 16 { 5 } else { 1 });
        e.edit("human", &[
            json!({"op":"effect.add","layer":layer,"kind":"curves","settings":{"points":[[0,0],[0.3,0.7],[1,1]],"interpolation":"smooth"}}),
            json!({"op":"selection","kind":"ellipse","rect":[2,2,10,8],"feather":2}),
        ], None, None, "Smooth selected artwork").unwrap();
        let expected = e.resident_state().expect("resident fixture").document.preview(None, 12, None, false).unwrap().2;
        let saved = psd::encode(&e.resident_state().expect("resident fixture").document).unwrap();
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
            e.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16()
        );
        assert_eq!(
            loaded.layers[0].effects[0].settings,
            e.resident_state().expect("resident fixture").document.layers[0].effects[0].settings
        );
        assert_eq!(
            loaded.selection_coverage.as_ref().unwrap().mask.rgba(),
            e.resident_state().expect("resident fixture").document.selection_coverage.as_ref().unwrap().mask.rgba()
        );
        assert_eq!(loaded.preview(None, 12, None, false).unwrap().2, expected);
        let revision = e.resident_state().expect("resident fixture").document.revision;
        assert!(e.edit("human", &[json!({"op":"effect.update","layer":layer,"effect":e.resident_state().expect("resident fixture").document.layers[0].effects[0].id,"settings":{"points":[[0,0],[1,1]],"interpolation":"unknown"}})], None, None, "Invalid curve").is_err());
        assert_eq!(e.resident_state().expect("resident fixture").document.revision, revision);
    }
}

#[test]
fn text_tracking_requires_new_sources_and_keeps_current_standard_pixels() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(96, 80, depth).unwrap();
        for y in 0..80 {
            for x in 0..96 {
                engine.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(x, y, [30001, 20003, 40007, 65535]);
            }
        }
        engine.edit("human", &[json!({"op":"source.add","x":3,"y":5,"source":{"width":84,"height":68,"content":{"kind":"text","text":"AV\nTitle","font":"regular","size":21,"line_height":1.2,"align":"center","color":[12345,23457,34569,50001]}}})],None,None,"Text").unwrap();
        assert_eq!(source_format(&psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap()), 8);
        let id = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"mask.add","layer":id})],
                None,
                None,
                "Mask",
            )
            .unwrap();
        assert_eq!(source_format(&psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap()), 29);
        let mut source = engine.resident_state().expect("resident fixture").document.layers[0].source.as_deref().cloned().unwrap();
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
        let expected = engine.resident_state().expect("resident fixture").document.export_png().unwrap();
        for bytes in [
            psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
            psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
        ] {
            assert_eq!(
                source_format(&bytes),
                30,
                "Tracking must take precedence over format-29 masks"
            );
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.layers[0].source, engine.resident_state().expect("resident fixture").document.layers[0].source);
            assert_eq!(
                reopened.layers[0].pixels.rgba16(),
                engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16()
            );
            assert_eq!(reopened.export_png().unwrap(), expected);
            for format in [8, 29, u32::MAX] {
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
                engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16()
            );
            assert_eq!(plain.export_png().unwrap(), expected);
        }
    }
}

fn with_format(bytes: &[u8], format: u32) -> Vec<u8> {
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write;
    let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let offset = 30 + color_len;
    let resource_len = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
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

#[test]
fn shaped_layout_requires_format34_and_keeps_current_standard_native_psd_and_psb() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(96, 80, depth).unwrap();
        for y in 0..80 {
            for x in 0..96 {
                engine.resident_state_mut().expect("resident fixture").document.layers[0]
                    .pixels
                    .set16(x, y, [30001, 20003, 40007, 65535]);
            }
        }
        engine.edit("human", &[json!({"op":"source.add","x":3,"y":5,"source":{"width":84,"height":68,"content":{"kind":"text","text":"office a\u{0301}","layout":"shaped","font":"regular","size":21,"line_height":1.2,"align":"center","color":[12345,23457,34569,50001]}}})],None,None,"Shaped source").unwrap();
        let expected = engine.resident_state().expect("resident fixture").document.export_png().unwrap();
        let source = engine.resident_state().expect("resident fixture").document.layers[0].source.clone();
        for bytes in [
            psd::encode(&engine.resident_state().expect("resident fixture").document).unwrap(),
            psd::encode_psb(&engine.resident_state().expect("resident fixture").document).unwrap(),
        ] {
            assert_eq!(source_format(&bytes), 34);
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(reopened.layers[0].source, source);
            assert_eq!(reopened.layers[0].pixels.depth, depth);
            assert_eq!(
                reopened.layers[0].pixels.rgba16(),
                engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16()
            );
            assert_eq!(reopened.export_png().unwrap(), expected);
            for format in [8, 30, 31, 32, 33, u32::MAX] {
                let rejected = psd::decode(&with_format(&bytes, format)).unwrap();
                assert!(rejected.read_only);
                assert!(rejected.layers.iter().all(|layer| layer.source.is_none()));
                assert_eq!(rejected.bit_depth, depth);
                assert_eq!(rejected.export_png().unwrap(), expected);
            }
            let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
            let offset = 30 + color_len;
            let resources =
                u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            let mut standard = bytes[..offset].to_vec();
            standard.extend_from_slice(&0u32.to_be_bytes());
            standard.extend_from_slice(&bytes[offset + 4 + resources..]);
            let standard = psd::decode(&standard).unwrap();
            assert_eq!(standard.bit_depth, depth);
            assert_eq!(standard.layers[0].pixels.depth, depth);
            assert_eq!(
                standard.layers[0].pixels.rgba16(),
                engine.resident_state().expect("resident fixture").document.layers[0].pixels.rgba16()
            );
            assert_eq!(standard.export_png().unwrap(), expected);
        }
    }
}

#[test]
fn paragraph_layout_requires_format128_and_reopens_with_current_native_pixels() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.resident_state_mut().unwrap().document = Document::new_depth(96, 120, depth).unwrap();
        engine.edit("human", &[json!({"op":"source.add","x":3,"y":5,"source":{
            "width":58,"height":100,"content":{"kind":"text","text":"office line wraps",
            "layout":"paragraph","font":"regular","size":21,"line_height":1.2,"align":"center",
            "color":[12345,23457,34569,50001]}}})],None,None,"Paragraph source").unwrap();
        let document = &engine.resident_state().unwrap().document;
        let expected = document.export_png().unwrap();
        for bytes in [psd::encode(document).unwrap(), psd::encode_psb(document).unwrap()] {
            assert_eq!(source_format(&bytes), 128);
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.layers[0].source, document.layers[0].source);
            assert_eq!(reopened.layers[0].pixels.rgba16(), document.layers[0].pixels.rgba16());
            assert_eq!(reopened.export_png().unwrap(), expected);
            let downgraded = psd::decode(&with_format(&bytes, 127)).unwrap();
            assert!(downgraded.read_only);
            assert_eq!(downgraded.bit_depth, depth);
            assert_eq!(downgraded.export_png().unwrap(), expected);
        }
    }
}
