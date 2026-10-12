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
