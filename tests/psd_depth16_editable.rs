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
