use base64::{engine::general_purpose::STANDARD, Engine as _};
use peerbrush::raster::{self, Raster, TILE};
use serde_json::json;
use std::sync::Arc;

#[test]
fn sixteen_bit_sparse_storage_roundtrips_lower_bits_hidden_rgb_and_cow() {
    let mut r = Raster::new_depth(513, 259, 16);
    r.set16(512, 258, [12345, 23456, 34567, 1]);
    r.set16(256, 1, [91, 123, 257, 0]);
    assert!(r.tiles.is_empty());
    assert_eq!(r.samples16.len(), 2);
    assert_eq!(r.get16(512, 258), [12345, 23456, 34567, 1]);
    let before = r.clone();
    let words = r.rgba16();
    let imported = Raster::from_rgba16(513, 259, &words).unwrap();
    assert_eq!(imported.rgba16(), words);
    assert_eq!(imported.get16(256, 1), [91, 123, 257, 0]);
    for (key, tile) in &r.samples16 {
        assert!(Arc::ptr_eq(tile, &before.samples16[key]));
    }
    r.set16(512, 258, [12346, 23457, 34568, 2]);
    assert_eq!(before.get16(512, 258), [12345, 23456, 34567, 1]);
    assert_eq!(r.get16(512, 258), [12346, 23457, 34568, 2]);
    assert_eq!(r.content_bounds(), Some([512, 258, 513, 259]));
    assert_eq!(r.bytes(), 2 * (TILE * TILE * 8) as usize);
    let json = serde_json::to_vec(&r).unwrap();
    let loaded: Raster = serde_json::from_slice(&json).unwrap();
    assert_eq!(loaded.rgba16(), r.rgba16());
    assert!(loaded.tiles.is_empty());
    loaded.validate_layout().unwrap();
}

#[test]
fn sixteen_bit_import_clips_padding_and_projection_never_becomes_authoritative() {
    let mut r = Raster::from_rgba16(1, 1, &[12345, 23456, 34567, 45678]).unwrap();
    let original = r.rgba16();
    let projected = r.rgba();
    assert_eq!(projected, [48, 91, 135, 178]);
    assert_eq!(r.rgba16(), original);
    Arc::make_mut(r.samples16.get_mut(&(0, 0)).unwrap())[4..8].copy_from_slice(&[65535; 4]);
    r.samples16.insert(
        (u32::MAX, u32::MAX),
        Arc::new(vec![65535; (TILE * TILE * 4) as usize]),
    );
    assert_eq!(r.rgba16(), original);
    assert_eq!(r.rgba(), projected);
    assert_eq!(r.tile_bounds(), Some([0, 0, 1, 1]));
    assert!(r.validate_layout().is_err());
    let mut eight = Raster::from_rgba(1, 1, &[7, 11, 19, 29]).unwrap();
    eight.promote16();
    assert_eq!(eight.get16(0, 0), [7 * 257, 11 * 257, 19 * 257, 29 * 257]);
    assert!(eight.tiles.is_empty());
    assert_eq!(eight.rgba(), [7, 11, 19, 29]);
    eight.set(0, 0, [43, 47, 53, 59]);
    assert_eq!(eight.get16(0, 0), [43 * 257, 47 * 257, 53 * 257, 59 * 257]);
}

#[test]
fn high_depth_sampling_and_blending_keep_sub_eight_bit_changes() {
    let r = Raster::from_rgba16(
        2,
        1,
        &[10001, 20001, 30001, 65535, 10003, 20003, 30003, 65535],
    )
    .unwrap();
    assert_eq!(r.sample16(0.5, 0.), [10002, 20002, 30002, 65535]);
    let mixed = raster::blend16(
        [10001, 20001, 30001, 65535],
        [10003, 20003, 30003, 65535],
        0.5,
        "normal",
    );
    assert_eq!(mixed, [10002, 20002, 30002, 65535]);
    let faint = raster::blend16([0; 4], [12345, 23456, 34567, 1], 1., "normal");
    assert_eq!(faint, [12345, 23456, 34567, 1]);
    let encoded = raster::png16(2, 1, &r.rgba16()).unwrap();
    let decoded = image::load_from_memory(&encoded).unwrap().to_rgba16();
    assert_eq!(decoded.as_raw(), &r.rgba16());
}

#[test]
fn legacy_dot_paint_and_erase_preserve_untouched_native_channels() {
    let mut r = Raster::from_rgba16(1, 1, &[12345, 23456, 34567, 45678]).unwrap();
    r.paint_dot(0.5, 0.5, 2., [0, 0, 0, 1], false, true, None);
    let erased = r.get16(0, 0);
    assert_eq!(&erased[..3], &[12345, 23456, 34567]);
    assert_eq!(erased[3], (45678f64 * (1. - 1. / 255.)).round() as u16);
    assert_ne!(erased[3] % 257, 0);
    r.paint_dot16(
        0.5,
        0.5,
        2.,
        [10001, 20001, 30001, 65535],
        false,
        false,
        None,
    );
    assert_eq!(r.get16(0, 0), [10001, 20001, 30001, 65535]);
}

fn serialized_tiles(depth: u16, entries: serde_json::Value) -> serde_json::Value {
    let mut value = json!({"width":257,"height":1,"depth":depth,"tiles":[]});
    value[if depth == 16 { "samples16" } else { "tiles" }] = entries;
    value
}

#[test]
fn tile_sequences_keep_mixed_legacy8_compact_last_entry_and_canonical_wire() {
    let empty: Raster = serde_json::from_value(json!({"width":1,"height":1,"tiles":[]})).unwrap();
    assert_eq!(empty, Raster::new(1, 1));
    let mut first = vec![0u8; (TILE * TILE * 4) as usize];
    first[..4].copy_from_slice(&[7, 11, 19, 0]);
    let mut last = first.clone();
    last[..4].copy_from_slice(&[41, 43, 47, 1]);
    let end = last.len();
    last[end - 4..].copy_from_slice(&[51, 53, 59, 0]);
    let input = json!({"width":257,"height":1,"tiles":[
        [1,0,STANDARD.encode(&first)],[0,0,first],[1,0,STANDARD.encode(&last)]]});
    let loaded: Raster = serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(loaded.tiles.len(), 2);
    assert_eq!(loaded.tiles[&(0, 0)].as_slice(), first.as_slice());
    assert_eq!(loaded.tiles[&(1, 0)].as_slice(), last.as_slice());
    assert_eq!(loaded.get(0, 0), [7, 11, 19, 0]);
    assert_eq!(loaded.get(256, 0), [41, 43, 47, 1]);
    loaded.validate_layout().unwrap();
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serialized_tiles(
            8,
            json!([
                [0, 0, STANDARD.encode(&first)],
                [1, 0, STANDARD.encode(&last)]
            ])
        )
    );
}

#[test]
fn tile_sequences_restore_exact_native16_words_hidden_rgb_and_wire_order() {
    let mut first: Vec<u16> = (0..TILE * TILE * 4)
        .map(|at| (at as u16).wrapping_mul(971).wrapping_add(91))
        .collect();
    first[..4].copy_from_slice(&[91, 123, 257, 0]);
    let mut second = first.clone();
    second[..4].copy_from_slice(&[12345, 23457, 34569, 1]);
    let encode = |words: &[u16]| {
        STANDARD.encode(
            words
                .iter()
                .flat_map(|word| word.to_be_bytes())
                .collect::<Vec<_>>(),
        )
    };
    let a = encode(&first);
    let b = encode(&second);
    let input = serialized_tiles(16, json!([[1, 0, b], [0, 0, a]]));
    let loaded: Raster = serde_json::from_slice(&serde_json::to_vec(&input).unwrap()).unwrap();
    assert_eq!(loaded, serde_json::from_value::<Raster>(input).unwrap());
    assert!(loaded.tiles.is_empty());
    assert_eq!(loaded.samples16.len(), 2);
    assert_eq!(loaded.samples16[&(0, 0)].as_slice(), first.as_slice());
    assert_eq!(loaded.samples16[&(1, 0)].as_slice(), second.as_slice());
    assert_eq!(loaded.get16(0, 0), [91, 123, 257, 0]);
    assert_eq!(loaded.get16(256, 0), [12345, 23457, 34569, 1]);
    loaded.validate_layout().unwrap();
    assert_eq!(
        serde_json::to_value(&loaded).unwrap(),
        serialized_tiles(16, json!([[0, 0, a], [1, 0, b]]))
    );
}

#[test]
fn tile_sequences_keep_encoded_decoded_tuple_and_duplicate16_rejections() {
    for depth in [8, 16] {
        let bytes = (TILE * TILE * 4 * u32::from(depth / 8)) as usize;
        let length_error = if depth == 16 {
            "Invalid16-bit tile length"
        } else {
            "Invalid tile length"
        };
        let oversized_error = if depth == 16 {
            "Oversized16-bit tile"
        } else {
            "Oversized tile"
        };
        for (data, expected) in [
            (String::new(), length_error),
            (STANDARD.encode(vec![0; bytes - 1]), length_error),
            (STANDARD.encode(vec![0; bytes + 1]), length_error),
            ("A".repeat(bytes.div_ceil(3) * 4 + 1), oversized_error),
        ] {
            let error =
                serde_json::from_value::<Raster>(serialized_tiles(depth, json!([[0, 0, data]])))
                    .unwrap_err();
            assert!(error.to_string().starts_with(expected), "{error}");
        }
        for entries in [
            json!([[0, 0, "!"]]),
            json!([[0, 0]]),
            json!([[0, 0, "", 0]]),
            json!([[-1, 0, ""]]),
            json!([[0, 0, [0, 1, 2]]]),
        ] {
            assert!(serde_json::from_value::<Raster>(serialized_tiles(depth, entries)).is_err());
        }
        if depth == 8 {
            let error = serde_json::from_value::<Raster>(serialized_tiles(
                depth,
                json!([[0, 0, vec![0u8; bytes + 1]]]),
            ))
            .unwrap_err();
            assert!(error.to_string().starts_with(oversized_error));
        }
        let data = STANDARD.encode(vec![0; bytes]);
        if depth == 16 {
            let error = serde_json::from_value::<Raster>(serialized_tiles(
                depth,
                json!([[0, 0, data], [0, 0, data]]),
            ))
            .unwrap_err();
            assert!(error.to_string().starts_with("Duplicate16-bit tile"));
        }
        let mut trailing =
            serde_json::to_vec(&serialized_tiles(depth, json!([[0, 0, data]]))).unwrap();
        trailing.extend(b" null");
        assert!(serde_json::from_slice::<Raster>(&trailing).is_err());
    }
}

#[test]
fn legacy_tile_rejects_first_excess_byte_before_reading_remaining_array() {
    use std::io::Cursor;
    let limit = (TILE * TILE * 4) as usize;
    let mut input = br#"{"width":1,"height":1,"tiles":[[0,0,["#.to_vec();
    input.extend_from_slice("0,".repeat(limit).as_bytes());
    input.push(b'0');
    let first_excess = input.len();
    input.extend_from_slice(
        b",0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]]]} ",
    );
    let mut reader = Cursor::new(input);
    let error = serde_json::from_reader::<_, Raster>(&mut reader).unwrap_err();
    assert!(error.to_string().starts_with("Oversized tile"), "{error}");
    // serde_json may read the delimiter while parsing the first excess number.
    assert!(reader.position() as usize <= first_excess + 2);
}

#[test]
fn tile_sequences_validate_current_tuple_before_reading_later_entries() {
    for (depth, field, expected) in [
        (8, "tiles", "Invalid tile length"),
        (16, "samples16", "Invalid16-bit tile length"),
    ] {
        let mut input = format!("{{\"width\":1,\"height\":1,\"depth\":{depth},");
        if depth == 16 {
            input.push_str("\"tiles\":[],");
        }
        input.push_str(&format!("\"{field}\":[[0,0,\"\"],"));
        // There is deliberately no next tuple or JSON end. Collecting all
        // encoded entries would report EOF before validating this first tile.
        let error = serde_json::from_slice::<Raster>(input.as_bytes()).unwrap_err();
        assert!(error.to_string().starts_with(expected), "{error}");
    }
}

#[test]
fn tile_sequence_decode_retains_the_existing_outer_layout_validation_boundary() {
    let eight = STANDARD.encode(vec![0; (TILE * TILE * 4) as usize]);
    let sixteen = STANDARD.encode(vec![0; (TILE * TILE * 8) as usize]);
    for depth in [8, 16] {
        let encoded = if depth == 16 { &sixteen } else { &eight };
        let loaded: Raster = serde_json::from_value(serialized_tiles(
            depth,
            json!([[u32::MAX, u32::MAX, encoded]]),
        ))
        .unwrap();
        assert_eq!(
            loaded.validate_layout().unwrap_err(),
            "Raster tile lies outside its image"
        );
        let mixed = json!({"width":1,"height":1,"depth":depth,
            "tiles":[[0,0,eight]],"samples16":[[0,0,sixteen]]});
        let loaded: Raster = serde_json::from_value(mixed).unwrap();
        assert_eq!(
            loaded.validate_layout().unwrap_err(),
            "Raster storage does not match its bit depth"
        );
    }
}
