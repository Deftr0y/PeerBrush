use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::{
    engine::{Document, Engine},
    loading::Control,
    psd::{self, patterns},
    raster, server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
fn u32b(out: &mut Vec<u8>, n: u32) {
    out.extend(n.to_be_bytes());
}
fn block(out: &mut Vec<u8>, bytes: &[u8]) {
    u32b(out, bytes.len() as u32);
    out.extend(bytes);
}
fn rectangle(out: &mut Vec<u8>) {
    for n in [5i32, -2, 7, 1] {
        out.extend(n.to_be_bytes());
    }
}
fn fixture(depth: u16, compression: u8, id: &str) -> (Vec<u8>, Vec<u16>) {
    fixture_alpha(depth, compression, id, false)
}
fn fixture_alpha(depth: u16, compression: u8, id: &str, alpha: bool) -> (Vec<u8>, Vec<u16>) {
    let mut words = Vec::new();
    for y in 0..2 {
        for x in 0..3 {
            words.extend([
                12345 + x * 911 + y * 103,
                23457 + x * 719 + y * 97,
                34569 + x * 619 + y * 71,
                if alpha {
                    [0, 41317, 65535, 21117, 1, 53219][(y * 3 + x) as usize]
                } else {
                    65535
                },
            ]);
        }
    }
    if depth == 8 {
        for n in &mut words {
            *n = u16::from(raster::project16(*n)) * 257;
        }
    }
    let mut pattern = Vec::new();
    u32b(&mut pattern, 1);
    u32b(&mut pattern, 3);
    pattern.extend(2i16.to_be_bytes());
    pattern.extend(3i16.to_be_bytes());
    let name: Vec<u16> = "Synthetic tile\0".encode_utf16().collect();
    u32b(&mut pattern, name.len() as u32);
    for ch in name {
        pattern.extend(ch.to_be_bytes());
    }
    pattern.push(id.len() as u8);
    pattern.extend(id.as_bytes());
    u32b(&mut pattern, 3);
    let mut arrays = Vec::new();
    rectangle(&mut arrays);
    u32b(&mut arrays, 24);
    for slot in 0..26 {
        if slot >= 3 && !(alpha && slot == 25) {
            u32b(&mut arrays, 0);
            continue;
        }
        let channel = if slot == 25 { 3 } else { slot };
        u32b(&mut arrays, 1);
        let mut c = Vec::new();
        u32b(&mut c, u32::from(depth));
        rectangle(&mut c);
        c.extend(depth.to_be_bytes());
        c.push(compression);
        let rows: Vec<Vec<u8>> = (0..2)
            .map(|y| {
                let mut row = Vec::new();
                for x in 0..3 {
                    let n = words[(y * 3 + x) * 4 + channel];
                    if depth == 16 {
                        row.extend(n.to_be_bytes());
                    } else {
                        row.push(raster::project16(n));
                    }
                }
                row
            })
            .collect();
        if compression == 1 {
            for row in &rows {
                c.extend(((row.len() + 2) as u16).to_be_bytes());
            }
            for row in rows {
                c.push(128);
                c.push(row.len() as u8 - 1);
                c.extend(row);
            }
        } else if compression == 2 || compression == 3 {
            use std::io::Write;
            let mut zip =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            for mut row in rows {
                if compression == 3 {
                    if depth == 16 {
                        let values: Vec<_> = row
                            .chunks_exact(2)
                            .map(|v| u16::from_be_bytes([v[0], v[1]]))
                            .collect();
                        row = values
                            .iter()
                            .enumerate()
                            .flat_map(|(i, v)| {
                                v.wrapping_sub(if i == 0 { 0 } else { values[i - 1] })
                                    .to_be_bytes()
                            })
                            .collect();
                    } else {
                        for i in (1..row.len()).rev() {
                            row[i] = row[i].wrapping_sub(row[i - 1]);
                        }
                    }
                }
                zip.write_all(&row).unwrap();
            }
            c.extend(zip.finish().unwrap());
        } else {
            for row in rows {
                c.extend(row);
            }
        }
        block(&mut arrays, &c);
    }
    block(&mut pattern, &arrays);
    let mut out = Vec::new();
    block(&mut out, &pattern);
    out.resize(out.len().div_ceil(4) * 4, 0);
    (out, words)
}
#[test]
fn trailing_pattern_transparency_preserves_hidden_rgb_and_every_native_alpha_sample() {
    for depth in [8, 16] {
        for compression in [0, 1, 2, 3] {
            for tag in ["Patt", "Pat2", "Pat3"] {
                let (bytes, words) = fixture_alpha(depth, compression, "transparent-pattern", true);
                let tile = patterns::decode(&bytes, tag, &Control::default())
                    .unwrap()
                    .remove(0);
                assert_eq!(tile.pixels.rgba16(), words);
                assert_eq!(tile.bounds, [-2, 5, 1, 7]);
                assert_eq!(tile.bit_depth, depth);
                let png = if depth == 16 {
                    raster::png16(3, 2, &tile.pixels.rgba16()).unwrap()
                } else {
                    raster::png(3, 2, &tile.pixels.rgba()).unwrap()
                };
                assert_eq!(
                    image::load_from_memory(&png)
                        .unwrap()
                        .into_rgba16()
                        .into_raw(),
                    words
                );
            }
        }
    }
}
#[test]
fn malformed_or_misplaced_pattern_transparency_rejects_instead_of_becoming_opaque() {
    for depth in [8, 16] {
        let (bytes, _) = fixture_alpha(depth, 0, "transparent-pattern", true);
        let pattern_len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
        let alpha_len = 23 + 6 * usize::from(depth / 8);
        let at = 4 + pattern_len - alpha_len;
        for (offset, patch) in [
            (
                0,
                (if depth == 8 { 16u32 } else { 8 }).to_be_bytes().to_vec(),
            ),
            (4, 6i32.to_be_bytes().to_vec()),
            (20, 32u16.to_be_bytes().to_vec()),
            (22, vec![4]),
        ] {
            let mut bad = bytes.clone();
            bad[at + offset..at + offset + patch.len()].copy_from_slice(&patch);
            assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
        }
        // Move a complete alpha plane into the reserved slot without changing any lengths.
        let mut bad = bytes.clone();
        bad[at - 12..4 + pattern_len].rotate_left(4);
        assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
        // A written-but-empty trailing plane cannot silently default to opaque.
        let mut bad = bytes[..at].to_vec();
        bad[at - 4..at].copy_from_slice(&0u32.to_be_bytes());
        bad[..4].copy_from_slice(&((pattern_len - alpha_len) as u32).to_be_bytes());
        let first = bytes
            .windows(8)
            .position(|v| v[..4] == u32::from(depth).to_be_bytes() && v[4..] == [0, 0, 0, 5])
            .unwrap();
        let arrays_at = first - 32;
        let arrays_len = u32::from_be_bytes(bytes[arrays_at..arrays_at + 4].try_into().unwrap());
        bad[arrays_at..arrays_at + 4]
            .copy_from_slice(&(arrays_len - alpha_len as u32).to_be_bytes());
        bad.resize(bad.len().div_ceil(4) * 4, 0);
        assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
    }
}
fn inject(bytes: &[u8], tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let large = bytes[4..6] == [0, 2];
    let n = if large { 8 } else { 4 };
    let mut at = 26;
    let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    at += 4 + len;
    let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    // Remove supplementary resources; retain standard profile and current composite.
    let resources = &bytes[at + 4..at + 4 + len];
    let mut kept = Vec::new();
    let mut p = 0;
    while p < resources.len() {
        let start = p;
        p += 4;
        let key = u16::from_be_bytes(resources[p..p + 2].try_into().unwrap());
        p += 2;
        let name = resources[p] as usize;
        p += 1 + name;
        p += p % 2;
        let size = u32::from_be_bytes(resources[p..p + 4].try_into().unwrap()) as usize;
        p += 4 + size;
        p += size % 2;
        if key != 4000 {
            kept.extend(&resources[start..p]);
        }
    }
    let mut out = bytes[..at].to_vec();
    block(&mut out, &kept);
    let section_at = out.len();
    let old_at = at + 4 + len;
    let size = if large {
        u64::from_be_bytes(bytes[old_at..old_at + 8].try_into().unwrap()) as usize
    } else {
        u32::from_be_bytes(bytes[old_at..old_at + 4].try_into().unwrap()) as usize
    };
    let mut section = bytes[old_at + n..old_at + n + size].to_vec();
    section.extend(b"8BIM");
    section.extend(tag);
    block(&mut section, payload);
    section.resize(section.len().div_ceil(4) * 4, 0);
    if large {
        out.extend((section.len() as u64).to_be_bytes());
    } else {
        u32b(&mut out, section.len() as u32);
    }
    out.extend(section);
    out.extend(&bytes[old_at + n + size..]);
    assert!(out.len() > section_at);
    out
}
#[test]
fn supported_rgb_assets_preserve_every_native_sample_and_asset_coordinates() {
    for depth in [8, 16] {
        for compression in [0, 1, 2, 3] {
            for tag in ["Patt", "Pat2", "Pat3"] {
                let (bytes, words) = fixture(depth, compression, "synthetic-pattern");
                let sources = patterns::decode(&bytes, tag, &Control::default()).unwrap();
                assert_eq!(sources.len(), 1);
                let s = &sources[0];
                assert_eq!((s.width, s.height, s.bit_depth), (3, 2, depth));
                assert_eq!(s.bounds, [-2, 5, 1, 7]);
                assert_eq!(s.name, "Synthetic tile");
                assert_eq!(s.pixels.rgba16(), words);
                assert_eq!(s.color_space, "unmanaged_source_samples");
                assert!(serde_json::to_value(s).unwrap().get("pixels").is_none());
            }
        }
    }
}
#[test]
fn malformed_assets_versions_sizes_unicode_duplicates_and_extra_data_reject() {
    let (bytes, _) = fixture(16, 1, "synthetic-pattern");
    for end in 1..bytes.len() {
        assert!(
            patterns::decode(&bytes[..end], "Patt", &Control::default()).is_err(),
            "prefix {end}"
        );
    }
    assert!(patterns::decode(&bytes, "unknown", &Control::default()).is_err());
    assert!(patterns::decode(
        &[bytes.clone(), bytes.clone()].concat(),
        "Patt",
        &Control::default()
    )
    .is_err());
    for (at, value) in [(4, 2u32), (8, 4), (16, 1025)] {
        let mut bad = bytes.clone();
        bad[at..at + 4].copy_from_slice(&value.to_be_bytes());
        assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
    }
    let mut bad = bytes.clone();
    bad[20..22].copy_from_slice(&0xd800u16.to_be_bytes());
    assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
    let mut bad = bytes.clone();
    bad[12..14].copy_from_slice(&513i16.to_be_bytes());
    assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
    let mut bad = bytes.clone();
    let end = bad.len();
    bad[end - 1] = 1;
    assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
    assert!(patterns::decode(&vec![0; 8 * 1024 * 1024 + 1], "Patt", &Control::default()).is_err());
    let control = Control::default();
    control.cancel();
    assert!(patterns::decode(&bytes, "Patt", &control).is_err());
}
#[test]
fn unsupported_channel_precision_compression_masks_and_bounds_never_substitute_pixels() {
    let (bytes, _) = fixture(16, 0, "synthetic-pattern");
    let marker = [0, 0, 0, 16, 0, 0, 0, 5];
    let at = bytes
        .windows(marker.len())
        .position(|b| b == marker)
        .unwrap();
    for (offset, patch) in [
        (0, 32u32.to_be_bytes().to_vec()),
        (4, 6i32.to_be_bytes().to_vec()),
        (20, 8u16.to_be_bytes().to_vec()),
        (22, vec![2]),
        (22, vec![3]),
    ] {
        let mut bad = bytes.clone();
        bad[at + offset..at + offset + patch.len()].copy_from_slice(&patch);
        assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
    }
    let mut bad = bytes.clone();
    bad[at + 23] = 1;
    bad.push(0);
    assert!(patterns::decode(&bad, "Patt", &Control::default()).is_err());
    // A genuinely written extra channel must reject, rather than become opaque RGB.
    let pattern_len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    let arrays_len_at = at - 32;
    let arrays_len =
        u32::from_be_bytes(bytes[arrays_len_at..arrays_len_at + 4].try_into().unwrap());
    let extra_at = 4 + pattern_len - 23 * 4;
    let mut bad = bytes[..4 + pattern_len].to_vec();
    bad.splice(extra_at..extra_at + 4, [0, 0, 0, 1, 0, 0, 0, 1, 0]);
    bad[..4].copy_from_slice(&((pattern_len + 5) as u32).to_be_bytes());
    bad[arrays_len_at..arrays_len_at + 4].copy_from_slice(&(arrays_len + 5).to_be_bytes());
    bad.resize(bad.len().div_ceil(4) * 4, 0);
    let error = patterns::decode(&bad, "Patt", &Control::default()).unwrap_err();
    assert!(error.contains("extra written channels"), "{error}");
    let mut list = Vec::new();
    for i in 0..17 {
        list.extend(fixture(8, 0, &format!("tile-{i}")).0);
    }
    assert!(patterns::decode(&list, "Patt", &Control::default()).is_err());
}
#[test]
fn psd_psb_pattern_sources_are_view_only_with_exact_current_native_composites() {
    for depth in [8, 16] {
        for compression in [1, 2, 3] {
            let mut doc = Document::new_depth(8, 6, depth).unwrap();
            doc.layers[0]
                .pixels
                .set16(3, 2, [12345, 23457, 34569, 65535]);
            let current = doc.export_png().unwrap();
            let (patt, words) = fixture_alpha(depth, compression, "synthetic-pattern", true);
            for (tag, encoded) in [
                (b"Patt", psd::encode(&doc).unwrap()),
                (b"Pat2", psd::encode_psb(&doc).unwrap()),
            ] {
                let bytes = inject(&encoded, tag, &patt);
                let restored = psd::decode(&bytes).unwrap();
                assert!(restored.read_only);
                assert_eq!(restored.export_png().unwrap(), current);
                assert_eq!(restored.source_patterns.len(), 1);
                assert_eq!(restored.source_patterns[0].pixels.rgba16(), words);
                assert!(restored.layers[0].effects.is_empty());
                assert!(serde_json::to_value(&restored)
                    .unwrap()
                    .get("source_patterns")
                    .is_none());
                let mut e = Engine::new();
                e.doc = restored;
                let before = e.doc.export_png().unwrap();
                let id = e.doc.layers[0].id.clone();
                assert!(e
                    .edit(
                        "human",
                        &[json!({"op":"paint.fill","layer":id})],
                        None,
                        None,
                        "Protected"
                    )
                    .is_err());
                assert!(e.undo.is_empty());
                assert_eq!(e.doc.export_png().unwrap(), before);
            }
        }
    }
}
#[test]
fn shared_pattern_inspection_returns_actual_native_pngs_with_source_guards_and_asset_space() {
    for depth in [8, 16] {
        for compression in [1, 2, 3] {
            let mut e = Engine::new();
            let (bytes, words) = fixture_alpha(depth, compression, "synthetic-pattern", true);
            e.doc.source_patterns = patterns::decode(&bytes, "Patt", &Control::default()).unwrap();
            e.doc.read_only = true;
            let before = serde_json::to_value(&e.doc).unwrap();
            let id = e.doc.id.clone();
            let revision = e.doc.revision;
            let shared = Arc::new(Mutex::new(e));
            let request = json!({"action":"pattern_sources","document_id":id,"expected_revision":revision,"pattern_index":0});
            let result = server::dispatch(&shared, "document", &request).unwrap();
            assert_eq!(result["coordinate_space"], "pattern_asset");
            assert!(result["images"][0].get("document_rect").is_none());
            assert_eq!(result["images"][0]["asset_rect"], json!([-2, 5, 1, 7]));
            assert_eq!(result["images"][0]["native_depth"], depth);
            let png = STANDARD
                .decode(result["images"][0]["data"].as_str().unwrap())
                .unwrap();
            let decoded = image::load_from_memory(&png).unwrap();
            assert_eq!(decoded.into_rgba16().into_raw(), words);
            assert_eq!(
                serde_json::to_value(&shared.lock().unwrap().doc).unwrap(),
                before
            );
            assert!(shared.lock().unwrap().undo.is_empty());
            for patch in [
                json!({"pattern_index":1}),
                json!({"pattern_index":-1}),
                json!({"pattern_index":0.5}),
                json!({"document_id":"different"}),
                json!({"expected_revision":revision+1}),
            ] {
                let mut p = request.clone();
                for (k, v) in patch.as_object().unwrap() {
                    p[k] = v.clone();
                }
                assert!(server::dispatch(&shared, "document", &p).is_err());
            }
        }
    }
}
fn replace_first_channel_payload(bytes: &[u8], payload: &[u8]) -> Vec<u8> {
    let at = bytes
        .windows(8)
        .position(|v| (v[..4] == [0, 0, 0, 8] || v[..4] == [0, 0, 0, 16]) && v[4..] == [0, 0, 0, 5])
        .unwrap();
    let channel_len = u32::from_be_bytes(bytes[at - 4..at].try_into().unwrap()) as usize;
    let pattern_len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
    let arrays_at = at - 32;
    let arrays_len =
        u32::from_be_bytes(bytes[arrays_at..arrays_at + 4].try_into().unwrap()) as usize;
    let delta = payload.len() as isize - (channel_len - 23) as isize;
    let mut out = bytes[..4 + pattern_len].to_vec();
    out.splice(at + 23..at + channel_len, payload.iter().copied());
    out[at - 4..at].copy_from_slice(&((23 + payload.len()) as u32).to_be_bytes());
    out[..4].copy_from_slice(&((pattern_len as isize + delta) as u32).to_be_bytes());
    out[arrays_at..arrays_at + 4]
        .copy_from_slice(&((arrays_len as isize + delta) as u32).to_be_bytes());
    out.resize(out.len().div_ceil(4) * 4, 0);
    out
}

#[test]
fn zip_patterns_reject_truncation_trailing_streams_corrupt_checksums_and_excessive_output() {
    use std::io::Write;
    let zipped = |raw: &[u8]| {
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(raw).unwrap();
        z.finish().unwrap()
    };
    let (bytes, _) = fixture(16, 2, "synthetic-pattern");
    let marker = [0, 0, 0, 16, 0, 0, 0, 5];
    let at = bytes
        .windows(marker.len())
        .position(|v| v == marker)
        .unwrap();
    let len = u32::from_be_bytes(bytes[at - 4..at].try_into().unwrap()) as usize;
    let payload = &bytes[at + 23..at + len];
    let mut corrupt = payload.to_vec();
    *corrupt.last_mut().unwrap() ^= 0xff;
    for bad in [
        corrupt,
        [payload, &[0]].concat(),
        [payload, payload].concat(),
        zipped(&[0; 11]),
        zipped(&[0; 13]),
        zipped(&vec![0; 1024 * 1024]),
    ] {
        assert!(patterns::decode(
            &replace_first_channel_payload(&bytes, &bad),
            "Patt",
            &Control::default()
        )
        .is_err());
    }
    for end in 0..payload.len() {
        assert!(
            patterns::decode(
                &replace_first_channel_payload(&bytes, &payload[..end]),
                "Patt",
                &Control::default()
            )
            .is_err(),
            "truncated compressed prefix {end}"
        );
    }
}

#[test]
fn predicted_pattern_rows_reset_and_wrap_at_their_original_channel_precision() {
    use std::io::Write;
    for depth in [8, 16] {
        let (bytes, mut expected) = fixture(depth, 3, "synthetic-pattern");
        let samples = [65013u16, 17, 64121, 997, 64531, 233];
        let mut raw = Vec::new();
        for (y, row) in samples.chunks_exact(3).enumerate() {
            let mut previous = 0u16;
            for (i, &value) in row.iter().enumerate() {
                let value = if depth == 16 {
                    value
                } else {
                    u16::from(raster::project16(value)) * 257
                };
                if depth == 16 {
                    raw.extend(value.wrapping_sub(previous).to_be_bytes());
                } else {
                    raw.push(raster::project16(value).wrapping_sub(raster::project16(previous)));
                }
                previous = value;
                expected[(y * 3 + i) * 4] = value;
            }
        }
        let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        zip.write_all(&raw).unwrap();
        let modified = replace_first_channel_payload(&bytes, &zip.finish().unwrap());
        let sources = patterns::decode(&modified, "Patt", &Control::default()).unwrap();
        assert_eq!(sources[0].pixels.rgba16(), expected);
    }
}
