use peerbrush::{
    engine::{self, Document, Layer, Mask, MaskStep},
    psd,
    raster::Raster,
};
use serde_json::json;

fn fixture() -> Document {
    let mut doc = Document::new(3, 2).unwrap();
    doc.bit_depth = 16;
    let mut layer = Layer::new("High precision", "paint", 3, 2);
    layer.pixels = Raster::from_rgba16(
        3,
        2,
        &[
            12345, 23456, 34567, 65535, 12346, 23457, 34568, 65535, 10001, 20001, 30001, 32768,
            123, 234, 345, 1, 91, 103, 127, 0, 60001, 50001, 40001, 45678,
        ],
    )
    .unwrap();
    let mut paint = Raster::new_depth(3, 2, 16);
    paint.set16(1, 0, [45679, 45679, 45679, 65535]);
    layer.mask = Some(Mask {
        linked: true,
        enabled: false,
        cache_key: engine::id(),
        steps: vec![
            MaskStep {
                weight: 1.0,
                id: engine::id(),
                kind: "fill".into(),
                enabled: true,
                value: 255.,
                pixels: Raster::new_depth(3, 2, 16),
                settings: json!(null),
            },
            MaskStep {
                weight: 1.0,
                id: engine::id(),
                kind: "paint".into(),
                enabled: true,
                value: 0.,
                pixels: paint,
                settings: json!(null),
            },
        ],
    });
    doc.layers = vec![layer];
    doc
}
fn remove_resources(bytes: &[u8]) -> Vec<u8> {
    let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let pos = 30 + color_len;
    let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    let mut out = bytes[..pos].to_vec();
    out.extend([0; 4]);
    out.extend(&bytes[pos + 4 + len..]);
    out
}

fn editable_record(bytes: &[u8]) -> serde_json::Value {
    use std::io::Read;
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let resources = &bytes[at + 4..at + 4 + size];
    assert_eq!(&resources[..6], b"8BIM\x0f\xa0");
    let length = 6 + (usize::from(resources[6]) + 1).div_ceil(2) * 2;
    let size = u32::from_be_bytes(resources[length..length + 4].try_into().unwrap()) as usize;
    let payload = &resources[length + 4..length + 4 + size];
    assert_eq!(&payload[..4], b"PBR1");
    let mut json = Vec::new();
    flate2::read::ZlibDecoder::new(&payload[4..])
        .read_to_end(&mut json)
        .unwrap();
    serde_json::from_slice(&json).unwrap()
}
fn replace_editable_record(bytes: &[u8], value: &serde_json::Value) -> Vec<u8> {
    use std::io::Write;
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

#[test]
fn installed_text_format_31_retains_native_standard_channels_masks_composite_and_protection() {
    use peerbrush::{
        fonts::{InstalledFont, TextFont},
        source::{Content, Source},
    };
    for depth in [8, 16] {
        let mut doc = Document::new_depth(80, 52, depth).unwrap();
        for y in 0..52 {
            for x in 0..80 {
                doc.layers[0]
                    .pixels
                    .set16(x, y, [17001, 29003, 41007, 65535]);
            }
        }
        let mut source = Source {
            width: 76,
            height: 38,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Text {
                text: "Native text".into(),
                layout: peerbrush::source::TextLayout::Legacy,
                font: "regular".into(),
                size: 17.,
                line_height: 1.2,
                letter_spacing: 0.,
                align: "left".into(),
                color: [12501, 23003, 34507, 65535],
            },
        };
        let mut layer = Layer::new("Absent installed font", "paint", 76, 38);
        layer.pixels = source.render(76, 38, depth).unwrap();
        layer.x = 2;
        layer.y = 3;
        if let Content::Text { font, .. } = &mut source.content {
            *font = TextFont::Installed(InstalledFont {
                family: "Synthetic absent font".into(),
                postscript: "PeerBrushSynthetic-Absent".into(),
                style: "normal".into(),
                weight: 400,
                stretch: 5,
                index: 0,
                sha256: "0".repeat(64),
            });
        }
        source.validate_structure().unwrap();
        layer.source = Some(source.clone());
        let mut mask = Raster::new_depth(76, 38, depth);
        mask.set16(4, 7, [45679, 45679, 45679, 65535]);
        layer.mask = Some(Mask {
            linked: false,
            enabled: true,
            cache_key: engine::id(),
            steps: vec![
                MaskStep {
                    id: engine::id(),
                    kind: "fill".into(),
                    enabled: true,
                    weight: 1.,
                    value: 255.,
                    pixels: Raster::new_depth(76, 38, depth),
                    settings: json!(null),
                },
                MaskStep {
                    id: engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 0.,
                    pixels: mask,
                    settings: json!(null),
                },
            ],
        });
        let id = layer.id.clone();
        let native = layer.pixels.rgba16();
        let mask = layer.mask.clone();
        doc.layers.insert(0, layer);
        let before = json!(doc);
        let current = doc.export_png().unwrap();
        let composite = image::load_from_memory(&current)
            .unwrap()
            .into_rgba16()
            .into_raw();
        for bytes in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let embedded = editable_record(&bytes);
            assert_eq!(embedded["format"], 31);
            assert_eq!(
                serde_json::from_value::<peerbrush::source::Source>(
                    embedded["document"]["layers"][0]["source"].clone()
                )
                .unwrap(),
                source
            );
            assert!(
                embedded["document"]["layers"][0]["source"]["content"]["font"]
                    .get("path")
                    .is_none()
            );
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(reopened.layers[0].id, id);
            assert_eq!(reopened.layers[0].source, Some(source.clone()));
            assert_eq!(reopened.layers[0].pixels.rgba16(), native);
            assert_eq!(json!(reopened.layers[0].mask), json!(mask));
            assert_eq!(reopened.export_png().unwrap(), current);
            let standard = psd::decode(&remove_resources(&bytes)).unwrap();
            assert!(!standard.read_only);
            assert!(standard.layers.iter().all(|layer| layer.source.is_none()));
            assert_eq!(standard.layers[0].pixels.rgba16(), native);
            assert_eq!(standard.export_png().unwrap(), current);
            for case in [
                "old_format",
                "future_format",
                "bad_reference",
                "hash_mismatch",
            ] {
                let mut changed = embedded.clone();
                match case {
                    "old_format" => changed["format"] = json!(30),
                    "future_format" => changed["format"] = json!(u32::MAX),
                    "bad_reference" => {
                        changed["document"]["layers"][0]["source"]["content"]["font"]["sha256"] =
                            json!("invalid")
                    }
                    _ => {
                        changed["standard_hash"] =
                            json!(changed["standard_hash"].as_u64().unwrap() ^ 1)
                    }
                }
                let protected = psd::decode(&replace_editable_record(&bytes, &changed)).unwrap();
                assert!(protected.read_only, "{case}");
                assert_eq!(protected.bit_depth, depth);
                assert!(protected.layers.iter().all(|layer| layer.source.is_none()));
                assert_eq!(protected.layers[0].pixels.rgba16(), composite, "{case}");
                assert_eq!(protected.export_png().unwrap(), current, "{case}");
            }
        }
        assert_eq!(json!(doc), before);
    }
}

#[test]
fn genuine_sixteen_bit_psd_sources_standard_channels_masks_and_composite_survive_saving() {
    let doc = fixture();
    let raw = doc.layers[0].pixels.rgba16();
    let bytes = psd::encode(&doc).unwrap();
    assert_eq!(&bytes[22..24], &16u16.to_be_bytes());
    let loaded = psd::decode(&bytes).unwrap();
    assert!(!loaded.read_only);
    assert_eq!(loaded.bit_depth, 16);
    assert_eq!(loaded.layers[0].pixels.rgba16(), raw);
    assert_eq!(
        loaded.layers[0].mask.as_ref().unwrap().steps[1]
            .pixels
            .get16(1, 0),
        [45679, 45679, 45679, 65535]
    );
    // An independent editor sees ordinary16-bit layer channels, not merely private PeerBrush data.
    let standard = psd::decode(&remove_resources(&bytes)).unwrap();
    assert!(!standard.read_only);
    assert_eq!(standard.bit_depth, 16);
    assert_eq!(standard.layers[0].pixels.rgba16(), raw);
    assert!(!standard.layers[0].mask.as_ref().unwrap().enabled);
    assert_eq!(
        standard.layers[0].mask.as_ref().unwrap().steps[1]
            .pixels
            .get16(1, 0),
        [45679, 45679, 45679, 65535]
    );
    assert!(standard.layers[0].pixels.tiles.is_empty());
}

#[test]
fn sixteen_bit_layer_records_are_read_from_lr16_with_lower_samples_intact() {
    let doc = fixture();
    let bytes = remove_resources(&psd::encode(&doc).unwrap());
    let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let pos = 30 + color_len;
    let res_len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    let layer_pos = pos + 4 + res_len;
    assert_eq!(&bytes[layer_pos + 4..layer_pos + 8], &[0; 4]);
    assert!(bytes.windows(8).any(|v| v == b"8BIMLr16"));
    let loaded = psd::decode(&bytes).unwrap();
    assert_eq!(
        loaded.layers[0].pixels.get16(0, 0),
        [12345, 23456, 34567, 65535]
    );
    assert_eq!(
        loaded.layers[0].pixels.get16(1, 0),
        [12346, 23457, 34568, 65535]
    );
    assert_eq!(
        loaded.layers[0].pixels.get(0, 0),
        loaded.layers[0].pixels.get(1, 0)
    );
}

#[test]
fn native_float_effect_sources_retain_exact_json_values_save_reopen_and_undo() {
    use peerbrush::engine::Engine;
    // These normalized descriptor values reproduced one-ULP drift with the
    // default best-effort JSON parser. Include adjacent native rounding edges.
    let values = [
        41.937f64 / 100.,
        53.193 / 255.,
        37.153 / 255.,
        211.237 / 255.,
        0.5,
        f64::from_bits(0.5f64.to_bits() - 1),
        f64::from_bits(0.5f64.to_bits() + 1),
        (12345.5 / 65535.),
    ];
    let encoded = serde_json::to_vec(&values).unwrap();
    let parsed: Vec<f64> = serde_json::from_slice(&encoded).unwrap();
    for (before, after) in values.into_iter().zip(parsed) {
        assert_eq!(before.to_bits(), after.to_bits());
    }
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(3, 2, depth).unwrap();
        engine.doc.layers[0].pixels = fixture().layers[0].pixels.clone();
        if depth == 8 {
            engine.doc.layers[0].pixels =
                Raster::from_rgba(3, 2, &engine.doc.layers[0].pixels.rgba()).unwrap();
        }
        let layer = engine.doc.layers[0].id.clone();
        let original = engine.doc.layers[0].pixels.rgba16();
        let baseline = engine.doc.export_png().unwrap();
        let settings = json!({
            "opacity":values[0],
            "color":[values[1],values[2],values[3]],
            "color2":[values[4],values[5],values[6]],
            "color_stops":[{"position":0.,"midpoint":values[7],"color":[values[1],values[2],values[3]]},{"position":1.,"color":[values[4],values[5],values[6]]}],
            "alpha_stops":[{"position":0.,"opacity":values[0]},{"position":1.,"opacity":values[5]}]
        });
        // Use the same JSON command boundary as MCP/CLI, then the engine's
        // guarded command path; no direct effect-state mutation is involved.
        let command =
            json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":settings});
        let wire = serde_json::to_vec(&command).unwrap();
        let command: serde_json::Value = serde_json::from_slice(&wire).unwrap();
        assert_eq!(command["settings"], settings);
        engine
            .edit("human", &[command], None, None, "Native float gradient")
            .unwrap();
        for (key, value) in settings.as_object().unwrap() {
            assert_eq!(&engine.doc.layers[0].effects[0].settings[key], value);
        }
        let settings = engine.doc.layers[0].effects[0].settings.clone();
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), original);
        let current = engine.doc.export_png().unwrap();
        assert_ne!(current, baseline);
        for bytes in [
            psd::encode(&engine.doc).unwrap(),
            psd::encode_psb(&engine.doc).unwrap(),
        ] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(reopened.layers[0].effects[0].settings, settings);
            assert_eq!(reopened.layers[0].pixels.rgba16(), original);
            assert_eq!(reopened.export_png().unwrap(), current);
            let standard = psd::decode(&remove_resources(&bytes)).unwrap();
            assert!(!standard.read_only);
            assert_eq!(standard.export_png().unwrap(), current);
        }
        assert_eq!(engine.undo.len(), 1);
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), original);
        assert_eq!(engine.doc.export_png().unwrap(), baseline);
    }
}
