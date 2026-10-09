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
