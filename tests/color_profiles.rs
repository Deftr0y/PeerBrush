use image::{codecs::png::PngDecoder, ImageDecoder};
use peerbrush::{
    color_profile,
    engine::{Document, Engine, Scope},
    loading::Control,
    psd, server,
};
use serde_json::json;
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};

fn fixed(v: f64) -> [u8; 4] {
    ((v * 65536.).round() as i32).to_be_bytes()
}
// Independently assembled ICC v4: D50-adapted sRGB primaries, linear TRCs.
// The expected conversion below uses the published sRGB transfer function.
fn linear_profile() -> Vec<u8> {
    let mut entries = Vec::new();
    for (key, xyz) in [
        (*b"rXYZ", [0.4360747, 0.2225045, 0.0139322]),
        (*b"gXYZ", [0.3850649, 0.7168786, 0.0971045]),
        (*b"bXYZ", [0.1430804, 0.0606169, 0.7141733]),
        (*b"wtpt", [0.9642, 1., 0.8249]),
    ] {
        let mut data = b"XYZ \0\0\0\0".to_vec();
        for v in xyz {
            data.extend(fixed(v));
        }
        entries.push((key, data));
    }
    for key in [*b"rTRC", *b"gTRC", *b"bTRC"] {
        entries.push((key, b"curv\0\0\0\0\0\0\0\0".to_vec()));
    }
    let mut bytes = vec![0; 132 + entries.len() * 12];
    bytes[8..12].copy_from_slice(&0x04200000u32.to_be_bytes());
    bytes[12..24].copy_from_slice(b"mntrRGB XYZ ");
    for (i, date) in [2026u16, 10, 9, 0, 0, 0].iter().enumerate() {
        bytes[24 + i * 2..26 + i * 2].copy_from_slice(&date.to_be_bytes());
    }
    bytes[36..40].copy_from_slice(b"acsp");
    for (i, v) in [0.9642, 1., 0.8249].iter().enumerate() {
        bytes[68 + i * 4..72 + i * 4].copy_from_slice(&fixed(*v));
    }
    bytes[128..132].copy_from_slice(&(entries.len() as u32).to_be_bytes());
    for (i, (key, data)) in entries.iter().enumerate() {
        let at = 132 + i * 12;
        bytes[at..at + 4].copy_from_slice(key);
        let offset = bytes.len() as u32;
        bytes[at + 4..at + 8].copy_from_slice(&offset.to_be_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend(data);
    }
    let len = bytes.len() as u32;
    bytes[..4].copy_from_slice(&len.to_be_bytes());
    bytes
}

// Classic LUT fixtures assembled directly from the ICC encoding, independently
// of the CMS encoder. XYZ nodes are a linear D50-adapted sRGB matrix. Lab nodes
// are a constant neutral L*=50, testing both legacy lut16 and lut8 encodings.
fn lut_profile(lab: bool, words: bool, relative: bool, version: u8) -> Vec<u8> {
    let mut lut = if words {
        b"mft2".to_vec()
    } else {
        b"mft1".to_vec()
    };
    lut.extend([0, 0, 0, 0, 3, 3, 2, 0]);
    for row in 0..3 {
        for col in 0..3 {
            lut.extend(fixed(f64::from(row == col)));
        }
    }
    if words {
        lut.extend(2u16.to_be_bytes());
        lut.extend(2u16.to_be_bytes());
        for _ in 0..3 {
            lut.extend([0, 0, 255, 255]);
        }
    } else {
        for _ in 0..3 {
            lut.extend(0..=255);
        }
    }
    for r in 0..2 {
        for g in 0..2 {
            for b in 0..2 {
                if lab {
                    if words {
                        lut.extend([0x7f, 0x80, 0x80, 0, 0x80, 0]);
                    } else {
                        lut.extend([128, 128, 128]);
                    }
                } else {
                    for value in [
                        0.4360747 * r as f64 + 0.3850649 * g as f64 + 0.1430804 * b as f64,
                        0.2225045 * r as f64 + 0.7168786 * g as f64 + 0.0606169 * b as f64,
                        0.0139322 * r as f64 + 0.0971045 * g as f64 + 0.7141733 * b as f64,
                    ] {
                        lut.extend(((value * 32768.).round() as u16).to_be_bytes());
                    }
                }
            }
        }
    }
    if words {
        for _ in 0..3 {
            lut.extend([0, 0, 255, 255]);
        }
    } else {
        for _ in 0..3 {
            lut.extend(0..=255);
        }
    }
    let mut white = b"XYZ \0\0\0\0".to_vec();
    for v in [0.9642, 1., 0.8249] {
        white.extend(fixed(v));
    }
    let entries = [
        (if relative { *b"A2B1" } else { *b"A2B0" }, lut),
        (*b"wtpt", white),
    ];
    let mut profile = linear_profile()[..128].to_vec();
    profile[8] = version;
    profile[9..12].fill(0);
    profile[20..24].copy_from_slice(if lab { b"Lab " } else { b"XYZ " });
    profile.extend((entries.len() as u32).to_be_bytes());
    profile.resize(132 + entries.len() * 12, 0);
    for (i, (key, data)) in entries.iter().enumerate() {
        let at = 132 + i * 12;
        profile[at..at + 4].copy_from_slice(key);
        let offset = profile.len() as u32;
        profile[at + 4..at + 8].copy_from_slice(&offset.to_be_bytes());
        profile[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_be_bytes());
        profile.extend(data);
        profile.resize((profile.len() + 3) & !3, 0);
    }
    let size = profile.len() as u32;
    profile[..4].copy_from_slice(&size.to_be_bytes());
    profile
}

#[test]
fn classic_lut_xyz_conversion_and_explicit_psd_copy_preserve_native_depth_and_alpha() {
    for version in [2, 4] {
        for relative in [false, true] {
            let profile = lut_profile(false, true, relative, version);
            color_profile::supported(&profile).unwrap();
            let input = [12345, 23456, 34567, 45679, 12346, 23457, 34568, 1];
            let mut output = input;
            color_profile::convert16(&profile, &mut output).unwrap();
            for (source, target) in input.chunks_exact(4).zip(output.chunks_exact(4)) {
                assert_eq!(source[3], target[3]);
                for c in 0..3 {
                    assert!(
                        // The CMS interpolates a 33-point output cube. Compare
                        // its color accuracy (under 0.2%) separately from the
                        // exact alpha/original-word preservation checks.
                        target[c].abs_diff(srgb(source[c])) <= 128,
                        "{source:?} -> {target:?}, expected {} for channel {c}",
                        srgb(source[c])
                    );
                }
            }
            assert!(output[..3].iter().any(|v| v % 257 != 0));
            for depth in [8, 16] {
                let source = psd::decode(&fixture(&profile, depth)).unwrap();
                assert!(source.read_only);
                if depth == 16 {
                    assert_eq!(source.layers[0].pixels.rgba16(), PIXELS.concat());
                }
                let original = source.export_png().unwrap();
                let display = source.preview(None, 4, None, false).unwrap().2;
                let shared = shared(source.clone());
                server::compatible_copy_with_color(&shared, "qa", Some(0), true).unwrap();
                let e = shared.lock().unwrap();
                assert!(!e.doc.read_only);
                assert_eq!(e.doc.bit_depth, depth);
                assert_eq!(e.doc.preview(None, 4, None, false).unwrap().2, display);
                assert_eq!(source.export_png().unwrap(), original);
                let saved = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
                assert_eq!(saved.export_png().unwrap(), e.doc.export_png().unwrap());
            }
        }
    }
}

#[test]
fn classic_lab_luts_follow_their_table_encoding_at_both_icc_versions() {
    for version in [2, 4] {
        for words in [false, true] {
            let profile = lut_profile(true, words, true, version);
            let input = [12345, 23456, 34567, 45679];
            let mut output = input;
            color_profile::convert16(&profile, &mut output).unwrap();
            let lightness: f64 = if words { 50. } else { 128. * 100. / 255. };
            let expected = srgb((((lightness + 16.) / 116.).powi(3) * 65535.).round() as u16);
            for c in 0..3 {
                assert!(
                    output[c].abs_diff(expected) <= 30,
                    "{output:?}, expected {expected}"
                );
            }
            assert_eq!(output[3], input[3]);
            let mut bytes = [0, 99, 200, 73];
            color_profile::convert8(&profile, &mut bytes).unwrap();
            assert_eq!(bytes[3], 73);
            for c in 0..3 {
                assert!(bytes[c].abs_diff(peerbrush::raster::project16(expected)) <= 1);
            }
        }
    }
}

#[test]
fn malformed_unsupported_or_unbounded_luts_cannot_modify_pixels_or_unlock_psd_sources() {
    let valid = lut_profile(false, true, true, 4);
    let offset = u32::from_be_bytes(valid[136..140].try_into().unwrap()) as usize;
    for (at, value) in [(8, 4), (9, 4), (10, 34), (48, 255), (49, 255), (4, 1)] {
        let mut invalid = valid.clone();
        invalid[offset + at] = value;
        let original = [12345, 23456, 34567, 45679];
        let mut pixels = original;
        assert!(color_profile::convert16(&invalid, &mut pixels).is_err());
        assert_eq!(pixels, original);
        let doc = psd::decode(&fixture(&invalid, 16)).unwrap();
        assert!(doc.read_only);
        assert_eq!(doc.layers[0].pixels.rgba16(), PIXELS.concat());
        assert!(server::compatible_copy_with_color(&shared(doc), "qa", Some(0), true).is_err());
    }
    for key in [b"D2B0", b"A2B2"] {
        let mut invalid = valid.clone();
        invalid[132..136].copy_from_slice(key);
        assert!(color_profile::supported(&invalid).is_err());
    }
    let mut newer = valid;
    newer[offset..offset + 4].copy_from_slice(b"mAB ");
    assert!(color_profile::supported(&newer).is_err());
}

#[test]
fn lut_png_import_normalizes_at_original_depth_and_keeps_exact_alpha() {
    let profile = lut_profile(false, true, true, 4);
    let input = [12345, 23456, 34567, 45679, 12346, 23457, 34568, 1];
    let mut expected = input;
    color_profile::convert16(&profile, &mut expected).unwrap();
    let png = peerbrush::raster::png16_with_profile(2, 1, &input, Some(&profile)).unwrap();
    let imported = peerbrush::image_import::Encoded::png(png)
        .unwrap()
        .decode(&json!({}), peerbrush::image_import::BUDGET)
        .unwrap();
    assert_eq!(imported.depth, 16);
    assert_eq!(imported.rgba16(), expected);
    assert!(imported.rgba16().iter().any(|v| v % 257 != 0));
    let mut bytes = [48, 91, 135, 173, 3, 7, 240, 0];
    let png = peerbrush::raster::png_with_profile(2, 1, &bytes, Some(&profile)).unwrap();
    color_profile::convert8(&profile, &mut bytes).unwrap();
    let imported = peerbrush::image_import::Encoded::png(png)
        .unwrap()
        .decode(&json!({}), peerbrush::image_import::BUDGET)
        .unwrap();
    assert_eq!(imported.depth, 8);
    assert_eq!(imported.rgba(), bytes);
}
fn srgb(v: u16) -> u16 {
    let v = v as f64 / 65535.;
    let encoded = if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1. / 2.4) - 0.055
    };
    (encoded * 65535.).round() as u16
}
const PIXELS: [[u16; 4]; 4] = [
    [12345, 23456, 34567, 65535],
    [12346, 23457, 34568, 65535],
    [54321, 11223, 32123, 65535],
    [1200, 128, 60001, 65535],
];
fn resource(profile: &[u8]) -> Vec<u8> {
    let mut bytes = b"8BIM".to_vec();
    bytes.extend(1039u16.to_be_bytes());
    bytes.extend([0; 2]);
    bytes.extend((profile.len() as u32).to_be_bytes());
    bytes.extend(profile);
    if profile.len() % 2 != 0 {
        bytes.push(0);
    }
    bytes
}
fn fixture(profile: &[u8], depth: u16) -> Vec<u8> {
    let mut bytes = b"8BPS".to_vec();
    bytes.extend(1u16.to_be_bytes());
    bytes.extend([0; 6]);
    bytes.extend(3u16.to_be_bytes());
    bytes.extend(1u32.to_be_bytes());
    bytes.extend(4u32.to_be_bytes());
    bytes.extend(depth.to_be_bytes());
    bytes.extend(3u16.to_be_bytes());
    bytes.extend([0; 4]);
    let resources = resource(profile);
    bytes.extend((resources.len() as u32).to_be_bytes());
    bytes.extend(resources);
    bytes.extend([0; 4]);
    bytes.extend([0; 2]);
    for channel in 0..3 {
        for pixel in PIXELS {
            if depth == 16 {
                bytes.extend(pixel[channel].to_be_bytes());
            } else {
                bytes.push(peerbrush::raster::project16(pixel[channel]));
            }
        }
    }
    bytes
}
fn shared(doc: Document) -> server::Shared {
    let mut e = Engine::new();
    e.replace(doc, Some("protected-original.psd".into()))
        .unwrap();
    Arc::new(Mutex::new(e))
}

#[test]
fn matrix_conversion_matches_independent_transfer_and_keeps_alpha_and_low_bits() {
    let profile = linear_profile();
    color_profile::supported(&profile).unwrap();
    let input = [
        12345, 23456, 34567, 45679, 12346, 23457, 34568, 1, 128, 1200, 60001, 0,
    ];
    let mut words = input;
    color_profile::convert16(&profile, &mut words).unwrap();
    for (source, result) in input.chunks_exact(4).zip(words.chunks_exact(4)) {
        assert_eq!(source[3], result[3]);
        for c in 0..3 {
            assert!(
                // ICC XYZ colorants use s15Fixed16; cross-channel rounding is
                // amplified by sRGB's steep near-black transfer slope.
                result[c].abs_diff(srgb(source[c])) <= 20,
                "{} -> {} (expected {})",
                source[c],
                result[c],
                srgb(source[c])
            );
        }
    }
    assert!(words[..3].iter().any(|v| v % 257 != 0));
    assert_ne!(words[..3], words[4..7]);
    let mut bytes = [48, 91, 135, 173, 3, 7, 240, 0];
    let before = bytes;
    color_profile::convert8(&profile, &mut bytes).unwrap();
    for (source, result) in before.chunks_exact(4).zip(bytes.chunks_exact(4)) {
        assert_eq!(source[3], result[3]);
        for c in 0..3 {
            assert!(
                result[c].abs_diff(peerbrush::raster::project16(srgb(source[c] as u16 * 257))) <= 1
            );
        }
    }
}

#[test]
fn profiled_psd_loading_canvas_and_export_have_separate_color_boundaries() {
    let profile = linear_profile();
    for depth in [8, 16] {
        let control = Control::default();
        let doc = psd::decode_reader(&mut Cursor::new(fixture(&profile, depth)), &control).unwrap();
        assert!(doc.read_only);
        assert_eq!(doc.bit_depth, depth);
        assert_eq!(doc.icc_profile.as_deref().unwrap().as_slice(), profile);
        let original = doc.layers[0].pixels.clone();
        let display = doc.preview(None, 4, None, false).unwrap().2;
        assert_eq!(control.take_preview().unwrap().2, display);
        let mut cache = peerbrush::preview::Cache::default();
        assert_eq!(
            cache
                .render(&doc, "profile", None, 4, None, false)
                .unwrap()
                .bytes,
            display
        );
        assert_eq!(
            cache
                .render(&doc, "profile", Some([0, 0, 1, 1]), 4, None, false)
                .unwrap()
                .bytes,
            display
        );
        let png = doc.export_png().unwrap();
        let mut decoder = PngDecoder::new(Cursor::new(&png)).unwrap();
        assert_eq!(decoder.icc_profile().unwrap().unwrap(), profile);
        let mut bytes = vec![0; decoder.total_bytes() as usize];
        decoder.read_image(&mut bytes).unwrap();
        if depth == 16 {
            assert_eq!(
                bytes
                    .chunks_exact(2)
                    .map(|v| u16::from_ne_bytes(v.try_into().unwrap()))
                    .collect::<Vec<_>>(),
                PIXELS.concat()
            );
            assert_eq!(original.rgba16(), PIXELS.concat());
        } else {
            assert_eq!(bytes, original.rgba());
        }
        assert_ne!(bytes, display);
        assert!(psd::encode(&doc).is_err());
        let state = shared(doc).lock().unwrap().state();
        assert_eq!(state["document"]["color_profile"]["preview"], "sRGB");
        assert!(!state.to_string().contains("icc_profile"));
    }
}

#[test]
fn explicit_protocol_copy_converts_original_precision_and_preserves_source_and_guards() {
    let profile = linear_profile();
    for depth in [8, 16] {
        let source = psd::decode(&fixture(&profile, depth)).unwrap();
        let source_png = source.export_png().unwrap();
        let source_id = source.id.clone();
        let display = source.preview(None, 4, None, false).unwrap().2;
        let shared = shared(source.clone());
        let command = json!({"action":"compatible_copy", "actor":"qa", "expected_revision":0, "feedback":"request"});
        assert!(server::dispatch(&shared, "document", &command)
            .unwrap_err()
            .contains("convert_to_srgb"));
        let mut explicit = command.clone();
        explicit["convert_to_srgb"] = json!(true);
        explicit["expected_revision"] = json!(99);
        assert!(server::dispatch(&shared, "document", &explicit).is_err());
        explicit["expected_revision"] = json!(0);
        let lease = shared
            .lock()
            .unwrap()
            .reserve(
                "another",
                "protected",
                vec![Scope {
                    target: None,
                    rect: None,
                }],
            )
            .unwrap();
        assert!(server::dispatch(&shared, "document", &explicit).is_err());
        shared.lock().unwrap().leases.retain(|l| l.id != lease.id);
        server::dispatch(&shared, "document", &explicit).unwrap();
        let e = shared.lock().unwrap();
        assert_ne!(e.doc.id, source_id);
        assert!(!e.doc.read_only);
        assert_eq!(e.doc.bit_depth, depth);
        assert!(e.doc.icc_profile.is_none());
        assert!(e.doc.srgb_tagged);
        assert!(e.path.is_none());
        assert_eq!(e.doc.revision, 1);
        assert_eq!(e.saved_revision, 0);
        assert!(e.undo.is_empty());
        assert_eq!(e.doc.preview(None, 4, None, false).unwrap().2, display);
        if depth == 16 {
            assert!(e.doc.layers[0].pixels.rgba16().iter().any(|v| v % 257 != 0));
        }
        let standard = psd::decode(&psd::encode(&e.doc).unwrap()).unwrap();
        assert!(!standard.read_only);
        assert!(standard.srgb_tagged);
        assert_eq!(standard.export_png().unwrap(), e.doc.export_png().unwrap());
        assert_eq!(source.export_png().unwrap(), source_png);
        assert!(source.read_only);
        assert_eq!(source.icc_profile.as_deref().unwrap().as_slice(), profile);
    }
}

#[test]
fn converted_standard_psd_and_png_keep_an_explicit_working_profile_without_private_sources() {
    for depth in [8, 16] {
        let source = psd::decode(&fixture(&linear_profile(), depth)).unwrap();
        let shared = shared(source);
        server::compatible_copy_with_color(&shared, "qa", Some(0), true).unwrap();
        let doc = shared.lock().unwrap().doc.clone();
        let bytes = psd::encode(&doc).unwrap();
        let size = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
        let resources = resource(color_profile::srgb_profile());
        let mut standard = bytes[..30].to_vec();
        standard.extend((resources.len() as u32).to_be_bytes());
        standard.extend(resources);
        standard.extend(&bytes[34 + size..]);
        let reopened = psd::decode(&standard).unwrap();
        assert!(!reopened.read_only);
        assert!(reopened.srgb_tagged);
        assert!(reopened.icc_profile.is_none());
        assert_eq!(reopened.export_png().unwrap(), doc.export_png().unwrap());
        let png = reopened.export_png().unwrap();
        let mut decoder = PngDecoder::new(Cursor::new(png)).unwrap();
        assert_eq!(
            decoder.icc_profile().unwrap().unwrap(),
            color_profile::srgb_profile()
        );
        let serialized = serde_json::to_vec(&reopened).unwrap();
        let restored: Document = serde_json::from_slice(&serialized).unwrap();
        assert!(restored.srgb_tagged);
        assert_eq!(restored.export_png().unwrap(), doc.export_png().unwrap());
    }
}

#[test]
fn srgb_tags_are_stable_and_earlier_encoder_dates_reopen_without_reconversion() {
    let standard = color_profile::srgb_profile();
    assert_eq!(
        &standard[24..36],
        &[0x07, 0xcc, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]
    );
    let mut earlier = standard.to_vec();
    earlier[24..36].copy_from_slice(&[0x07, 0xe4, 0, 2, 0, 3, 0, 4, 0, 5, 0, 6]);
    assert!(color_profile::is_srgb_profile(&earlier));
    for depth in [8, 16] {
        let mut source = Document::new_depth(4, 1, depth).unwrap();
        source.srgb_tagged = true;
        for (x, pixel) in PIXELS.iter().enumerate() {
            source.layers[0].pixels.set16(x as i32, 0, *pixel);
        }
        let bytes = psd::encode(&source).unwrap();
        let size = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
        let resources = resource(&earlier);
        let mut timestamped = bytes[..30].to_vec();
        timestamped.extend((resources.len() as u32).to_be_bytes());
        timestamped.extend(resources);
        timestamped.extend(&bytes[34 + size..]);
        let opened = psd::decode(&timestamped).unwrap();
        assert!(!opened.read_only);
        assert!(opened.srgb_tagged);
        assert!(opened.icc_profile.is_none());
        assert_eq!(
            opened.layers[0].pixels.rgba16(),
            if depth == 16 {
                PIXELS.concat()
            } else {
                PIXELS
                    .concat()
                    .into_iter()
                    .map(|v| u16::from(peerbrush::raster::project16(v)) * 257)
                    .collect()
            }
        );
    }
    let mut changed = earlier;
    let last = changed.len() - 1;
    changed[last] ^= 1;
    assert!(!color_profile::is_srgb_profile(&changed));
    assert!(psd::decode(&fixture(&changed, 16)).unwrap().read_only);
}

#[test]
fn unsupported_malformed_ambiguous_and_oversized_profiles_never_become_editable() {
    let profile = linear_profile();
    let mut lut = profile.clone();
    lut[132..136].copy_from_slice(b"A2B0");
    let mut duplicate = profile.clone();
    duplicate[144..148].copy_from_slice(b"rXYZ");
    let mut truncated = profile.clone();
    truncated.truncate(truncated.len() - 1);
    let mut bounds = profile.clone();
    bounds[136..140].copy_from_slice(&0u32.to_be_bytes());
    let mut hdr = profile.clone();
    hdr[132..136].copy_from_slice(b"cicp");
    for invalid in [lut, duplicate, truncated, bounds, hdr, vec![1, 2, 3]] {
        assert!(color_profile::supported(&invalid).is_err());
        let doc = psd::decode(&fixture(&invalid, 16)).unwrap();
        assert!(doc.read_only);
        assert_eq!(doc.layers[0].pixels.rgba16(), PIXELS.concat());
        let summary = color_profile::summary(Some(&invalid));
        assert_eq!(summary["preview"], "unmanaged");
        let shared = shared(doc);
        let before = shared.lock().unwrap().doc.id.clone();
        assert!(server::compatible_copy_with_color(&shared, "qa", Some(0), true).is_err());
        assert_eq!(shared.lock().unwrap().doc.id, before);
    }
    let oversized = vec![0; color_profile::MAX_PROFILE_BYTES + 1];
    assert!(psd::decode(&fixture(&oversized, 8))
        .unwrap_err()
        .contains("4 MiB"));
    let mut bytes = fixture(&profile, 8);
    let size = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
    let extra = resource(&profile);
    bytes.splice(34 + size..34 + size, extra.iter().copied());
    bytes[30..34].copy_from_slice(&((size + extra.len()) as u32).to_be_bytes());
    assert!(psd::decode(&bytes).unwrap_err().contains("Duplicate"));
    let mut doc = Document::new(1, 1).unwrap();
    doc.icc_profile = Some(Arc::new(profile));
    assert!(psd::validate(&doc).is_err());
}

#[test]
fn common_rgb_matrix_profiles_are_supported_and_profile_metadata_cannot_restore_private_sources() {
    for source in [
        moxcms::ColorProfile::new_srgb(),
        moxcms::ColorProfile::new_adobe_rgb(),
        moxcms::ColorProfile::new_display_p3(),
        moxcms::ColorProfile::new_pro_photo_rgb(),
    ] {
        color_profile::supported(&source.encode().unwrap()).unwrap();
    }
    let mut doc = Document::new_depth(4, 1, 16).unwrap();
    doc.layers[0].pixels = peerbrush::raster::Raster::from_rgba16(4, 1, &PIXELS.concat()).unwrap();
    let mut bytes = psd::encode(&doc).unwrap();
    let size = u32::from_be_bytes(bytes[30..34].try_into().unwrap()) as usize;
    let extra = resource(&linear_profile());
    bytes.splice(34 + size..34 + size, extra.iter().copied());
    bytes[30..34].copy_from_slice(&((size + extra.len()) as u32).to_be_bytes());
    let loaded = psd::decode(&bytes).unwrap();
    assert!(loaded.read_only);
    assert!(loaded.icc_profile.is_some());
    assert_ne!(loaded.id, doc.id);
    assert_eq!(loaded.layers[0].name, "Saved PSD composite");
}
