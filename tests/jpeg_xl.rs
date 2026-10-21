use peerbrush::{
    color_profile,
    jpeg_xl::{self, Options, Pixels, PixelsRef},
};
use std::cell::Cell;

fn bytes() -> Vec<u8> {
    (0..32 * 24)
        .flat_map(|p| {
            [
                (p * 17) as u8,
                (p * 29 + 3) as u8,
                (p * 43 + 7) as u8,
                if p % 7 == 0 { 0 } else { (p * 13) as u8 },
            ]
        })
        .collect()
}
fn words() -> Vec<u16> {
    (0..32 * 24)
        .flat_map(|p| {
            [
                (p * 317 + 12347) as u16,
                (p * 797 + 23459) as u16,
                (p * 191 + 34571) as u16,
                if p % 7 == 0 {
                    0
                } else {
                    (p * 113 + 12001) as u16
                },
            ]
        })
        .collect()
}
#[test]
fn native_lossless_rgba8_retains_hidden_rgb_and_exact_alpha() {
    let original = bytes();
    let encoded = jpeg_xl::encode(
        32,
        24,
        PixelsRef::Eight(&original),
        None,
        Options::default(),
        &|| Ok(()),
    )
    .unwrap();
    assert!(jpeg_xl::signature(&encoded));
    let info = jpeg_xl::inspect(&encoded, &|| Ok(())).unwrap();
    assert_eq!((info.width, info.height, info.bit_depth), (32, 24, 8));
    let decoded = jpeg_xl::decode(&encoded, &|| Ok(())).unwrap();
    assert_eq!(decoded.info, info);
    assert_eq!(decoded.pixels, Pixels::Eight(original));
    assert_eq!(decoded.icc_profile, color_profile::srgb_profile());
    color_profile::supported(&decoded.icc_profile).unwrap();
}
#[test]
fn native_lossless_rgba16_retains_exact_words_and_hidden_rgb() {
    let original = words();
    assert!(original
        .chunks_exact(4)
        .any(|p| p[..3].iter().any(|v| v % 257 != 0)));
    let encoded = jpeg_xl::encode(
        32,
        24,
        PixelsRef::Sixteen(&original),
        None,
        Options::default(),
        &|| Ok(()),
    )
    .unwrap();
    let decoded = jpeg_xl::decode(&encoded, &|| Ok(())).unwrap();
    assert_eq!(decoded.info.bit_depth, 16);
    assert_eq!(decoded.pixels, Pixels::Sixteen(original));
    assert_eq!(decoded.icc_profile, color_profile::srgb_profile());
}
#[test]
fn lossy_both_depths_keep_lossless_alpha_and_native_rgb_precision() {
    let options = Options {
        lossless: false,
        ..Options::default()
    };
    let original = bytes();
    let encoded = jpeg_xl::encode(32, 24, PixelsRef::Eight(&original), None, options, &|| {
        Ok(())
    })
    .unwrap();
    let decoded = jpeg_xl::decode(&encoded, &|| Ok(())).unwrap();
    color_profile::supported(&decoded.icc_profile).unwrap();
    assert_ne!(decoded.icc_profile, color_profile::srgb_profile());
    let Pixels::Eight(decoded) = decoded.pixels else {
        panic!("8-bit source was promoted unexpectedly")
    };
    assert_ne!(decoded, original);
    assert!(decoded
        .iter()
        .skip(3)
        .step_by(4)
        .eq(original.iter().skip(3).step_by(4)));
    let original = words();
    let encoded = jpeg_xl::encode(
        32,
        24,
        PixelsRef::Sixteen(&original),
        None,
        options,
        &|| Ok(()),
    )
    .unwrap();
    let decoded = jpeg_xl::decode(&encoded, &|| Ok(())).unwrap();
    assert_eq!(decoded.info.bit_depth, 16);
    color_profile::supported(&decoded.icc_profile).unwrap();
    assert_ne!(decoded.icc_profile, color_profile::srgb_profile());
    let Pixels::Sixteen(decoded) = decoded.pixels else {
        panic!("Native 16-bit channels were reduced")
    };
    assert_ne!(decoded, original);
    assert!(decoded
        .iter()
        .skip(3)
        .step_by(4)
        .eq(original.iter().skip(3).step_by(4)));
    assert!(decoded
        .chunks_exact(4)
        .any(|p| p[..3].iter().any(|v| v % 257 != 0)));
}

// Same independently assembled ICC v4 matrix/linear-TRC fixture used by the
// RGB profile acceptance suite, independent of libjxl's profile generator.
fn linear_profile() -> Vec<u8> {
    let fixed = |v: f64| ((v * 65536.).round() as i32).to_be_bytes();
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
    let mut profile = vec![0; 132 + entries.len() * 12];
    profile[8..12].copy_from_slice(&0x04200000u32.to_be_bytes());
    profile[12..24].copy_from_slice(b"mntrRGB XYZ ");
    profile[24..36].copy_from_slice(&[0x07, 0xcc, 0, 1, 0, 1, 0, 0, 0, 0, 0, 0]);
    profile[36..40].copy_from_slice(b"acsp");
    for (i, value) in [0.9642, 1., 0.8249].iter().enumerate() {
        profile[68 + i * 4..72 + i * 4].copy_from_slice(&fixed(*value));
    }
    profile[128..132].copy_from_slice(&(entries.len() as u32).to_be_bytes());
    for (i, (key, data)) in entries.iter().enumerate() {
        let at = 132 + i * 12;
        let offset = profile.len() as u32;
        profile[at..at + 4].copy_from_slice(key);
        profile[at + 4..at + 8].copy_from_slice(&offset.to_be_bytes());
        profile[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_be_bytes());
        profile.extend(data);
    }
    let size = profile.len() as u32;
    profile[..4].copy_from_slice(&size.to_be_bytes());
    profile
}
#[test]
fn linear_rgb_profile_is_preserved_losslessly_and_xyb_uses_actual_output_profile() {
    let profile = linear_profile();
    color_profile::supported(&profile).unwrap();
    // Mostly flat opaque RGB makes the difference between linear and sRGB
    // interpretation large relative to the lossy tolerance. Alpha stays exact.
    let original: Vec<u16> = (0..32 * 24)
        .flat_map(|p| [12347 + (p % 4) as u16, 23459, 34571, 45679])
        .collect();
    for lossless in [true, false] {
        let encoded = jpeg_xl::encode(
            32,
            24,
            PixelsRef::Sixteen(&original),
            Some(&profile),
            Options {
                lossless,
                distance: 0.1,
                effort: 3,
            },
            &|| Ok(()),
        )
        .unwrap();
        let decoded = jpeg_xl::decode(&encoded, &|| Ok(())).unwrap();
        if lossless {
            assert_eq!(decoded.icc_profile, profile);
        }
        // A differing default output selects sRGB; compare colors using the
        // actual output ICC, which may differ from Original linear ICC.
        color_profile::supported(&decoded.icc_profile).unwrap();
        let Pixels::Sixteen(mut pixels) = decoded.pixels else {
            panic!("16-bit profile was projected")
        };
        if lossless {
            assert_eq!(pixels, original);
        }
        for (actual, source) in pixels.chunks_exact(4).zip(original.chunks_exact(4)) {
            assert_eq!(actual[3], source[3]);
        }
        let original_alpha: Vec<_> = pixels.iter().skip(3).step_by(4).copied().collect();
        let mut expected = original.clone();
        color_profile::convert16(&profile, &mut expected).unwrap();
        color_profile::convert16(&decoded.icc_profile, &mut pixels).unwrap();
        for (actual, expected) in pixels.chunks_exact(4).zip(expected.chunks_exact(4)) {
            for c in 0..3 {
                assert!(actual[c].abs_diff(expected[c]) <= 2048);
            }
        }
        assert!(
            pixels[0] > original[0] + 10000,
            "Actual output samples were mislabeled as Original linear RGB"
        );
        assert!(pixels.iter().skip(3).step_by(4).copied().eq(original_alpha));
    }
}
#[test]
fn malformed_truncated_canceled_and_invalid_requests_return_no_output() {
    assert!(!jpeg_xl::signature(b"not a JPEG XL image"));
    assert!(jpeg_xl::decode(b"not a JPEG XL image", &|| Ok(())).is_err());
    let original = bytes();
    let encoded = jpeg_xl::encode(
        32,
        24,
        PixelsRef::Eight(&original),
        None,
        Options::default(),
        &|| Ok(()),
    )
    .unwrap();
    for end in [0, 1, 2, encoded.len() / 2, encoded.len() - 1] {
        assert!(jpeg_xl::decode(&encoded[..end], &|| Ok(())).is_err());
    }
    for threshold in [0, 2, 4] {
        let count = Cell::new(0);
        let check = || {
            let value = count.get();
            count.set(value + 1);
            if value >= threshold {
                Err("fixture canceled".into())
            } else {
                Ok(())
            }
        };
        assert!(jpeg_xl::encode(
            32,
            24,
            PixelsRef::Eight(&original),
            None,
            Options::default(),
            &check
        )
        .is_err());
    }
    let sixteen = jpeg_xl::encode(
        32,
        24,
        PixelsRef::Sixteen(&words()),
        None,
        Options::default(),
        &|| Ok(()),
    )
    .unwrap();
    for payload in [&encoded, &sixteen] {
        // Cover entry, queue, validated IPC and final publication boundaries.
        // The backend separately tests exact native event phases.
        for threshold in [0, 2, 4, 5, 6] {
            let count = Cell::new(0);
            let check = || {
                let value = count.get();
                count.set(value + 1);
                if value >= threshold {
                    Err("fixture canceled".into())
                } else {
                    Ok(())
                }
            };
            let error = jpeg_xl::decode(payload, &check)
                .err()
                .expect("Canceled decode returned output");
            assert_eq!(error, "fixture canceled", "threshold {threshold}");
            assert!(count.get() > threshold);
        }
    }
    assert!(jpeg_xl::encode(
        0,
        24,
        PixelsRef::Eight(&original),
        None,
        Options::default(),
        &|| Ok(())
    )
    .is_err());
    assert!(jpeg_xl::encode(
        32,
        24,
        PixelsRef::Eight(&original[..100]),
        None,
        Options::default(),
        &|| Ok(())
    )
    .is_err());
    assert!(jpeg_xl::encode(
        32,
        24,
        PixelsRef::Eight(&original),
        Some(b"bad ICC"),
        Options::default(),
        &|| Ok(())
    )
    .is_err());
    for options in [
        Options {
            distance: f32::NAN,
            ..Options::default()
        },
        Options {
            distance: 0.,
            ..Options::default()
        },
        Options {
            effort: 10,
            ..Options::default()
        },
    ] {
        assert!(
            jpeg_xl::encode(32, 24, PixelsRef::Eight(&original), None, options, &|| Ok(
                ()
            ))
            .is_err()
        );
    }
}
