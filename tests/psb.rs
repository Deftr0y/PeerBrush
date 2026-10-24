use flate2::{write::ZlibEncoder, Compression};
use peerbrush::{
    engine::{Document, Engine, Layer, Mask, MaskStep},
    loading::Control,
    psd,
    raster::Raster,
    server,
};
use serde_json::json;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};

fn b16(out: &mut Vec<u8>, v: u16) {
    out.extend(v.to_be_bytes());
}
fn b32(out: &mut Vec<u8>, v: u32) {
    out.extend(v.to_be_bytes());
}
fn b64(out: &mut Vec<u8>, v: u64) {
    out.extend(v.to_be_bytes());
}
fn block(out: &mut Vec<u8>, data: &[u8], wide: bool) {
    if wide {
        b64(out, data.len() as u64)
    } else {
        b32(out, data.len() as u32)
    };
    out.extend(data);
}
fn tag(out: &mut Vec<u8>, key: &[u8; 4], data: &[u8], wide: bool, padding: usize) {
    out.extend(b"8B64");
    out.extend(key);
    block(out, data, wide);
    out.resize(out.len() + (padding - data.len() % padding) % padding, 0);
}
fn words(depth: u16) -> Vec<Vec<u16>> {
    vec![
        vec![10001, 10002, 30003, 40007, 50001, 60001],
        vec![20003, 22007, 24011, 26013, 28017, 30019],
        vec![40007, 40009, 44011, 46013, 48017, 50021],
        vec![65535; 6],
        vec![12347, 23459, 34567, 45679, 56789, 65535],
    ]
    .into_iter()
    .map(|p| {
        if depth == 8 {
            p.into_iter().map(|v| v / 257 * 257).collect()
        } else {
            p
        }
    })
    .collect()
}
fn compressed(planes: &[Vec<u16>], depth: u16, kind: u16) -> Vec<u8> {
    let row_bytes = 3 * if depth == 16 { 2 } else { 1 };
    let mut raw: Vec<u8> = planes
        .iter()
        .flat_map(|p| {
            p.iter().flat_map(|v| {
                if depth == 16 {
                    v.to_be_bytes().to_vec()
                } else {
                    vec![(v / 257) as u8]
                }
            })
        })
        .collect();
    let mut out = vec![];
    b16(&mut out, kind);
    match kind {
        0 => out.extend(raw),
        1 => {
            let rows: Vec<_> = raw
                .chunks_exact(row_bytes)
                .map(|r| {
                    let mut v = vec![r.len() as u8 - 1];
                    v.extend(r);
                    v
                })
                .collect();
            for r in &rows {
                b32(&mut out, r.len() as u32);
            }
            for r in rows {
                out.extend(r);
            }
        }
        2 | 3 => {
            if kind == 3 {
                for row in raw.chunks_exact_mut(row_bytes) {
                    if depth == 16 {
                        let mut previous = 0u16;
                        for w in row.chunks_exact_mut(2) {
                            let value = u16::from_be_bytes(w.try_into().unwrap());
                            w.copy_from_slice(&value.wrapping_sub(previous).to_be_bytes());
                            previous = value;
                        }
                    } else {
                        let mut previous = 0u8;
                        for v in row {
                            let value = *v;
                            *v = value.wrapping_sub(previous);
                            previous = value;
                        }
                    }
                }
            }
            let mut zip = ZlibEncoder::new(vec![], Compression::fast());
            zip.write_all(&raw).unwrap();
            out.extend(zip.finish().unwrap());
        }
        _ => unreachable!(),
    }
    out
}
// Construct independently of the production writer, with 64-bit structural
// lengths, 32-bit PSB RLE row counts and both 8B64 / ordinary tag lengths.
fn fixture(depth: u16, kind: u16, unsupported_layer: bool, unsupported_global: bool) -> Vec<u8> {
    let planes = words(depth);
    let mut info = vec![];
    b16(&mut info, (-1i16) as u16);
    for v in [0, 0, 2, 3] {
        b32(&mut info, v);
    }
    b16(&mut info, 5);
    let channels: Vec<_> = planes
        .iter()
        .map(|p| compressed(&[p.clone()], depth, kind))
        .collect();
    for (id, data) in [0, 1, 2, -1i16, -2].into_iter().zip(&channels) {
        b16(&mut info, id as u16);
        b64(&mut info, data.len() as u64);
    }
    info.extend(b"8BIMnorm");
    info.extend([255, 0, 0, 0]);
    let mut extra = vec![];
    let mut mask = vec![];
    for v in [0, 0, 2, 3] {
        b32(&mut mask, v);
    }
    mask.extend([255, 2, 0, 0]);
    block(&mut extra, &mask, false);
    b32(&mut extra, 0);
    extra.extend([6, b'S', b'o', b'u', b'r', b'c', b'e', 0]);
    let mut name = vec![];
    let utf16: Vec<_> = "Source Δ".encode_utf16().collect();
    b32(&mut name, utf16.len() as u32);
    for c in utf16 {
        b16(&mut name, c);
    }
    tag(&mut extra, b"luni", &name, false, 2);
    if unsupported_layer {
        tag(&mut extra, b"lnk2", b"opaque source", true, 2);
    }
    block(&mut info, &extra, false);
    for data in channels {
        info.extend(data);
    }
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut layers = vec![];
    if depth == 16 {
        b64(&mut layers, 0);
        b32(&mut layers, 0);
        tag(&mut layers, b"Lr16", &info, true, 4);
        tag(&mut layers, b"Mt16", &[], true, 4);
    } else {
        block(&mut layers, &info, true);
        b32(&mut layers, 0);
    }
    if unsupported_global {
        tag(&mut layers, b"LMsk", b"unsupported", true, 4);
    }
    let mut out = b"8BPS".to_vec();
    b16(&mut out, 2);
    out.extend([0; 6]);
    b16(&mut out, 4);
    b32(&mut out, 2);
    b32(&mut out, 3);
    b16(&mut out, depth);
    b16(&mut out, 3);
    b32(&mut out, 0);
    b32(&mut out, 0);
    block(&mut out, &layers, true);
    out.extend(compressed(&planes[..4], depth, kind));
    out
}
fn expected_pixels(depth: u16) -> Vec<u16> {
    let p = words(depth);
    (0..6)
        .flat_map(|i| [p[0][i], p[1][i], p[2][i], 65535])
        .collect()
}
fn remove_resources(bytes: &[u8]) -> Vec<u8> {
    let color = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let pos = 30 + color;
    let n = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
    let mut out = bytes[..pos].to_vec();
    b32(&mut out, 0);
    out.extend(&bytes[pos + 4 + n..]);
    out
}

#[test]
fn independent_psb_raw_rle_zip_and_prediction_keep_native_layers_masks_and_composite() {
    for depth in [8, 16] {
        for kind in 0..4 {
            let bytes = fixture(depth, kind, false, false);
            let doc = psd::decode(&bytes).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            assert_eq!(doc.bit_depth, depth);
            assert_eq!(doc.layers[0].name, "Source Δ");
            assert_eq!(doc.layers[0].pixels.rgba16(), expected_pixels(depth));
            let mask = doc.layers[0].mask.as_ref().unwrap();
            assert!(!mask.enabled);
            for (i, v) in words(depth)[4].iter().enumerate() {
                assert_eq!(
                    mask.steps[1].pixels.get16((i % 3) as i32, (i / 3) as i32),
                    [*v, *v, *v, 65535]
                );
            }
            let saved = psd::encode_psb(&doc).unwrap();
            assert_eq!(&saved[4..6], &2u16.to_be_bytes());
            let standard = psd::decode(&remove_resources(&saved)).unwrap();
            assert!(!standard.read_only);
            assert_eq!(standard.export_png().unwrap(), doc.export_png().unwrap());
            assert_eq!(standard.layers[0].pixels.rgba16(), expected_pixels(depth));
            let converted = psd::decode(&psd::encode(&doc).unwrap()).unwrap();
            assert_eq!(converted.export_png().unwrap(), doc.export_png().unwrap());
        }
    }
}

#[test]
fn psb_saved_groups_effects_clipping_profiles_and_standard_channels_are_current() {
    for depth in [8, 16] {
        let mut doc = Document::new_depth(3, 2, depth).unwrap();
        doc.srgb_tagged = true;
        let mut folder = Layer::new("Folder", "group", 3, 2);
        folder.pixels.convert_depth(depth);
        folder.blend = "pass_through".into();
        let mut base = Layer::new("Native source", "paint", 3, 2);
        base.pixels = Raster::from_rgba16(3, 2, &expected_pixels(depth)).unwrap();
        base.pixels.convert_depth(depth);
        base.parent = Some(folder.id.clone());
        let mut mask_pixels = Raster::new_depth(3, 2, depth);
        mask_pixels.set16(1, 0, [23459, 23459, 23459, 65535]);
        base.mask = Some(Mask {
            linked: true,
            enabled: true,
            cache_key: peerbrush::engine::id(),
            steps: vec![
                MaskStep {
                    weight: 1.,
                    id: peerbrush::engine::id(),
                    kind: "paint".into(),
                    enabled: true,
                    value: 0.,
                    pixels: mask_pixels,
                    settings: json!(null),
                },
                MaskStep {
                    weight: 1.,
                    id: peerbrush::engine::id(),
                    kind: "fill".into(),
                    enabled: true,
                    value: 255.,
                    pixels: Raster::new_depth(3, 2, depth),
                    settings: json!(null),
                },
            ],
        });
        let mut clipped = Layer::new("Clipped", "paint", 3, 2);
        clipped.parent = Some(folder.id.clone());
        clipped.clip_to = Some(base.id.clone());
        clipped.blend = "multiply".into();
        clipped.pixels = Raster::from_rgba16(
            3,
            2,
            &vec![[50001, 45003, 40007, 65535]; 6]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>(),
        )
        .unwrap();
        clipped.pixels.convert_depth(depth);
        clipped.effects.push(peerbrush::effects::Effect {
            id: peerbrush::engine::id(),
            kind: "invert".into(),
            enabled: true,
            weight: 0.35,
            settings: json!({}).into(),
        });
        let mut background = Layer::new("Opaque backdrop", "paint", 3, 2);
        background.pixels.convert_depth(depth);
        for y in 0..2 {
            for x in 0..3 {
                background.pixels.set16(x, y, [60001, 61003, 62007, 65535]);
            }
        }
        background.pixels.convert_depth(depth);
        doc.layers = vec![folder, clipped, base, background];
        let before = doc.export_png().unwrap();
        let bytes = psd::encode_psb(&doc).unwrap();
        let loaded = psd::decode(&bytes).unwrap();
        assert!(!loaded.read_only);
        assert_eq!(loaded.export_png().unwrap(), before);
        assert!(loaded.srgb_tagged);
        for (a, b) in loaded.layers.iter().zip(&doc.layers) {
            assert_eq!(a.pixels.rgba16(), b.pixels.rgba16());
            assert_eq!(a.parent, b.parent);
            assert_eq!(a.clip_to, b.clip_to);
        }
        let standard = psd::decode(&remove_resources(&bytes)).unwrap();
        assert!(!standard.read_only, "{:?}", standard.warnings);
        assert_eq!(standard.layers.len(), 4);
        assert_eq!(standard.export_png().unwrap(), remove_profile_png(&doc));
        assert_eq!(
            standard.layers[2].pixels.rgba16(),
            doc.layers[2].pixels.rgba16()
        );
        assert!(standard.layers[1].clip_to.is_some());
        // The current merged raw planar image is standard PSB data, not private sources.
        let pos = 30;
        let resources = u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap()) as usize;
        let pos = pos + 4 + resources;
        let size = u64::from_be_bytes(bytes[pos..pos + 8].try_into().unwrap()) as usize;
        let composite = &bytes[pos + 8 + size..];
        assert_eq!(&composite[..2], &[0, 0]);
        let expected = if depth == 16 {
            peerbrush::depth16::render(&doc).unwrap().words
        } else {
            let (_, _, rgba, _) = doc.preview(None, 3, None, false).unwrap();
            rgba.into_iter().map(|v| v as u16 * 257).collect()
        };
        let values: Vec<u16> = if depth == 16 {
            composite[2..]
                .chunks_exact(2)
                .map(|v| u16::from_be_bytes(v.try_into().unwrap()))
                .collect()
        } else {
            composite[2..].iter().map(|v| *v as u16 * 257).collect()
        };
        for c in 0..4 {
            for i in 0..6 {
                assert_eq!(values[c * 6 + i], expected[i * 4 + c]);
            }
        }
    }
}
fn remove_profile_png(doc: &Document) -> Vec<u8> {
    let mut d = doc.clone();
    d.srgb_tagged = false;
    d.export_png().unwrap()
}

#[test]
fn unsupported_psb_wide_tags_and_global_sources_keep_protected_native_composites() {
    for depth in [8, 16] {
        for (layer, global) in [(true, false), (false, true)] {
            let doc = psd::decode(&fixture(depth, 1, layer, global)).unwrap();
            assert!(doc.read_only);
            assert_eq!(doc.bit_depth, depth);
            assert_eq!(doc.layers.len(), 1);
            assert_eq!(doc.layers[0].pixels.rgba16(), expected_pixels(depth));
            assert!(psd::encode_psb(&doc).is_err());
            assert!(psd::encode(&doc).is_err());
            let shared = Arc::new(Mutex::new(Engine::new()));
            shared.lock().unwrap().doc = doc;
            server::compatible_copy(&shared, "human", None).unwrap();
            let e = shared.lock().unwrap();
            assert!(!e.doc.read_only);
            assert_eq!(e.doc.bit_depth, depth);
            assert_eq!(e.doc.layers[0].pixels.rgba16(), expected_pixels(depth));
            assert!(e.path.is_none());
        }
    }
}

#[test]
fn oversized_psb_lengths_and_rle_tables_fail_before_allocation_and_cancellation_preserves_work() {
    let bytes = fixture(16, 1, false, false);
    for (at, width) in [(34, 8), (42, 8), (62, 8), (92, 8)] {
        let mut bad = bytes.clone();
        bad[at..at + width].fill(255);
        assert!(psd::decode(&bad).is_err(), "Offset {at}");
    }
    let mut bad = fixture(8, 1, false, false);
    let outer = u64::from_be_bytes(bad[34..42].try_into().unwrap()) as usize;
    let row = 42 + outer + 2;
    bad[row..row + 4].fill(255);
    assert!(psd::decode(&bad).unwrap_err().contains("RLE row"));
    let control = Control::default();
    control.cancel();
    assert!(psd::decode_reader(&mut std::io::Cursor::new(bytes), &control).is_err());
    let mut too_large = fixture(16, 0, false, false);
    too_large[18..22].copy_from_slice(&8193u32.to_be_bytes());
    assert!(psd::decode(&too_large).is_err());
    let mut unsupported_depth = fixture(16, 0, false, false);
    unsupported_depth[22..24].copy_from_slice(&32u16.to_be_bytes());
    assert!(psd::decode(&unsupported_depth).is_err());
}

#[test]
fn protocol_psb_save_open_external_conflict_and_failed_open_preserve_original_work() {
    let temp = std::env::temp_dir().join(format!("peerbrush-psb-{}", peerbrush::engine::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let path = temp.join("Artwork.PSB");
    let mut engine = Engine::new();
    engine.doc = psd::decode(&fixture(16, 2, false, false)).unwrap();
    let shared = Arc::new(Mutex::new(engine));
    peerbrush::workspace::attach(&shared);
    let before = shared.lock().unwrap().doc.export_png().unwrap();
    let document = shared.lock().unwrap().doc.id.clone();
    let revision = shared.lock().unwrap().doc.revision;
    let report = server::dispatch(
        &shared,
        "document",
        &json!({"action":"save_report","document_id":document,"expected_revision":revision}),
    )
    .unwrap();
    assert!(report["has_editability_losses"].as_bool().unwrap());
    server::dispatch(
        &shared,
        "document",
        &json!({"action":"save","path":path,"actor":"human","document_id":document,"expected_revision":revision,"accept_editability_loss":true}),
    )
    .unwrap();
    assert_eq!(&std::fs::read(&path).unwrap()[4..6], &2u16.to_be_bytes());
    assert!(server::dispatch(
        &shared,
        "projects",
        &json!({"action":"open","path":path,"actor":"human"})
    )
    .is_err());
    // Save As releases the original before the protocol opens it as an independent project.
    server::save_accepted_source(
        &shared,
        &temp.join("Working.psb"),
        &document,
        revision,
        false,
    )
    .unwrap();
    let opened = server::dispatch(
        &shared,
        "projects",
        &json!({"action":"open","path":path,"actor":"human"}),
    )
    .unwrap();
    assert_eq!(opened["file_status"], "PSB opened");
    let id = opened["project_id"].as_str().unwrap();
    let project = peerbrush::workspace::get(&shared, id).unwrap();
    assert_eq!(project.lock().unwrap().doc.export_png().unwrap(), before);
    assert_eq!(project.lock().unwrap().doc.bit_depth, 16);
    let doc = project.lock().unwrap().doc.id.clone();
    let revision = project.lock().unwrap().doc.revision;
    let mut changed = std::fs::read(&path).unwrap();
    changed.push(0);
    std::fs::write(&path, &changed).unwrap();
    assert!(
        server::save_reviewed_source(&project, &path, &doc, revision)
            .unwrap_err()
            .contains("outside")
    );
    assert_eq!(std::fs::read(&path).unwrap(), changed);
    let malformed = temp.join("Invalid.psb");
    std::fs::write(&malformed, b"8BPS\0\x02bad").unwrap();
    assert!(server::open(&project, &malformed).is_err());
    let e = project.lock().unwrap();
    assert_eq!(e.doc.id, doc);
    assert_eq!(e.doc.revision, revision);
    assert_eq!(e.doc.export_png().unwrap(), before);
    assert!(e.undo.is_empty());
    drop(e);
    assert_eq!(shared.lock().unwrap().doc.export_png().unwrap(), before);
    std::fs::remove_dir_all(temp).unwrap();
}
