use peerbrush::{
    engine::{Document, Engine, Layer, Mask, MaskStep},
    psd::{self, LayerMetadata},
    raster::Raster,
};
use serde_json::json;
use std::io::{Read, Write};

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

fn global_mask_range(bytes: &[u8]) -> (usize, std::ops::Range<usize>) {
    let wide = bytes[4..6] == 2u16.to_be_bytes();
    let size = if wide { 8 } else { 4 };
    let color = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let resources = 30 + color;
    let resources_len =
        u32::from_be_bytes(bytes[resources..resources + 4].try_into().unwrap()) as usize;
    let section = resources + 4 + resources_len;
    let info = section + size;
    let info_len = if wide {
        u64::from_be_bytes(bytes[info..info + size].try_into().unwrap()) as usize
    } else {
        u32::from_be_bytes(bytes[info..info + size].try_into().unwrap()) as usize
    };
    let mask = info + size + info_len;
    let mask_len = u32::from_be_bytes(bytes[mask..mask + 4].try_into().unwrap()) as usize;
    (section, mask + 4..mask + 4 + mask_len)
}
fn replace_global_mask(bytes: &[u8], data: &[u8]) -> Vec<u8> {
    let (section, range) = global_mask_range(bytes);
    let wide = bytes[4..6] == 2u16.to_be_bytes();
    let size = if wide { 8 } else { 4 };
    let old_len = if wide {
        u64::from_be_bytes(bytes[section..section + size].try_into().unwrap()) as usize
    } else {
        u32::from_be_bytes(bytes[section..section + size].try_into().unwrap()) as usize
    };
    let mut out = bytes.to_vec();
    out.splice(range.clone(), data.iter().copied());
    out[range.start - 4..range.start].copy_from_slice(&(data.len() as u32).to_be_bytes());
    let length = old_len - range.len() + data.len();
    if wide {
        out[section..section + size].copy_from_slice(&(length as u64).to_be_bytes());
    } else {
        out[section..section + size].copy_from_slice(&(length as u32).to_be_bytes());
    }
    out
}
fn per_layer_global_mask() -> Vec<u8> {
    // The preferred kind uses per-layer values. Overlay settings are preserved,
    // not applied to raster or mask samples.
    vec![0, 0, 255, 255, 0, 0, 0, 0, 0, 0, 0, 50, 128, 0, 0, 0]
}

#[test]
fn per_layer_global_mask_settings_keep_standard_layers_editable_and_survive_native_edits() {
    for depth in [8, 16] {
        let original = fixture(depth);
        let original_png = original.export_png().unwrap();
        for large in [false, true] {
            let bytes = if large {
                psd::encode_psb(&original).unwrap()
            } else {
                psd::encode(&original).unwrap()
            };
            for length in 13..=16 {
                let settings = &per_layer_global_mask()[..length];
                let external = replace_global_mask(&ordinary(&bytes), settings);
                let loaded = psd::decode(&external).unwrap();
                assert!(!loaded.read_only, "{:?}", loaded.warnings);
                assert_eq!(loaded.layers.len(), 1);
                assert_eq!(loaded.bit_depth, depth);
                assert_eq!(loaded.export_png().unwrap(), original_png);
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
                        &[json!({"op":"selection","kind":"rectangle","rect":[0,0,1,1]})],
                        None,
                        None,
                        "Select native pixel",
                    )
                    .unwrap();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"paint.clear_selection","layer":layer})],
                        None,
                        None,
                        "Clear selected pixel",
                    )
                    .unwrap();
                assert_eq!(engine.doc.layers[0].pixels.get16(0, 0), [0; 4]);
                assert_eq!(
                    engine.doc.layers[0].pixels.get16(1, 0),
                    original.layers[0].pixels.get16(1, 0)
                );
                for saved in [
                    psd::encode(&engine.doc).unwrap(),
                    psd::encode_psb(&engine.doc).unwrap(),
                ] {
                    assert_eq!(&saved[global_mask_range(&saved).1], settings);
                    for reopening in [saved.clone(), ordinary(&saved)] {
                        let restored = psd::decode(&reopening).unwrap();
                        assert!(!restored.read_only, "{:?}", restored.warnings);
                        assert_eq!(restored.bit_depth, depth);
                        assert_eq!(
                            restored.layers[0].pixels.rgba16(),
                            engine.doc.layers[0].pixels.rgba16()
                        );
                        assert_eq!(
                            restored.export_png().unwrap(),
                            engine.doc.export_png().unwrap()
                        );
                        let resaved = psd::encode(&restored).unwrap();
                        assert_eq!(&resaved[global_mask_range(&resaved).1], settings);
                    }
                }
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.export_png().unwrap(), original_png);
                let undone = psd::encode(&engine.doc).unwrap();
                assert_eq!(&undone[global_mask_range(&undone).1], settings);
            }
        }
    }
}

#[test]
fn unknown_or_malformed_global_mask_settings_remain_protected_with_saved_precision() {
    for depth in [8, 16] {
        let original = fixture(depth);
        for bytes in [
            psd::encode(&original).unwrap(),
            psd::encode_psb(&original).unwrap(),
        ] {
            let mut cases = vec![vec![0; 12], vec![0; 17]];
            for (offset, value) in [
                (1, 1),
                (10, 1),
                (11, 101),
                (12, 0),
                (12, 1),
                (12, 129),
                (15, 1),
            ] {
                let mut settings = per_layer_global_mask();
                settings[offset] = value;
                cases.push(settings);
            }
            for settings in cases {
                // Test standard-only files and attempts to bypass protection
                // through otherwise valid supplementary sources.
                for source in [bytes.clone(), ordinary(&bytes)] {
                    let protected = psd::decode(&replace_global_mask(&source, &settings)).unwrap();
                    assert!(protected.read_only, "{depth}-bit {settings:?}");
                    assert!(protected
                        .warnings
                        .iter()
                        .any(|w| w.contains("global layer mask")));
                    assert_eq!(protected.bit_depth, depth);
                    assert_eq!(
                        protected.export_png().unwrap(),
                        original.export_png().unwrap()
                    );
                    assert!(psd::encode(&protected).is_err());
                    assert!(psd::encode_psb(&protected).is_err());
                }
            }
        }
    }
}

#[test]
fn preferred_global_mask_settings_preserve_per_layer_masks_clipping_and_folders() {
    for depth in [8, 16] {
        let mut doc = fixture(depth);
        doc.width = 4;
        doc.height = 3;
        let mut folder = Layer::new("Masked folder", "group", 4, 3);
        folder.pixels = Raster::new_depth(4, 3, depth);
        let mut base = Layer::new("Clipping base", "paint", 2, 1);
        base.pixels = Raster::new_depth(2, 1, depth);
        base.pixels.set16(0, 0, [10123, 20247, 30371, 65535]);
        base.pixels.set16(1, 0, [40123, 50247, 60371, 65535]);
        base.parent = Some(folder.id.clone());
        base.x = 1;
        base.y = 1;
        doc.layers[0].parent = base.parent.clone();
        doc.layers[0].clip_to = Some(base.id.clone());
        doc.layers[0].x = 1;
        doc.layers[0].y = 1;
        let mask = |width, height| {
            let mut pixels = Raster::new_depth(width, height, depth);
            pixels.set16(0, 0, [65535; 4]);
            pixels.set16(1, 0, [32769, 32769, 32769, 65535]);
            Mask {
                enabled: true,
                cache_key: peerbrush::engine::id(),
                steps: vec![MaskStep {
                    id: peerbrush::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    weight: 1.,
                    value: 1.,
                    pixels,
                    settings: json!({}),
                }],
            }
        };
        doc.layers[0].mask = Some(mask(2, 1));
        // Folder mask is local to its stored frame, as are raster masks.
        folder.mask = Some(mask(4, 3));
        doc.layers.insert(0, folder);
        doc.layers.push(base);
        let expected = doc.export_png().unwrap();
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let external = replace_global_mask(&ordinary(&encoded), &per_layer_global_mask());
            let restored = psd::decode(&external).unwrap();
            assert!(!restored.read_only, "{:?}", restored.warnings);
            assert_eq!(restored.layers.len(), 3);
            assert_eq!(restored.layers[0].kind, "group");
            assert_eq!(
                restored.layers[1].parent.as_ref(),
                Some(&restored.layers[0].id)
            );
            assert_eq!(
                restored.layers[1].clip_to.as_ref(),
                Some(&restored.layers[2].id)
            );
            for index in [1, 2] {
                assert_eq!(
                    restored.layers[index].pixels.rgba16(),
                    doc.layers[index].pixels.rgba16()
                );
                assert_eq!([restored.layers[index].x, restored.layers[index].y], [1, 1]);
            }
            for index in [0, 1] {
                assert_eq!(
                    peerbrush::depth16::mask_image(&restored, index)
                        .unwrap()
                        .unwrap()
                        .words,
                    peerbrush::depth16::mask_image(&doc, index)
                        .unwrap()
                        .unwrap()
                        .words
                );
            }
            assert_eq!(restored.export_png().unwrap(), expected);
            let saved = psd::encode_psb(&restored).unwrap();
            assert_eq!(&saved[global_mask_range(&saved).1], per_layer_global_mask());
            assert_eq!(
                psd::decode(&ordinary(&saved))
                    .unwrap()
                    .export_png()
                    .unwrap(),
                expected
            );
        }
    }
}

fn rewrite_private_document(bytes: &[u8], update: impl FnOnce(&mut serde_json::Value)) -> Vec<u8> {
    let color = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let resources = 30 + color;
    let resources_len =
        u32::from_be_bytes(bytes[resources..resources + 4].try_into().unwrap()) as usize;
    let start = bytes.windows(4).position(|word| word == b"PBR1").unwrap();
    let length = u32::from_be_bytes(bytes[start - 4..start].try_into().unwrap()) as usize;
    let mut json = vec![];
    flate2::read::ZlibDecoder::new(&bytes[start + 4..start + length])
        .read_to_end(&mut json)
        .unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    update(&mut value["document"]);
    let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zip.write_all(&serde_json::to_vec(&value).unwrap()).unwrap();
    let mut payload = b"PBR1".to_vec();
    payload.extend(zip.finish().unwrap());
    let new_len = payload.len();
    payload.resize(new_len + new_len % 2, 0);
    let mut out = bytes.to_vec();
    out.splice(start..start + length + length % 2, payload.iter().copied());
    out[start - 4..start].copy_from_slice(&(new_len as u32).to_be_bytes());
    let new_resources = resources_len - length - length % 2 + payload.len();
    out[resources..resources + 4].copy_from_slice(&(new_resources as u32).to_be_bytes());
    out
}

#[test]
fn supplementary_sources_cannot_drop_or_replace_standard_global_mask_settings() {
    for depth in [8, 16] {
        let mut original = fixture(depth);
        original.psd_global_mask = Some(psd::GlobalMaskSettings {
            data: per_layer_global_mask(),
        });
        for encoded in [
            psd::encode(&original).unwrap(),
            psd::encode_psb(&original).unwrap(),
        ] {
            for remove in [false, true] {
                let changed = rewrite_private_document(&encoded, |doc| {
                    if remove {
                        doc.as_object_mut().unwrap().remove("psd_global_mask");
                    } else {
                        doc["psd_global_mask"]["data"][2] = json!(127);
                    }
                });
                assert_eq!(
                    &changed[global_mask_range(&changed).1],
                    per_layer_global_mask()
                );
                let protected = psd::decode(&changed).unwrap();
                assert!(protected.read_only);
                assert!(protected
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("definitions were changed")));
                assert_eq!(protected.psd_global_mask, original.psd_global_mask);
                assert_eq!(protected.bit_depth, depth);
                assert_eq!(
                    protected.export_png().unwrap(),
                    original.export_png().unwrap()
                );
                assert!(psd::encode(&protected).is_err());
                let shared = std::sync::Arc::new(std::sync::Mutex::new({
                    let mut engine = Engine::new();
                    engine.doc = protected;
                    engine
                }));
                peerbrush::server::compatible_copy(&shared, "human", Some(0)).unwrap();
                let copied = shared.lock().unwrap();
                assert_eq!(copied.doc.psd_global_mask, None);
                assert_eq!(copied.doc.bit_depth, depth);
                assert_eq!(
                    copied.doc.export_png().unwrap(),
                    original.export_png().unwrap()
                );
            }
        }
    }
}

#[test]
fn unsupported_global_mask_settings_cannot_be_injected_into_editable_saves() {
    for depth in [8, 16] {
        for data in [vec![], vec![0; 12], vec![0; 16], vec![0; 4096]] {
            let mut doc = fixture(depth);
            doc.psd_global_mask = Some(psd::GlobalMaskSettings { data });
            assert!(psd::validate(&doc).is_err());
            assert!(psd::encode(&doc).is_err());
            assert!(psd::encode_psb(&doc).is_err());
        }
    }
}

#[test]
fn protected_source_report_retains_original_tree_coordinates_and_layer_specific_reasons() {
    for depth in [8, 16] {
        let mut original = fixture(depth);
        original.width = 4;
        original.height = 3;
        original.layers[0].x = 1;
        original.layers[0].y = 1;
        let folder = Layer::new("Source folder", "group", 4, 3);
        original.layers[0].parent = Some(folder.id.clone());
        original.layers.insert(0, folder);
        original
            .layers
            .push(Layer::new("Unrelated source", "paint", 4, 3));
        if depth == 16 {
            for layer in &mut original.layers {
                layer.pixels.promote16();
            }
        }
        let standard = ordinary(&psd::encode(&original).unwrap());
        let editable = psd::decode(&standard).unwrap();
        assert!(!editable.read_only);
        assert!(editable.source_structure.is_empty());
        let before = editable.export_png().unwrap();
        let mut unsupported = standard.clone();
        for (old, new) in [(b"lnsr", b"lrFX"), (b"lclr", b"lyvr")] {
            let range = payload_range(&unsupported, old);
            unsupported[range.start - 8..range.start - 4].copy_from_slice(new);
        }
        let protected = psd::decode(&unsupported).unwrap();
        assert!(protected.read_only);
        assert_eq!(protected.bit_depth, depth);
        assert_eq!(protected.layers.len(), 1);
        assert_eq!(protected.export_png().unwrap(), before);
        assert_eq!(protected.source_structure.len(), 3); // Closing divider is not a layer.
        let report = &protected.source_structure;
        let child = report
            .iter()
            .find(|layer| layer.name == "Preserved label")
            .unwrap();
        let parent = report
            .iter()
            .find(|layer| layer.name == "Source folder")
            .unwrap();
        assert_eq!(child.parent_index, Some(parent.source_index));
        assert_eq!(parent.parent_index, None);
        assert_eq!(child.bounds, [1, 1, 3, 2]);
        assert_eq!(child.kind, "layer"); // Unsupported records do not establish their editable type.
        assert!(child.visible);
        assert!(!child.details_truncated);
        assert!(child.tags.contains(&"lrFX".into()));
        assert!(child
            .warnings
            .iter()
            .any(|reason| reason.ends_with(": lrFX")));
        assert!(child
            .warnings
            .iter()
            .any(|reason| reason.ends_with(": lyvr")));
        assert!(report
            .iter()
            .filter(|layer| layer.name != child.name)
            .all(|layer| layer.warnings.is_empty()));
        assert!(psd::encode(&protected).unwrap_err().contains("Cannot save"));
        let serialized = serde_json::to_value(&protected).unwrap();
        assert!(serialized.get("source_structure").is_none());
        let mut engine = Engine::new();
        engine.doc = protected;
        let observed = engine.state();
        assert_eq!(
            observed["document"]["source_structure"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(observed["document"]["source_structure"][0]
            .get("id")
            .is_none());
        assert!(engine
            .edit(
                "human",
                &[json!({"op":"layer.delete","layer":engine.doc.layers[0].id})],
                None,
                None,
                "Protected source"
            )
            .is_err());
        assert_eq!(engine.doc.revision, 0);
        assert_eq!(engine.doc.export_png().unwrap(), before);
        let shared = std::sync::Arc::new(std::sync::Mutex::new(engine));
        peerbrush::server::compatible_copy(&shared, "human", Some(0)).unwrap();
        let copied = shared.lock().unwrap();
        assert!(!copied.doc.read_only);
        assert!(copied.doc.source_structure.is_empty());
        assert_eq!(copied.doc.bit_depth, depth);
        assert_eq!(copied.doc.export_png().unwrap(), before);
    }
}

#[test]
fn protected_source_report_bounds_unicode_names_without_changing_source_serialization() {
    let mut original = fixture(16);
    original.layers[0].name = "色".repeat(1100);
    let mut standard = ordinary(&psd::encode(&original).unwrap());
    let range = payload_range(&standard, b"lnsr");
    standard[range.start - 8..range.start - 4].copy_from_slice(b"lrFX");
    let protected = psd::decode(&standard).unwrap();
    assert!(protected.read_only);
    let layer = &protected.source_structure[0];
    assert_eq!(layer.name, "色".repeat(1024));
    assert!(layer.details_truncated);
    assert_eq!(protected.bit_depth, 16);
    let restored: Document =
        serde_json::from_value(serde_json::to_value(&protected).unwrap()).unwrap();
    assert!(restored.source_structure.is_empty());
    assert!(restored.read_only);
    assert_eq!(
        protected.export_png().unwrap(),
        restored.export_png().unwrap()
    );
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

#[test]
fn known_layer_reader_versions_preserve_standard_metadata_native_sources_and_history() {
    for depth in [8, 16] {
        for version in (70u32..=160).step_by(10) {
            let mut original = fixture(depth);
            let marker = LayerMetadata {
                key: "lyvr".into(),
                data: version.to_be_bytes().to_vec(),
            };
            original.layers[0].psd_metadata.push(marker.clone());
            let pixels = original.layers[0].pixels.rgba16();
            let png = original.export_png().unwrap();
            for large in [false, true] {
                let encoded = if large {
                    psd::encode_psb(&original).unwrap()
                } else {
                    psd::encode(&original).unwrap()
                };
                let at = encoded.windows(4).position(|v| v == b"PBR1").unwrap() + 4;
                let mut source = vec![];
                ZlibDecoder::new(&encoded[at..])
                    .read_to_end(&mut source)
                    .unwrap();
                assert_eq!(
                    serde_json::from_slice::<serde_json::Value>(&source).unwrap()["format"],
                    24
                );
                let standard = ordinary(&encoded);
                let loaded = psd::decode(&standard).unwrap();
                assert!(!loaded.read_only, "{:?}", loaded.warnings);
                assert_eq!(
                    loaded.layers[0].psd_metadata,
                    original.layers[0].psd_metadata
                );
                assert_eq!(loaded.layers[0].pixels.rgba16(), pixels);
                assert_eq!(loaded.export_png().unwrap(), png);
                let mut e = Engine::new();
                e.doc = loaded;
                let layer = e.doc.layers[0].id.clone();
                e.edit(
                    "human",
                    &[json!({"op":"layer.update","layer":layer,"opacity":0.5})],
                    None,
                    None,
                    "Preserve layer version",
                )
                .unwrap();
                assert_eq!(e.undo.len(), 1);
                assert_eq!(e.doc.layers[0].pixels.rgba16(), pixels);
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), png);
                e.edit(
                    "human",
                    &[json!({"op":"layer.duplicate","layers":[layer]})],
                    None,
                    None,
                    "Duplicate retained layer version",
                )
                .unwrap();
                assert_eq!(e.doc.layers.len(), 2);
                for l in &e.doc.layers {
                    assert!(l.psd_metadata.contains(&marker));
                }
                e.undo("human").unwrap();
                let saved = if large {
                    psd::encode_psb(&e.doc).unwrap()
                } else {
                    psd::encode(&e.doc).unwrap()
                };
                assert_eq!(&saved[payload_range(&saved, b"lyvr")], marker.data);
                for bytes in [saved.clone(), ordinary(&saved)] {
                    let reopened = psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(
                        reopened.layers[0].psd_metadata,
                        original.layers[0].psd_metadata
                    );
                    assert_eq!(reopened.layers[0].pixels.rgba16(), pixels);
                    assert_eq!(reopened.export_png().unwrap(), png);
                }
            }
        }
    }
}
#[test]
fn unknown_malformed_or_duplicate_layer_versions_never_authorize_editing() {
    for depth in [8, 16] {
        let mut doc = fixture(depth);
        doc.layers[0].psd_metadata.push(LayerMetadata {
            key: "lyvr".into(),
            data: 160u32.to_be_bytes().to_vec(),
        });
        for encoded in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
            let original = ordinary(&encoded);
            let range = payload_range(&original, b"lyvr");
            for version in [0u32, 69, 71, 161, 170, u32::MAX] {
                let mut bad = original.clone();
                bad[range.clone()].copy_from_slice(&version.to_be_bytes());
                let protected = psd::decode(&bad).unwrap();
                assert!(protected.read_only);
                assert_eq!(protected.export_png().unwrap(), doc.export_png().unwrap());
            }
            // The version marker cannot override an unsupported sibling source.
            let mut bad = original.clone();
            let unknown = payload_range(&bad, b"lnsr");
            bad[unknown.start - 8..unknown.start - 4].copy_from_slice(b"SoLd");
            assert!(psd::decode(&bad).unwrap().read_only);
            let mut bad = original.clone();
            let duplicate = payload_range(&bad, b"lnsr");
            bad[duplicate.start - 8..duplicate.start - 4].copy_from_slice(b"lyvr");
            bad[duplicate].copy_from_slice(&160u32.to_be_bytes());
            assert!(psd::decode(&bad).unwrap().read_only);
        }
        for data in [
            vec![],
            vec![0; 3],
            vec![0; 5],
            vec![0; 128],
            170u32.to_be_bytes().to_vec(),
        ] {
            doc.layers[0].psd_metadata.last_mut().unwrap().data = data;
            assert!(psd::validate(&doc).is_err());
            assert!(psd::encode(&doc).is_err());
            assert!(psd::encode_psb(&doc).is_err());
        }
    }
}
