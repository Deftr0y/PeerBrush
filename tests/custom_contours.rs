use peerbrush::{
    effects,
    engine::{Document, Engine},
    psd, server,
};
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

fn points() -> Value {
    json!([{"position":0.,"value":0.},{"position":0.25,"value":0.75},{"position":1.,"value":0.}])
}
fn settings() -> Value {
    json!({"contour":"custom","contour_points":points(),"opacity":1.})
}
fn native(value: u16, depth: u16) -> u16 {
    if depth == 16 {
        value
    } else {
        ((u32::from(value) + 128) / 257) as u16 * 257
    }
}
fn strip(bytes: &[u8]) -> Vec<u8> {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let count = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    [
        bytes[..at].to_vec(),
        vec![0; 4],
        bytes[at + 4 + count..].to_vec(),
    ]
    .concat()
}
fn embedded(bytes: &[u8]) -> Value {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let resources = &bytes[at + 4..at + 4 + size];
    assert_eq!(&resources[..6], b"8BIM\x0f\xa0");
    let length = 6 + (usize::from(resources[6]) + 1).div_ceil(2) * 2;
    let size = u32::from_be_bytes(resources[length..length + 4].try_into().unwrap()) as usize;
    let payload = &resources[length + 4..length + 4 + size];
    assert_eq!(&payload[..4], b"PBR1");
    let mut decoded = Vec::new();
    flate2::read::ZlibDecoder::new(&payload[4..])
        .read_to_end(&mut decoded)
        .unwrap();
    serde_json::from_slice(&decoded).unwrap()
}
fn replace_embedded(bytes: &[u8], value: &Value) -> Vec<u8> {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let resources = &bytes[at + 4..at + 4 + size];
    let length = 6 + (usize::from(resources[6]) + 1).div_ceil(2) * 2;
    let old_size = u32::from_be_bytes(resources[length..length + 4].try_into().unwrap()) as usize;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder
        .write_all(&serde_json::to_vec(value).unwrap())
        .unwrap();
    let payload = [b"PBR1".to_vec(), encoder.finish().unwrap()].concat();
    let mut replacement = resources[..length].to_vec();
    replacement.extend((payload.len() as u32).to_be_bytes());
    replacement.extend(&payload);
    if payload.len() % 2 != 0 {
        replacement.push(0);
    }
    replacement.extend(&resources[length + 4 + old_size.div_ceil(2) * 2..]);
    [
        bytes[..at].to_vec(),
        (replacement.len() as u32).to_be_bytes().to_vec(),
        replacement,
        bytes[at + 4 + size..].to_vec(),
    ]
    .concat()
}
fn standard_composite(bytes: &[u8], depth: u16, width: u32, height: u32) -> Vec<u16> {
    let count = (width * height) as usize;
    let byte_count = count * 4 * usize::from(depth / 8);
    let at = bytes.len() - byte_count;
    assert_eq!(&bytes[at - 2..at], &[0, 0]);
    let words = if depth == 16 {
        bytes[at..]
            .chunks_exact(2)
            .map(|b| u16::from_be_bytes(b.try_into().unwrap()))
            .collect::<Vec<_>>()
    } else {
        bytes[at..].iter().map(|b| u16::from(*b)).collect()
    };
    let max = if depth == 16 { 65535i64 } else { 255 };
    (0..count)
        .flat_map(|i| {
            let alpha = i64::from(words[count * 3 + i]);
            let mut pixel = [0u16; 4];
            pixel[3] = alpha as u16;
            for c in 0..3 {
                pixel[c] = if alpha == 0 {
                    0
                } else {
                    ((i64::from(words[count * c + i]) + alpha - max) * max + alpha / 2)
                        .div_euclid(alpha)
                        .clamp(0, max) as u16
                };
            }
            if depth == 8 {
                pixel = pixel.map(|n| n * 257);
            }
            pixel
        })
        .collect()
}
#[test]
fn contour_mapping_retains_native_piecewise_endpoints_and_nonmonotone_values() {
    let curve = effects::contours::Curve::decode(&points()).unwrap();
    for (input, expected) in [
        (-1., 0.),
        (0., 0.),
        (0.125, 0.375),
        (0.25, 0.75),
        (0.5, 0.5),
        (1., 0.),
        (2., 0.),
    ] {
        assert_eq!(curve.sample(input), expected);
    }
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(4, 1, depth).unwrap();
        let levels = [10000u16, 20000, 40000, 50000];
        for (x, level) in levels.into_iter().enumerate() {
            engine.doc.layers[0]
                .pixels
                .set16(x as i32, 0, [level, level, level, 65535]);
        }
        let layer = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"contour","settings":settings()})],
                None,
                None,
                "Native contour",
            )
            .unwrap();
        let rendered = if depth == 16 {
            peerbrush::depth16::render(&engine.doc).unwrap().words
        } else {
            engine
                .doc
                .preview(None, 4, None, false)
                .unwrap()
                .2
                .into_iter()
                .map(|n| u16::from(n) * 257)
                .collect()
        };
        for (x, level) in levels.into_iter().enumerate() {
            let level = native(level, depth);
            let mapped = native(
                if level < 16384 {
                    level * 3
                } else {
                    65535 - level
                },
                depth,
            );
            assert_eq!(
                &rendered[x * 4..x * 4 + 4],
                &[mapped, mapped, mapped, 65535]
            );
        }
    }
}
#[test]
fn malformed_or_inapplicable_contours_rollback_pixels_and_history_atomically() {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(4, 4, 16).unwrap();
    let layer = engine.doc.layers[0].id.clone();
    let before = serde_json::to_value(&engine.doc).unwrap();
    let mut invalid = vec![
        json!(null),
        json!([]),
        json!([{"position":0.,"value":0.}]),
        json!([{"position":0.,"value":0.},{"position":0.,"value":1.}]),
        json!([{"position":0.1,"value":0.},{"position":1.,"value":1.}]),
        json!([{"position":0.,"value":0.},{"position":0.9,"value":1.}]),
        json!([{"position":0.,"value":0.},{"position":1.,"value":2.}]),
        json!([{"position":0.,"value":0.,"extra":true},{"position":1.,"value":1.}]),
        json!([{"position":0.,"value":0.},{"position":1.}]),
    ];
    invalid.push(json!((0..17)
        .map(|i| json!({"position":i as f64/16.,"value":0.5}))
        .collect::<Vec<_>>()));
    for bad in invalid {
        assert!(engine.edit("human",&[
            json!({"op":"paint.fill","layer":layer,"rect":[0,0,1,1],"color":[255,0,0,255]}),
            json!({"op":"effect.add","layer":layer,"kind":"contour","settings":{"contour":"custom","contour_points":bad}})
        ],None,None,"Invalid contour").is_err());
        assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
        assert!(engine.undo.is_empty());
    }
    for (kind, value) in [
        ("contour", json!({"contour":"custom"})),
        (
            "contour",
            json!({"contour":"linear","contour_points":points()}),
        ),
        ("pattern_overlay", settings()),
        ("levels", settings()),
    ] {
        assert!(effects::validate(kind, &value).is_err(), "{kind}");
    }
}
fn fixture(depth: u16) -> Engine {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(32, 24, depth).unwrap();
    for y in 4..20 {
        for x in 4..28 {
            engine.doc.layers[0].pixels.set16(
                x,
                y,
                [
                    12347 + x as u16 * 211,
                    23459 + y as u16 * 193,
                    34571,
                    if x % 3 == 0 { 41317 } else { 65535 },
                ],
            );
        }
    }
    engine
}
#[test]
fn every_custom_style_contour_retains_sources_preview_undo_and_current_psd_psb_outputs() {
    for depth in [8, 16] {
        for kind in [
            "contour",
            "bevel_emboss",
            "inner_shadow",
            "inner_glow",
            "satin",
            "outer_glow",
            "drop_shadow",
        ] {
            let mut engine = fixture(depth);
            let layer = engine.doc.layers[0].id.clone();
            let before = engine.doc.export_png().unwrap();
            let source = engine.doc.layers[0].pixels.rgba16();
            let custom = json!({"contour":"custom","contour_points":[{"position":0.,"value":0.1},{"position":0.4,"value":0.8},{"position":1.,"value":0.2}]});
            let command = json!({"op":"effect.add","layer":layer,"kind":kind,"settings":custom});
            let preview =
                Engine::preview_edits(engine.doc.clone(), std::slice::from_ref(&command)).unwrap();
            engine
                .edit("human", &[command], None, None, "Custom style contour")
                .unwrap();
            let current = engine.doc.export_png().unwrap();
            assert_eq!(current, preview.export_png().unwrap());
            assert_ne!(current, before, "{depth} {kind}");
            assert_eq!(engine.undo.len(), 1);
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
            for output in [
                psd::encode(&engine.doc).unwrap(),
                psd::encode_psb(&engine.doc).unwrap(),
            ] {
                assert_eq!(
                    embedded(&output)["format"],
                    if kind == "bevel_emboss" { 22 } else { 21 }
                );
                let reopened = psd::decode(&output).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                assert_eq!(reopened.export_png().unwrap(), current);
                assert_eq!(
                    reopened.layers[0].effects[0].settings["contour_points"],
                    custom["contour_points"]
                );
            }
            let standard = psd::decode(&strip(&psd::encode(&engine.doc).unwrap())).unwrap();
            assert!(!standard.read_only);
            assert!(standard.layers[0].effects.is_empty());
            assert_eq!(standard.export_png().unwrap(), current);
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), before);
            engine.redo("human").unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), current);
        }
    }
}
#[test]
fn future_and_malformed_contour_sources_preserve_native_composites_and_disabled_definitions() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let layer = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"contour","settings":settings()})],
                None,
                None,
                "Contour source",
            )
            .unwrap();
        let effect = engine.doc.layers[0].effects[0].id.clone();
        for disabled in [false, true] {
            engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":!disabled})],None,None,"Contour bypass").unwrap();
            for bytes in [
                psd::encode(&engine.doc).unwrap(),
                psd::encode_psb(&engine.doc).unwrap(),
            ] {
                let source = embedded(&bytes);
                assert_eq!(source["format"], 21);
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.layers[0].effects[0].enabled, !disabled);
                assert_eq!(
                    reopened.layers[0].effects[0].settings["contour_points"],
                    points()
                );
                for malformed in [false, true] {
                    let mut source = source.clone();
                    if malformed {
                        source["document"]["layers"][0]["effects"][0]["settings"]
                            ["contour_points"][1]["position"] = json!(0.);
                    } else {
                        source["format"] = json!(25);
                    }
                    let protected = psd::decode(&replace_embedded(&bytes, &source)).unwrap();
                    assert!(protected.read_only);
                    assert_eq!(protected.bit_depth, depth);
                    assert_eq!(
                        protected.layers[0].pixels.rgba16(),
                        standard_composite(&bytes, depth, 32, 24)
                    );
                }
            }
        }
    }
}
#[test]
fn shared_custom_contour_feedback_and_reservations_preserve_human_native_work() {
    let engine = fixture(16);
    let layer = engine.doc.layers[0].id.clone();
    let document = engine.doc.id.clone();
    let source = engine.doc.layers[0].pixels.rgba16();
    let shared = Arc::new(Mutex::new(engine));
    let result=server::dispatch(&shared,"edit",&json!({"actor":"human","document_id":document,"expected_revision":0,"commands":[{"op":"effect.add","layer":layer,"kind":"contour","settings":settings()}],"feedback":"always","max_edge":64})).unwrap();
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 32, 24]));
    assert!(result["images"][0]["data"].as_str().unwrap().len() > 20);
    let mut engine = shared.lock().unwrap();
    assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
    engine
        .reserve(
            "assistant",
            "Synthetic contour reservation",
            vec![peerbrush::engine::Scope {
                target: Some(layer.clone()),
                rect: None,
            }],
        )
        .unwrap();
    let before = serde_json::to_value(&engine.doc).unwrap();
    let history = engine.undo.len();
    assert!(engine
        .edit(
            "human",
            &[json!({"op":"effect.add","layer":layer,"kind":"contour","settings":settings()})],
            None,
            None,
            "Reserved contour"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
    assert_eq!(engine.undo.len(), history);
}
