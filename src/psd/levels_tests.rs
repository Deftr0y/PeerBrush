use super::*;
use super::super::*;
use serde_json::json;

fn payload(master: [u16; 5]) -> Vec<u8> {
    let mut bytes = vec![0, 2];
    for i in 0..29 {
        for value in if i == 0 { master } else if i < 27 { [0, 255, 0, 255, 100] } else { [0; 5] } {
            bytes.extend(value.to_be_bytes());
        }
    }
    bytes
}

fn standard_only(bytes: &[u8]) -> Vec<u8> {
    let mut reader = Cursor::new(bytes);
    reader.bytes(26).unwrap();
    reader.block().unwrap();
    let start = reader.pos;
    reader.block().unwrap();
    let mut result = bytes[..start].to_vec();
    u32b(&mut result, 0);
    result.extend_from_slice(&bytes[reader.pos..]);
    result
}

#[test]
fn master_curves_binds_standard_points_and_refuses_conflicting_records() {
    let settings = json!({"interpolation":"linear","points":[[0.,17./255.],[1.,239./255.]]});
    let payload = curves_descriptor(&settings).unwrap();
    assert_eq!(curves_settings(&payload).unwrap(), settings);
    assert_eq!(curves_settings(&payload[..17]).unwrap(), settings);
    let mut neutral_rgb = payload[..17].to_vec();
    neutral_rgb[6] = 15;
    for _ in 0..3 {
        neutral_rgb.extend_from_slice(&[0, 2, 0, 0, 0, 0, 0, 255, 0, 255]);
    }
    assert_eq!(curves_settings(&neutral_rgb).unwrap(), settings);
    neutral_rgb[20] = 1;
    assert_eq!(curves_settings(&neutral_rgb).unwrap()["composition"], curves::PHOTOSHOP_BYTE);
    for at in [0, 8, 12, 28, 32, 39] {
        let mut changed = payload.clone();
        changed[at] ^= 1;
        assert!(curves_settings(&changed).is_err());
    }
    let mut smooth = settings.clone();
    smooth["interpolation"] = json!("smooth");
    assert!(curves_descriptor(&smooth).is_none());

    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
        doc.layers[0].pixels.set16(1, 0, [0, 65535, 0, 65535]);
        crate::adjustment::add(&mut doc, &json!({"kind":"curves","settings":settings})).unwrap();
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&doc, layout).unwrap();
            let plain = standard_only(&bytes);
            for restored in [
                decode(&bytes).unwrap(),
                decode(&plain).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap(),
            ] {
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(restored.layers[0].kind, "adjustment");
                assert_eq!(*restored.layers[0].effects[0].settings, settings);
                assert_eq!(restored.preview(None, 8, None, false).unwrap().2,
                    doc.preview(None, 8, None, false).unwrap().2);
            }
            let old = super::tests::replace_sources(&bytes, layout, &doc, 51);
            assert!(decode(&old).unwrap().read_only);
            let index = structural::index(
                &mut std::io::Cursor::new(&bytes), structural::Limits::current(), || Ok(()),
            ).unwrap();
            let tag = index.layers[0].tags.iter().find(|tag| tag.key == *b"curv").unwrap();
            let at = tag.data.offset as usize;
            assert_eq!(&bytes[at..at + 40], payload.as_slice());
            let mut changed = bytes.clone();
            changed[at + 10] ^= 1;
            changed[at + 32] ^= 1;
            assert!(curves_settings(&changed[at..at + 40]).is_ok());
            let rebound = super::tests::replace_sources(&changed, layout, &doc, 52);
            assert!(decode(&rebound).unwrap().read_only);
        }
    }
}

#[test]
fn rgb_curves_preserve_independent_psd_records_and_native_samples() {
    let identity = json!({"interpolation":"linear","points":[[0.,0.],[1.,1.]]});
    let mut settings = identity.clone();
    settings["channels"] = json!([
        {"interpolation":"linear","points":[[0.,1.],[1.,0.]]},
        identity,
        {"interpolation":"linear","points":[[0.,64./255.],[1.,192./255.]]},
    ]);
    let payload = curves_descriptor(&settings).unwrap();
    assert_eq!(payload.len(), 108);
    assert_eq!(curves_settings(&payload).unwrap(), settings);
    assert_eq!(curves_settings(&payload[..47]).unwrap(), settings);
    let mut changed = payload.clone();
    changed[60] ^= 1;
    assert!(curves_settings(&changed).is_err());
    let mut mixed = settings.clone();
    mixed["points"][0][1] = json!(17./255.);
    assert!(curves_descriptor(&mixed).is_none());
    assert!(crate::mask::commands::validate_settings("curves", &settings).is_err());

    let mut image = crate::effects::Image { width: 1, height: 1, bytes: vec![18, 53, 255, 127] };
    crate::effects::apply_cpu(&mut image, "curves", &settings).unwrap();
    assert_eq!(image.bytes, [237, 53, 192, 127]);
    let mut image = crate::depth16::Image16 { width: 1, height: 1, words: vec![12347, 23459, 65535, 34001] };
    crate::depth16::color::apply_cpu(&mut image, "curves", &settings).unwrap();
    assert_eq!(image.words, [65535 - 12347, 23459, 192 * 257, 34001]);

    for depth in [8, 16] {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, [12347, 23459, 34571, 65535]);
        crate::adjustment::add(&mut doc, &json!({"kind":"curves","settings":settings})).unwrap();
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&doc, layout).unwrap();
            let plain = standard_only(&bytes);
            for restored in [
                decode(&bytes).unwrap(),
                decode(&plain).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap(),
            ] {
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(*restored.layers[0].effects[0].settings, settings);
            }
            let old = super::tests::replace_sources(&bytes, layout, &doc, 52);
            assert!(decode(&old).unwrap().read_only);
        }
    }
}

#[test]
fn shifted_curves_preserve_input_points_and_bind_the_new_standard_layout() {
    let settings = json!({"interpolation":"linear","points":[[51./255.,17./255.],[204./255.,239./255.]]});
    let payload = curves_descriptor(&settings).unwrap();
    assert_eq!(&payload[7..17], &[0, 2, 0, 17, 0, 51, 0, 239, 0, 204]);
    assert_eq!(curves_settings(&payload).unwrap(), settings);
    for invalid in [[0, 204], [0, 205], [1, 0]] {
        let mut primary = payload[..17].to_vec();
        primary[11..13].copy_from_slice(&invalid);
        assert!(curves_settings(&primary).is_err());
    }
    let mut conflict = payload.clone();
    conflict[12] += 1;
    assert!(curves_settings(&conflict).is_err());
    let mut rgb = json!({"interpolation":"linear","points":[[0.,0.],[1.,1.]]});
    rgb["channels"] = json!([settings, rgb.clone(), rgb.clone()]);
    assert_eq!(curves_settings(&curves_descriptor(&rgb).unwrap()).unwrap(), rgb);

    let mut image = crate::depth16::Image16 { width: 1, height: 1, words: vec![0, 26215, 65535, 65535] };
    crate::depth16::color::apply_cpu(&mut image, "curves", &settings).unwrap();
    assert_eq!(image.words, [17 * 257, 91 * 257 + 1, 239 * 257, 65535]);

    for depth in [8, 16] {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, [0, 26215, 65535, 65535]);
        let original = doc.layers[0].pixels.get16(0, 0);
        crate::adjustment::add(&mut doc, &json!({"kind":"curves","settings":settings})).unwrap();
        assert!(standard_plan_for(&doc, true, true, ToneRecords::for_source_format(53, depth)).composite);
        assert!(!standard_plan(&doc).composite);
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&doc, layout).unwrap();
            let plain = standard_only(&bytes);
            for restored in [decode(&bytes).unwrap(), decode(&plain).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap()] {
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(*restored.layers[0].effects[0].settings, settings);
                assert_eq!(restored.layers[1].pixels.get16(0, 0), original);
            }
            let downgraded = super::tests::replace_sources(&bytes, layout, &doc, 53);
            assert!(decode(&downgraded).unwrap().read_only);
        }
    }
}

#[test]
fn photoshop_curves_accept_sparse_rgb_records_with_an_implicit_master() {
    // Photoshop writes only the changed Red and Blue curves in both tables.
    let payload = [
        0, 0, 1, 0, 0, 0, 10, 0, 3, 0, 20, 0, 0, 0, 150, 0, 128, 0, 255, 0, 255,
        0, 3, 0, 0, 0, 0, 0, 100, 0, 128, 0, 230, 0, 255,
        b'C', b'r', b'v', b' ', 0, 4, 0, 0, 0, 2,
        0, 1, 0, 3, 0, 20, 0, 0, 0, 150, 0, 128, 0, 255, 0, 255,
        0, 3, 0, 3, 0, 0, 0, 0, 0, 100, 0, 128, 0, 230, 0, 255, 0, 0, 0,
    ];
    let settings = curves_settings(&payload).unwrap();
    assert_eq!(settings["points"], json!([[0.,0.],[1.,1.]]));
    assert_eq!(settings["channels"][1], json!({"interpolation":"linear","points":[[0.,0.],[1.,1.]]}));
    assert_eq!(curves_settings(&curves_descriptor(&settings).unwrap()).unwrap(), settings);
    let mut image = crate::effects::Image { width: 2, height: 1, bytes: vec![249,249,249,255,203,255,0,127] };
    crate::effects::apply_cpu(&mut image, "curves", &settings).unwrap();
    assert_eq!(image.bytes, [250,249,223,255,214,255,0,127]);

    for channel in [0, 2, 3, 4] {
        let mut wrong_extension = payload;
        wrong_extension[46] = channel;
        assert!(curves_settings(&wrong_extension).is_err());
    }
    let mut non_rgb = payload;
    non_rgb[6] |= 16;
    assert!(curves_settings(&non_rgb).is_err());
}

#[test]
fn combined_photoshop_curves_round_each_rgb8_stage_and_preserve_the_record() {
    let mut settings = json!({"composition":curves::PHOTOSHOP_BYTE,"interpolation":"linear",
        "points":[[0.,0.],[200./255.,1.]],"channels":[
            {"interpolation":"photoshop","points":[[49./255.,60./255.],[94./255.,218./255.],
                [102./255.,0.],[185./255.,229./255.],[195./255.,36./255.]]},
            {"interpolation":"linear","points":[[0.,0.],[1.,213./255.]]},
            {"interpolation":"photoshop","points":[[0.,0.],[80./255.,117./255.],
                [190./255.,153./255.],[1.,1.]]}
        ]});
    let payload = curves_descriptor(&settings).unwrap();
    assert_eq!(curves_settings(&payload).unwrap(), settings);
    let input = [0, 72, 170, 255, 63, 28, 1, 255];
    let mut image = crate::effects::Image { width: 2, height: 1, bytes: input.to_vec() };
    crate::effects::apply_cpu(&mut image, "curves", &settings).unwrap();
    assert_eq!(image.bytes, [76, 76, 178, 255, 255, 29, 3, 255]);

    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        for (x, pixel) in input.chunks_exact(4).enumerate() {
            let words = <[u8; 4]>::try_from(pixel).unwrap().map(|v| u16::from(v) * 257);
            doc.layers[0].pixels.set16(x as i32, 0, words);
        }
        crate::adjustment::add(&mut doc, &json!({"kind":"curves","settings":settings})).unwrap();
        assert_eq!(tone_source_format(&doc), Some(59));
        assert_eq!(standard_plan(&doc).composite, depth == 16);
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&doc, layout).unwrap();
            let restored = decode(&bytes).unwrap();
            assert!(!restored.read_only, "{:?}", restored.warnings);
            assert_eq!(*restored.layers[0].effects[0].settings, settings);
            let old = super::tests::replace_sources(&bytes, layout, &doc, 58);
            assert!(decode(&old).unwrap().read_only);
            if depth == 8 {
                let plain = standard_only(&bytes);
                let restored = decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap();
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(*restored.layers[0].effects[0].settings, settings);
            }
        }
    }
    settings["channels"][1]["points"][1][1] = json!(0.1);
    assert!(crate::effects::validate("curves", &settings).is_err());
}

#[test]
fn photoshop_spline_curves_match_rgb8_and_bind_the_standard_record() {
    let settings = json!({"interpolation":"photoshop","points":[
        [0.,0.],[60./255.,35./255.],[190./255.,225./255.],[1.,1.]
    ]});
    let payload = curves_descriptor(&settings).unwrap();
    assert_eq!(&payload[7..25], &[0,4,0,0,0,0,0,35,0,60,0,225,0,190,0,255,0,255]);
    assert_eq!(curves_settings(&payload).unwrap(), settings);
    assert_eq!(curves_settings(&payload[..25]).unwrap(), settings);
    let mut changed = payload.clone();
    changed[44] += 1;
    assert!(curves_settings(&changed).is_err());
    let mut duplicate = payload[..25].to_vec();
    duplicate[15..17].fill(0);
    assert!(curves_settings(&duplicate).is_err());
    let identity = json!({"interpolation":"linear","points":[[0.,0.],[1.,1.]]});
    let mut rgb = identity.clone();
    rgb["channels"] = json!([settings, identity, identity]);
    assert_eq!(curves_settings(&curves_descriptor(&rgb).unwrap()).unwrap(), rgb);
    assert!(crate::mask::commands::validate_settings("curves", &settings).is_err());

    let mut image = crate::effects::Image { width: 1, height: 1, bytes: vec![31, 85, 200, 137] };
    crate::effects::apply_cpu(&mut image, "curves", &settings).unwrap();
    assert_eq!(image.bytes, [13, 66, 233, 137]);
    for depth in [8, 16] {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, [31 * 257, 85 * 257, 200 * 257, 65535]);
        let original = doc.layers[0].pixels.rgba16();
        crate::adjustment::add(&mut doc, &json!({"kind":"curves","settings":settings})).unwrap();
        assert_eq!(tone_source_format(&doc), Some(58));
        assert_eq!(standard_plan(&doc).composite, depth == 16);
        for layout in [Layout::Psd, Layout::Psb] {
            let encoded = encode_layout(&doc, layout).unwrap();
            let restored = decode(&encoded).unwrap();
            assert!(!restored.read_only, "{:?}", restored.warnings);
            assert_eq!(*restored.layers[0].effects[0].settings, settings);
            assert_eq!(restored.layers[1].pixels.rgba16(), original);
            let downgraded = super::tests::replace_sources(&encoded, layout, &doc, 57);
            assert!(decode(&downgraded).unwrap().read_only);
            let plain = standard_only(&encoded);
            let index = structural::index(&mut std::io::Cursor::new(&plain), structural::Limits::current(), || Ok(())).unwrap();
            assert_eq!(index.layers.iter().any(|layer| layer.tags.iter().any(|tag| tag.key == *b"curv")), depth == 8);
            if depth == 8 {
                let standard = decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap();
                assert!(!standard.read_only, "{:?}", standard.warnings);
                assert_eq!(*standard.layers[0].effects[0].settings, settings);
            }
        }
    }
}

#[test]
fn photoshop_posterize_roundtrips_only_calibrated_standard_records() {
    let modern = json!({"levels":3,"mapping":"photoshop_native"});
    assert_eq!(posterize_settings(&[0, 3]).unwrap(), modern);
    assert_eq!(posterize_settings(&[0, 3, 0, 0]).unwrap(), modern);
    for malformed in [&[0, 1][..], &[1, 0], &[0, 3, 0], &[0, 3, 0, 1], &[0, 3, 0, 0, 0]] {
        assert!(posterize_settings(malformed).is_err());
    }
    assert!(posterize_descriptor(&json!({"levels":3}), 8).is_none());
    let legacy = json!({"levels":3,"mapping":"photoshop"});
    for (depth, settings, format) in [(8, &modern, 56), (16, &modern, 56), (8, &legacy, 55), (16, &legacy, 55)] {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, [85 * 257, 86 * 257, 171 * 257, 65535]);
        let original = doc.layers[0].pixels.get16(0, 0);
        crate::adjustment::add(&mut doc, &json!({"kind":"posterize","settings":settings})).unwrap();
        let standard = depth == 8 || format == 56;
        assert_eq!(tone_source_format(&doc), Some(format));
        assert_eq!(standard_plan(&doc).composite, !standard);
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&doc, layout).unwrap();
            let restored = decode(&bytes).unwrap();
            assert!(!restored.read_only, "{:?}", restored.warnings);
            assert_eq!(&*restored.layers[0].effects[0].settings, settings);
            assert_eq!(restored.layers[1].pixels.get16(0, 0), original);
            let old = super::tests::replace_sources(&bytes, layout, &doc, format - 1);
            assert!(decode(&old).unwrap().read_only);

            let mut plain = standard_only(&bytes);
            let index = structural::index(&mut std::io::Cursor::new(&plain), structural::Limits::current(), || Ok(())).unwrap();
            let tag = index.layers.iter().flat_map(|layer| &layer.tags).find(|tag| tag.key == *b"post");
            assert_eq!(tag.is_some(), standard);
            if let Some(tag) = tag {
                for restored in [decode(&plain).unwrap(), decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap()] {
                    assert!(!restored.read_only, "{:?}", restored.warnings);
                    assert_eq!(*restored.layers[0].effects[0].settings, modern);
                    assert_eq!(restored.layers[1].pixels.get16(0, 0), original);
                }
                // Another valid count must still agree with every saved sample.
                plain[tag.data.offset as usize + 1] = 2;
                assert!(decode(&plain).unwrap().read_only);
            }
        }
    }
}

#[test]
fn photoshop_threshold_preserves_depth_specific_ties_and_standard_settings() {
    let mut bytes = crate::effects::Image {
        width: 2, height: 1, bytes: vec![131, 109, 99, 137, 131, 109, 100, 17],
    };
    crate::effects::apply_cpu(&mut bytes, "threshold", &json!({"level":115})).unwrap();
    assert_eq!(bytes.bytes, [0, 0, 0, 137, 255, 255, 255, 17]);

    let mut words = crate::depth16::Image16 {
        width: 2, height: 1, words: vec![32894, 32894, 32894, 33003, 32895, 32895, 32895, 27139],
    };
    crate::depth16::color::apply_cpu(&mut words, "threshold", &json!({"level":128})).unwrap();
    assert_eq!(words.words, [0, 0, 0, 33003, 65535, 65535, 65535, 27139]);
    for invalid in [json!({"level":0}), json!({"level":256}), json!({"level":7.5}), json!({"unknown":128})] {
        assert!(crate::effects::validate("threshold", &invalid).is_err());
    }

    let settings = json!({"level":115});
    assert_eq!(threshold_settings(&[0, 115]).unwrap(), settings);
    assert_eq!(threshold_settings(&[0, 115, 0, 0]).unwrap(), settings);
    for malformed in [&[0, 0][..], &[1, 0], &[0, 115, 0], &[0, 115, 1, 0], &[0, 115, 0, 0, 0]] {
        assert!(threshold_settings(malformed).is_err());
    }
    for depth in [8, 16] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        for (x, value) in [114 * 257, 115 * 257].into_iter().enumerate() {
            doc.layers[0].pixels.set16(x as i32, 0, [value, value, value, 65535]);
        }
        let original = doc.layers[0].pixels.rgba16();
        crate::adjustment::add(&mut doc, &json!({"kind":"threshold","settings":settings})).unwrap();
        assert_eq!(tone_source_format(&doc), Some(57));
        assert!(!standard_plan(&doc).composite);
        for layout in [Layout::Psd, Layout::Psb] {
            let encoded = encode_layout(&doc, layout).unwrap();
            let mut plain = standard_only(&encoded);
            for restored in [decode(&encoded).unwrap(), decode(&plain).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap()] {
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(restored.layers[0].effects[0].kind, "threshold");
                assert_eq!(*restored.layers[0].effects[0].settings, settings);
                assert_eq!(restored.layers[1].pixels.rgba16(), original);
            }
            let old = super::tests::replace_sources(&encoded, layout, &doc, 56);
            assert!(decode(&old).unwrap().read_only);
            let index = structural::index(&mut std::io::Cursor::new(&plain), structural::Limits::current(), || Ok(())).unwrap();
            let tag = index.layers.iter().flat_map(|layer| &layer.tags).find(|tag| tag.key == *b"thrs").unwrap();
            let offset = tag.data.offset as usize;
            assert_eq!(&plain[offset..offset + 4], &[0, 115, 0, 0]);
            plain[offset + 1] = 114;
            assert!(decode(&plain).unwrap().read_only);
        }
    }
}

#[test]
fn levels_records_validate_every_channel_extension_and_exact_output_mapping() {
    let bytes = payload([51, 204, 17, 239, 125]);
    let settings = levels_settings(&bytes).unwrap();
    assert_eq!(settings, json!({"black":0.2,"white":0.8,"gamma":1.25,
        "output_black":17./255.,"output_white":239./255.,"input_mapping":"exact"}));
    assert_eq!(levels_descriptor(&settings).unwrap().as_slice(), bytes);
    for length in [0, 1, 2, 291, 293, 299, 301] {
        let mut truncated = bytes.clone();
        truncated.resize(length, 0);
        assert!(levels_settings(&truncated).is_err());
    }
    for master in [[254,255,0,255,100], [0,1,0,255,100], [7,7,0,255,100],
        [0,255,256,255,100], [0,255,0,256,100], [0,255,0,255,9],
        [0,255,0,255,1000]] {
        assert!(levels_settings(&payload(master)).is_err());
    }
    for record in 1..29 {
        let mut changed = bytes.clone();
        changed[2 + record * 10] = 1;
        assert!(levels_settings(&changed).is_err(), "record {record}");
    }
    let mut extended = bytes.clone();
    extended.extend_from_slice(b"Lvls\0\x03\0\x1e");
    for value in [0u16, 255, 0, 255, 100] { extended.extend(value.to_be_bytes()); }
    assert_eq!(levels_settings(&extended).unwrap(), settings);
    *extended.last_mut().unwrap() = 99;
    assert!(levels_settings(&extended).is_err());
    extended[299] = 29;
    assert!(levels_settings(&extended).is_err());

    let mut padded = bytes.clone();
    padded.extend_from_slice(b"Lvls\0\x03\0\x3e");
    for _ in 0..33 {
        for value in IDENTITY_LEVELS { padded.extend_from_slice(&value.to_be_bytes()); }
    }
    padded.extend_from_slice(&[0, 0]);
    assert_eq!(padded.len(), 632);
    assert_eq!(levels_settings(&padded).unwrap(), settings);
    let mut invalid = padded.clone();
    invalid[631] = 1;
    assert!(levels_settings(&invalid).is_err());
    padded[629] = 99;
    assert!(levels_settings(&padded).is_err());
    padded[629] = 100;
    padded.push(0);
    assert!(levels_settings(&padded).is_err());

    assert!(levels_descriptor(&json!({"black":0.123456})).is_none());
    assert!(levels_descriptor(&json!({"channel":"red"})).is_none());
    assert!(levels_descriptor(&json!({"gamma":1.234})).is_none());
    for master in [[0,2,0,255,100], [1,2,255,0,999], [253,254,0,255,10],
        [253,255,17,239,501], [0,255,0,255,999]] {
        let bytes = payload(master);
        let settings = levels_settings(&bytes).unwrap();
        assert_eq!(settings["input_mapping"], "exact");
        assert_eq!(levels_descriptor(&settings).unwrap().as_slice(), bytes);
    }
    assert!(levels_descriptor(&json!({"black":17./255.,"white":18./255.})).is_none());
}

#[test]
fn independent_rgb_levels_roundtrip_without_master_or_extra_channel_ambiguity() {
    let mut bytes = payload([0,255,0,255,100]);
    for (i, values) in [[0u16,255,255,0,100], [0,255,51,51,100], [17,18,0,255,999]].into_iter().enumerate() {
        for (target, value) in bytes[12+i*10..22+i*10].chunks_exact_mut(2).zip(values) {
            target.copy_from_slice(&value.to_be_bytes());
        }
    }
    let settings = levels_settings(&bytes).unwrap();
    assert_eq!(levels_descriptor(&settings).unwrap().as_slice(), bytes);
    let mut combined = bytes.clone();
    combined[11] = 125;
    assert!(levels_settings(&combined).is_err());
    let mut combined_settings = settings.clone();
    combined_settings["gamma"] = json!(1.25);
    assert!(levels_descriptor(&combined_settings).is_none());
    for offset in [12,22,32,42] {
        let mut invalid = bytes.clone();
        invalid[offset] = 1;
        assert!(levels_settings(&invalid).is_err());
    }
    for depth in [8,16] {
        let mut doc = Document::new_depth(2,1,depth).unwrap();
        doc.layers[0].pixels.set16(0,0,[0,65535,17*257,65535]);
        doc.layers[0].pixels.set16(1,0,[65535,0,18*257,65535]);
        crate::adjustment::add(&mut doc,&json!({"kind":"levels","settings":settings})).unwrap();
        for layout in [Layout::Psd,Layout::Psb] {
            let plain = standard_only(&encode_layout(&doc,layout).unwrap());
            for restored in [decode(&plain).unwrap(),decode_reader(&mut std::io::Cursor::new(&plain),&Default::default()).unwrap()] {
                assert!(!restored.read_only,"{:?}",restored.warnings);
                assert_eq!(*restored.layers[0].effects[0].settings,settings);
                let rendered = crate::depth16::render(&restored).unwrap();
                assert_eq!(rendered.get(0,0),[65535,13107,0,65535]);
                assert_eq!(rendered.get(1,0),[0,13107,65535,65535]);
            }
            let index = structural::index(&mut std::io::Cursor::new(&plain),structural::Limits::current(),|| Ok(())).unwrap();
            let tag = index.layers.iter().flat_map(|layer| &layer.tags).find(|tag| tag.key == *b"levl").unwrap();
            let mut ambiguous = plain.clone();
            ambiguous[tag.data.offset as usize+11] = 125;
            // Import must protect unsupported master/channel combinations.
            assert!(decode(&ambiguous).unwrap().read_only);
            assert!(decode_reader(&mut std::io::Cursor::new(&ambiguous),&Default::default()).unwrap().read_only);
        }
    }
}

#[test]
fn combined_linear_photoshop_levels_round_each_rgb8_stage() {
    let mut bytes = payload([48, 135, 74, 141, 100]);
    for (channel, values) in [[81u16, 255, 99, 192, 100], [0, 128, 0, 255, 100]].into_iter().enumerate() {
        for (slot, value) in bytes[12 + channel * 10..22 + channel * 10].chunks_exact_mut(2).zip(values) {
            slot.copy_from_slice(&value.to_be_bytes());
        }
    }
    let settings = levels_settings(&bytes).unwrap();
    assert_eq!(settings["composition"], crate::effects::levels::PHOTOSHOP_BYTE);
    assert_eq!(levels_descriptor(&settings).unwrap().as_slice(), bytes);
    let mut continuous = settings.clone();
    continuous.as_object_mut().unwrap().remove("composition");
    assert!(levels_descriptor(&continuous).is_none());
    let mut nonlinear = settings.clone();
    nonlinear["channels"][0]["gamma"] = json!(2.12);
    assert!(levels_descriptor(&nonlinear).is_none());

    let input = [110, 25, 80, 255];
    for (settings, expected) in [(&settings, [126, 76, 99, 255]), (&continuous, [125, 75, 99, 255])] {
        let mut image = crate::effects::Image { width: 1, height: 1, bytes: input.to_vec() };
        crate::effects::apply_cpu(&mut image, "levels", settings).unwrap();
        assert_eq!(image.bytes, expected);
    }

    for depth in [8, 16] {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, input.map(|value| u16::from(value) * 257));
        crate::adjustment::add(&mut doc, &json!({"kind":"levels","settings":settings})).unwrap();
        assert_eq!(tone_source_format(&doc), Some(61));
        assert_eq!(standard_plan(&doc).composite, depth == 16);
        for layout in [Layout::Psd, Layout::Psb] {
            let file = encode_layout(&doc, layout).unwrap();
            let reopened = decode(&file).unwrap();
            assert!(!reopened.read_only, "{:?}", reopened.warnings);
            assert_eq!(*reopened.layers[0].effects[0].settings, settings);
            assert!(decode(&super::tests::replace_sources(&file, layout, &doc, 60)).unwrap().read_only);
            if depth == 8 {
                let conventional = standard_only(&file);
                let reopened = decode_reader(&mut std::io::Cursor::new(&conventional), &Default::default()).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(*reopened.layers[0].effects[0].settings, settings);
                assert_eq!(reopened.preview(None, 1, None, false).unwrap().2, [126, 76, 99, 255]);
                let index = structural::index(&mut std::io::Cursor::new(&conventional), structural::Limits::current(), || Ok(())).unwrap();
                let mut wrong = conventional;
                wrong[index.composite.span.offset as usize + 2] ^= 1;
                assert!(decode(&wrong).unwrap().read_only);
            }
        }
    }
}

#[test]
fn narrow_master_levels_imports_exact_native_input_boundaries() {
    let settings = levels_settings(&payload([17,18,0,255,999])).unwrap();
    for depth in [8, 16] {
        let mut doc = Document::new_depth(1, 1, depth).unwrap();
        doc.layers[0].pixels.set16(0, 0, [17*257,18*257,17*257,65535]);
        crate::adjustment::add(&mut doc, &json!({"kind":"levels","settings":settings})).unwrap();
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = standard_only(&encode_layout(&doc, layout).unwrap());
            for restored in [decode(&bytes).unwrap(), decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap()] {
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(*restored.layers[0].effects[0].settings, settings);
                assert_eq!(crate::depth16::render(&restored).unwrap().get(0,0), [0,65535,0,65535]);
                assert_eq!(restored.layers[1].pixels.get16(0,0), [17*257,18*257,17*257,65535]);
            }
        }
    }
}

#[test]
fn master_levels_standard_roundtrip_requires_exact_composite_and_source_binding() {
    for depth in [8, 16] {
        for clipped in [false, true] {
            let mut doc = Document::new_depth(2, 1, depth).unwrap();
            let original = [[0, 65535, 0, 65535], [65535, 0, 65535, 65535]];
            for (x, pixel) in original.into_iter().enumerate() {
                doc.layers[0].pixels.set16(x as i32, 0, pixel);
            }
            let mut command = json!({"kind":"levels","settings":{"output_black":1.,"output_white":0.}});
            if clipped { command["layer"] = json!(doc.layers[0].id); }
            crate::adjustment::add(&mut doc, &command).unwrap();
            let mut expected_settings = (*doc.layers[0].effects[0].settings).clone();
            expected_settings["input_mapping"] = json!("exact");
            assert!(!standard_plan(&doc).composite);
            for layout in [Layout::Psd, Layout::Psb] {
                let bytes = encode_layout(&doc, layout).unwrap();
                let restored = decode(&bytes).unwrap();
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(serde_json::to_value(&restored).unwrap(), serde_json::to_value(&doc).unwrap());
                let plain = standard_only(&bytes);
                let index = structural::index(&mut std::io::Cursor::new(&plain), structural::Limits::current(), || Ok(())).unwrap();
                let tag = index.layers.iter().flat_map(|l| &l.tags).find(|t| t.key == *b"levl").unwrap();
                let start = tag.data.offset as usize;
                assert_eq!(&plain[start..start + 292], payload([0,255,255,0,100]));
                for reopened in [decode(&plain).unwrap(), decode_reader(&mut std::io::Cursor::new(&plain), &Default::default()).unwrap()] {
                    assert!(!reopened.read_only, "{:?}", reopened.warnings);
                    assert_eq!(reopened.layers[0].kind, "adjustment");
                    assert_eq!(*reopened.layers[0].effects[0].settings, expected_settings);
                    assert_eq!(reopened.layers[0].clip_to.is_some(), clipped);
                    let output = crate::depth16::render(&reopened).unwrap();
                    for (x, pixel) in original.into_iter().enumerate() {
                        assert_eq!(output.get(x as i32, 0), [65535-pixel[0],65535-pixel[1],65535-pixel[2],65535]);
                    }
                }
                let mut mismatch = plain.clone();
                mismatch[index.composite.span.offset as usize + 2] ^= 1;
                assert!(decode(&mismatch).unwrap().read_only);
                // A hidden unsupported channel still requires protection.
                let mut unsupported = plain.clone();
                unsupported[start + 12] = 1;
                assert!(decode(&unsupported).unwrap().read_only);
                let old = super::tests::replace_sources(&bytes, layout, &doc, 47);
                assert!(decode(&old).unwrap().read_only);
                let mut changed = bytes.clone();
                let index = structural::index(&mut std::io::Cursor::new(&bytes), structural::Limits::current(), || Ok(())).unwrap();
                let tag = index.layers.iter().flat_map(|l| &l.tags).find(|t| t.key == *b"levl").unwrap();
                changed[tag.data.offset as usize + 11] = 125;
                let forged = super::tests::replace_sources(&changed, layout, &doc, 48);
                assert!(decode(&forged).unwrap().read_only);
                let mut baked = Document::new_depth(2, 1, depth).unwrap();
                for (x, pixel) in original.into_iter().enumerate() {
                    baked.layers[0].pixels.set16(x as i32, 0, [65535-pixel[0],65535-pixel[1],65535-pixel[2],65535]);
                }
                let baked = encode_layout(&baked, layout).unwrap();
                let old = super::tests::replace_sources(&baked, layout, &doc, 47);
                let restored = decode(&old).unwrap();
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(serde_json::to_value(restored).unwrap(), serde_json::to_value(&doc).unwrap());
                assert!(decode(&super::tests::replace_sources(&baked, layout, &doc, 48)).unwrap().read_only);
            }
        }
    }
}
