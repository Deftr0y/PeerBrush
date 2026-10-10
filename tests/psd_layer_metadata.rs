use peerbrush::{
    engine::{Document, Engine, Layer},
    psd::{self, LayerMetadata},
    raster::Raster,
};
use serde_json::json;

fn timestamp(copy: bool) -> Vec<u8> {
    let mut descriptor = vec![];
    descriptor.extend(16u32.to_be_bytes());
    descriptor.extend(1u32.to_be_bytes());
    descriptor.extend(0u16.to_be_bytes());
    descriptor.extend(8u32.to_be_bytes());
    descriptor.extend(b"metadata");
    descriptor.extend(1u32.to_be_bytes());
    descriptor.extend(9u32.to_be_bytes());
    descriptor.extend(b"layerTime");
    descriptor.extend(b"doub");
    descriptor.extend(1720591532.2346468f64.to_be_bytes());
    descriptor.resize((descriptor.len() + 3) & !3, 0);
    let mut out = 1u32.to_be_bytes().to_vec();
    out.extend(b"8BIMcust");
    out.extend([u8::from(copy), 0, 0, 0]);
    out.extend((descriptor.len() as u32).to_be_bytes());
    out.extend(descriptor);
    out
}
fn metadata() -> Vec<LayerMetadata> {
    let mut point = 1.25f64.to_be_bytes().to_vec();
    point.extend((-3.5f64).to_be_bytes());
    vec![
        LayerMetadata {
            key: "fxrp".into(),
            data: point,
        },
        LayerMetadata {
            key: "lclr".into(),
            data: vec![0, 4, 0, 0, 0, 0, 0, 0],
        },
        LayerMetadata {
            key: "lnsr".into(),
            data: b"layr".to_vec(),
        },
        LayerMetadata {
            key: "shmd".into(),
            data: timestamp(false),
        },
    ]
}
fn fixture(depth: u16) -> Document {
    let mut doc = Document::new_depth(2, 1, depth).unwrap();
    let mut layer = Layer::new("Preserved label", "paint", 2, 1);
    layer.pixels = if depth == 16 {
        Raster::from_rgba16(
            2,
            1,
            &[12345, 23456, 34567, 65535, 12346, 23457, 34568, 65535],
        )
        .unwrap()
    } else {
        Raster::from_rgba(2, 1, &[19, 52, 93, 255, 22, 65, 107, 255]).unwrap()
    };
    layer.psd_metadata = metadata();
    doc.layers = vec![layer];
    doc
}
fn ordinary(bytes: &[u8]) -> Vec<u8> {
    let color_len = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let pos = 30 + color_len;
    let len = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    let mut out = bytes[..pos].to_vec();
    out.extend([0; 4]);
    out.extend(&bytes[pos + 4 + len..]);
    out
}
fn payload_range(bytes: &[u8], key: &[u8; 4]) -> std::ops::Range<usize> {
    let mut signature = b"8BIM".to_vec();
    signature.extend(key);
    let at = bytes
        .windows(8)
        .position(|v| v == signature.as_slice())
        .unwrap();
    let length = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
    at + 12..at + 12 + length
}

#[test]
fn standard_full_layer_locks_survive_without_private_sources_and_use_shared_edit_guards() {
    for depth in [8, 16] {
        let mut original = fixture(depth);
        original.layers[0].locked = true;
        let source = original.layers[0].pixels.rgba16();
        let standard = ordinary(&psd::encode(&original).unwrap());
        assert_eq!(
            &standard[payload_range(&standard, b"lspf")],
            &7u32.to_be_bytes()
        );
        let loaded = psd::decode(&standard).unwrap();
        assert!(!loaded.read_only, "{:?}", loaded.warnings);
        assert!(loaded.layers[0].locked);
        assert_eq!(loaded.layers[0].pixels.rgba16(), source);
        let mut engine = Engine::new();
        engine.doc = loaded;
        let layer = engine.doc.layers[0].id.clone();
        for actor in ["human", "agent"] {
            assert!(engine
                .edit(
                    actor,
                    &[json!({"op":"layer.delete","layer":layer})],
                    None,
                    None,
                    "Protected source"
                )
                .is_err());
            assert!(engine
                .edit(
                    actor,
                    &[json!({"op":"layer.update","layer":layer,"opacity":0.5})],
                    None,
                    None,
                    "Protected source"
                )
                .is_err());
        }
        assert_eq!(engine.doc.revision, 0);
        engine
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"locked":false})],
                None,
                None,
                "Unlock standard layer",
            )
            .unwrap();
        assert!(!engine.doc.layers[0].locked);
        let unlocked = psd::decode(&ordinary(&psd::encode(&engine.doc).unwrap())).unwrap();
        assert!(!unlocked.layers[0].locked);
        engine.undo("human").unwrap();
        assert!(engine.doc.layers[0].locked);
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
        let restored = psd::decode(&ordinary(&psd::encode(&engine.doc).unwrap())).unwrap();
        assert!(restored.layers[0].locked);
    }
}

#[test]
fn standard_locked_folders_protect_children_and_partial_or_unknown_flags_remain_read_only() {
    for depth in [8, 16] {
        let mut original = fixture(depth);
        let mut folder = Layer::new("Locked folder", "group", 2, 1);
        folder.pixels = Raster::new_depth(2, 1, depth);
        folder.locked = true;
        original.layers[0].parent = Some(folder.id.clone());
        original.layers.insert(0, folder);
        let standard = ordinary(&psd::encode(&original).unwrap());
        let loaded = psd::decode(&standard).unwrap();
        assert!(!loaded.read_only, "{:?}", loaded.warnings);
        assert!(loaded.layers[0].locked);
        assert!(!loaded.layers[1].locked);
        assert_eq!(loaded.layers[1].parent.as_ref(), Some(&loaded.layers[0].id));
        let mut engine = Engine::new();
        engine.doc = loaded;
        assert!(engine
            .edit(
                "human",
                &[json!({"op":"layer.delete","layer":engine.doc.layers[1].id})],
                None,
                None,
                "Locked ancestor"
            )
            .is_err());
        let range = payload_range(&standard, b"lspf");
        for flag in [0u32, 1, 2, 4, 8, 0x80000000] {
            let mut partial = standard.clone();
            partial[range.clone()].copy_from_slice(&flag.to_be_bytes());
            let protected = psd::decode(&partial).unwrap();
            assert!(protected.read_only, "flag {flag}: {:?}", protected.warnings);
            assert_eq!(protected.bit_depth, depth);
            assert_eq!(
                protected.export_png().unwrap(),
                original.export_png().unwrap()
            );
        }
    }
}

#[test]
fn benign_standard_metadata_keeps_raster_layers_editable_at_both_depths_and_survives_edits() {
    for depth in [8, 16] {
        let original = fixture(depth);
        let standard = ordinary(&psd::encode(&original).unwrap());
        let loaded = psd::decode(&standard).unwrap();
        assert!(!loaded.read_only, "{:?}", loaded.warnings);
        assert_eq!(loaded.layers.len(), 1);
        assert_eq!(loaded.layers[0].psd_metadata, metadata());
        assert_eq!(
            loaded.layers[0].pixels.rgba16(),
            original.layers[0].pixels.rgba16()
        );
        let mut engine = Engine::new();
        engine.doc = loaded;
        let layer = engine.doc.layers[0].id.clone();
        engine
            .edit(
                "human",
                &[json!({"op":"layer.update","layer":layer,"opacity":0.5})],
                None,
                None,
                "Preserve static metadata",
            )
            .unwrap();
        let saved = ordinary(&psd::encode(&engine.doc).unwrap());
        for item in metadata() {
            assert_eq!(
                &saved[payload_range(&saved, item.key.as_bytes().try_into().unwrap())],
                &item.data
            );
        }
        let again = psd::decode(&saved).unwrap();
        assert!(!again.read_only);
        assert_eq!(again.layers[0].psd_metadata, metadata());
        assert_eq!(
            again.layers[0].pixels.rgba16(),
            original.layers[0].pixels.rgba16()
        );
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].psd_metadata, metadata());
    }
}

#[test]
fn animation_unknown_custom_descriptor_entries_and_invalid_timestamp_stay_protected() {
    let original = ordinary(&psd::encode(&fixture(16)).unwrap());
    let range = payload_range(&original, b"shmd");
    let mut cases = vec![];
    for key in [b"tmln", b"mlst", b"cmls", b"sgrp", b"extn"] {
        let mut bytes = original.clone();
        bytes[range.start + 8..range.start + 12].copy_from_slice(key);
        cases.push(bytes);
    }
    let marker = original
        .windows(13)
        .position(|v| v == b"layerTimedoub")
        .unwrap();
    let mut unknown_entry = original.clone();
    unknown_entry[marker..marker + 9].copy_from_slice(b"animFrame");
    cases.push(unknown_entry);
    let mut invalid_timestamp = original.clone();
    invalid_timestamp[marker + 13..marker + 21].copy_from_slice(&f64::NAN.to_be_bytes());
    cases.push(invalid_timestamp);
    let mut invalid_padding = original.clone();
    invalid_padding[range.end - 1] = 7;
    cases.push(invalid_padding);
    let mut invalid_count = original.clone();
    invalid_count[range.start..range.start + 4].copy_from_slice(&2u32.to_be_bytes());
    cases.push(invalid_count);
    for bytes in cases {
        let doc = psd::decode(&bytes).unwrap();
        assert!(doc.read_only);
        assert!(doc.warnings.iter().any(|w| w.contains("shmd")));
        assert_eq!(doc.bit_depth, 16);
        assert!(psd::encode(&doc).is_err());
        assert_eq!(
            doc.layers[0].pixels.rgba16(),
            fixture(16).layers[0].pixels.rgba16()
        );
    }
}

#[test]
fn malformed_static_blocks_and_unique_ids_cannot_be_injected_into_saved_sources() {
    let mut bad = metadata();
    bad[0].data[..8].copy_from_slice(&f64::INFINITY.to_be_bytes());
    assert!(psd::validate_metadata(&bad).is_err());
    let mut bad = metadata();
    bad[1].data[7] = 1;
    assert!(psd::validate_metadata(&bad).is_err());
    let mut bad = metadata();
    bad[2].data.pop();
    assert!(psd::validate_metadata(&bad).is_err());
    let mut bad = metadata();
    bad.push(bad[0].clone());
    assert!(psd::validate_metadata(&bad).is_err());
    let mut doc = fixture(16);
    doc.layers[0].psd_metadata = vec![LayerMetadata {
        key: "lyid".into(),
        data: 1u32.to_be_bytes().to_vec(),
    }];
    assert!(psd::encode(&doc).is_err());
    doc.layers[0].psd_metadata = vec![LayerMetadata {
        key: "SoLd".into(),
        data: vec![],
    }];
    assert!(psd::encode(&doc).is_err());
}

#[test]
fn duplicate_respects_photoshop_timestamp_copy_flag_without_reusing_unique_layer_ids() {
    let original = metadata();
    let duplicate = psd::metadata_for_duplicate(&original);
    assert_eq!(duplicate.len(), 3);
    assert!(!duplicate.iter().any(|item| item.key == "shmd"));
    assert_eq!(original, metadata());
    let mut copyable = original;
    copyable[3].data = timestamp(true);
    assert_eq!(psd::metadata_for_duplicate(&copyable), copyable);
}
