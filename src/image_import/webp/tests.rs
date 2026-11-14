use super::*;
use image::ImageEncoder;
use std::io::Cursor;

pub(crate) fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(kind);
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() & 1 != 0 {
        out.push(0);
    }
}
pub(crate) fn riff(body: Vec<u8>) -> Vec<u8> {
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}
fn le24(value: u32) -> [u8; 3] {
    value.to_le_bytes()[..3].try_into().unwrap()
}
pub(crate) fn lossless(w: u32, h: u32, seed: u8, alpha: bool) -> Vec<u8> {
    let channels = if alpha { 4 } else { 3 };
    let pixels: Vec<_> = (0..w * h)
        .flat_map(|n| {
            let values = [
                (n as u8).wrapping_add(seed),
                (n as u8).wrapping_mul(31),
                seed,
                if n % 7 == 0 { 63 } else { 255 },
            ];
            values.into_iter().take(channels)
        })
        .collect();
    let mut bytes = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
        .write_image(
            &pixels,
            w,
            h,
            if alpha {
                image::ExtendedColorType::Rgba8
            } else {
                image::ExtendedColorType::Rgb8
            },
        )
        .unwrap();
    bytes
}
pub(crate) fn lossy(alpha: bool) -> Vec<u8> {
    // VP8 payload from image-webp 0.2.4's decoder test, under vendor licenses.
    let vp8 = [
        0xd0, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x02, 0x00, 0x02, 0x00, 0x02, 0x00, 0x34, 0x25, 0xa0,
        0x02, 0x74, 0xba, 0x01, 0xf8, 0x00, 0x03, 0xb0, 0x00, 0xfe, 0xf0, 0xc4, 0x0b, 0xff, 0x20,
        0xb9, 0x61, 0x75, 0xc8, 0xd7, 0xff, 0x20, 0x3f, 0xe4, 0x07, 0xfc, 0x80, 0xff, 0xf8, 0xf2,
        0x00, 0x00, 0x00,
    ];
    let mut body = b"WEBP".to_vec();
    if alpha {
        chunk(&mut body, b"VP8X", &[16, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
        chunk(&mut body, b"ALPH", &[0, 0, 31, 127, 255]);
    }
    chunk(&mut body, b"VP8 ", &vp8);
    riff(body)
}

pub(crate) fn compressed_alpha_fixture() -> Vec<u8> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    // 17 × 53 gradient encoded with Pillow/libwebp, quality 41, with compressed ALPH.
    STANDARD.decode(concat!(
        "UklGRh4BAABXRUJQVlA4WAoAAAAQAAAAEAAANAAAQUxQSBIAAAAFMJu2TUBp+//tXSIiAgBwLwdWUDgg5gAAAFAHAJ0BKh",
        "EANQA+7W6sUCmmpCMoGAwBMB2JagC7M5gKzuAwWYV1phIp/7+WBWcBEDlpqNbV4Mn4t0wZWaB0vTTE9OAA/vhWcWMNDIP",
        "GkBGFoSkIElqALsgQKI7l8ZunLa7RVP2h9WL60vzQqDMc7I6SNcr/+JDR3wBZkOWuxMZGiFUQz1MmsGvFHv1qnPy2WsXl",
        "qjK5Hn2rEpnt5Pz+dN15rn5umXwVAocA0NBtyZwVd1/uDdbGnrW8M/P5+5aovNUDHax/50945HsK2o9US8t/zrlrbrbxb",
        "3rxl3e2g+aD7Xz3wAAA",
    )).unwrap()
}
pub(crate) fn fixture(frames: usize, profiled: bool) -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    chunk(
        &mut body,
        b"VP8X",
        &[
            if profiled { 0x3a } else { 0x12 },
            0,
            0,
            0,
            13,
            1,
            0,
            18,
            0,
            0,
        ],
    );
    if profiled {
        chunk(
            &mut body,
            b"ICCP",
            &moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        );
    }
    chunk(&mut body, b"ANIM", &[19, 31, 47, 255, 0, 0]);
    for n in 0..frames {
        let (w, h, payload) = if n == 0 {
            (270, 19, lossless(270, 19, 17, true)[12..].to_vec())
        } else if n == 2 {
            (2, 2, lossy(true)[30..].to_vec())
        } else {
            (7, 5, lossless(7, 5, 71, true)[12..].to_vec())
        };
        let mut frame = Vec::new();
        for value in [n as u32, n as u32, w - 1, h - 1, 10] {
            frame.extend(le24(value));
        }
        frame.push(if n == 1 {
            3
        } else if n == 0 {
            2
        } else {
            0
        });
        frame.extend(payload);
        chunk(&mut body, b"ANMF", &frame);
        // Unknown metadata between frames is legal and must not derail seek.
        chunk(&mut body, b"JUNK", &[17, 29, 43]);
    }
    if profiled {
        chunk(
            &mut body,
            b"EXIF",
            b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0",
        );
    }
    riff(body)
}

#[test]
fn raw_alpha_rows_match_buffered_filters_and_refuse_truncated_planes() {
    use image_webp::{RawAlphaRows, WebPDecoder};

    for (filter, expected) in [[0, 31, 127, 255], [0, 31, 127, 126], [0, 31, 127, 30], [0, 31, 127, 157]]
        .into_iter().enumerate()
    {
        let mut bytes = lossy(true);
        let start = bytes.windows(4).position(|kind| kind == b"ALPH").unwrap() + 8;
        bytes[start] = 0xd0 | (filter as u8) << 2;
        let info = inspect_native(&mut Cursor::new(&bytes), bytes.len() as u64, true, &|| Ok(())).unwrap();
        let alpha = info.static_alpha.unwrap();
        assert_eq!(alpha.range, start as u64..start as u64 + 5);
        assert!(!alpha.compressed());
        assert!(!info.lossless);
        let mut full = [0; 16];
        WebPDecoder::new(Cursor::new(&bytes)).unwrap().read_image(&mut full).unwrap();
        let mut scratch = [0; 4];
        let mut rows = RawAlphaRows::new(&bytes[start..start + 5], 2, 2, &mut scratch).unwrap();
        assert_eq!(rows.read_row().unwrap(), &expected[..2]);
        assert_eq!(rows.read_row().unwrap(), &expected[2..]);
        assert!(rows.read_row().is_err());
        assert_eq!(full.chunks_exact(4).map(|pixel| pixel[3]).collect::<Vec<_>>(), expected);
        let mut truncated = RawAlphaRows::new(&bytes[start..start + 4], 2, 2, &mut scratch).unwrap();
        assert!(truncated.read_row().is_ok());
        assert!(truncated.read_row().is_err());
        bytes[start - 4..start].copy_from_slice(&4u32.to_le_bytes());
        assert!(inspect_native(&mut Cursor::new(&bytes), bytes.len() as u64, true, &|| Ok(())).is_err());
    }
}

#[test]
fn native_encoded_limits_match_the_webp_layout_and_export_ceiling() {
    let normal = super::super::ENCODED_LIMIT as u64;
    let lossless_limit = NATIVE_FILE_BYTES;
    assert_eq!(super::super::native::OpenFormat::Webp.encoded_limit(), lossless_limit);
    for (fixture, chunk, cap) in [
        (lossless(3, 2, 11, true), b"VP8L", lossless_limit),
        (compressed_alpha_fixture(), b"VP8 ", NATIVE_ALPHA_FILE_BYTES),
        (lossy(false), b"VP8 ", normal),
    ] {
        let start = fixture.windows(4).position(|kind| kind == chunk).unwrap() + 8;
        let inspect_declared = |length: u64, limits| {
            // The metadata reader seeks over image data. Exercise its range and
            // encoded limits without allocating the declared VP8/VP8L payload.
            let mut header = fixture.clone();
            header[4..8].copy_from_slice(&((length - 8) as u32).to_le_bytes());
            header[start - 4..start].copy_from_slice(&((length - start as u64) as u32).to_le_bytes());
            inspect_with_limits(&mut Cursor::new(header), length, true, limits, &|| Ok(()))
        };
        assert_eq!(inspect_declared(cap, CanvasLimits::Native).unwrap().native_file_limit(), cap);
        assert!(inspect_declared(cap + 2, CanvasLimits::Native).is_err());
        assert!(inspect_declared(normal + 2, CanvasLimits::Resident).is_err());
    }
}

#[test]
fn large_animation_layout_uses_the_native_file_envelope() {
    let original = fixture(1, false);
    let frame = original.windows(4).position(|kind| kind == b"ANMF").unwrap() + 8;
    let pixels = original.windows(4).position(|kind| kind == b"VP8L").unwrap() + 8;
    for length in [super::super::ENCODED_LIMIT as u64 + 2, NATIVE_FILE_BYTES, NATIVE_FILE_BYTES + 2] {
        // A seekable metadata probe need only see the first bytes of the image payload.
        let mut header = original[..pixels + 5].to_vec();
        header[4..8].copy_from_slice(&((length - 8) as u32).to_le_bytes());
        for start in [frame, pixels] {
            header[start - 4..start].copy_from_slice(&((length - start as u64) as u32).to_le_bytes());
        }
        assert!(inspect(&mut Cursor::new(&header), length, true, &|| Ok(())).is_err());
        let mut input = Cursor::new(&header);
        if length <= NATIVE_FILE_BYTES {
            preflight_large(&mut input, length, true, &|| Ok(())).unwrap();
            let info = inspect_native(&mut input, length, true, &|| Ok(())).unwrap();
            assert!(info.animated);
            assert_eq!(info.frames, 1);
        } else {
            assert!(preflight_large(&mut input, length, true, &|| Ok(())).is_err());
        }
    }
}

#[test]
fn lossy_row_output_matches_buffered_pixels_across_filtered_bands() {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use image_webp::{DecodingError, UpsamplingMethod, WebPDecodeOptions, WebPDecoder};

    // Deterministic RGB gradients encoded as VP8 by Pillow/libwebp, quality 41.
    for fixture in [
        concat!(
            "UklGRvAAAABXRUJQVlA4IOQAAAAQBwCdASoRADUAPu1urFAppqQjKBgMATAdiWoAguRZeoDBZgNO7YD7w/1NSD1t5YHB",
            "q8BUc3tf3vtWPbWfMdGE4AD++JMKdCeyC0ll5UC4yQkqeqI0662ZA3h28yZAZL1U6VB6gttiirp0k9KFQPCQybyNRWa6",
            "h23loKiYSw6lBs//43GpM6v/ZzLo/Iqbx5CafuWqMrk+33kHtA7hOQvJ+htrpM2ujjCX0DOFgfph9Fpi8tqu9MZZpP5+",
            "5AVO+nYlpqjSkLF8Y/tlKFs/4DZ/fc/wPGgD09/b7LTvDijY55AAAAA=",
        ),
        concat!(
            "UklGRgIBAABXRUJQVlA4IPYAAAAQCACdASoUADIAPu1srlAppiQiqAqpMB2JagCpJ46Cz8hxcrwAcoFo7dQxj65kP6Ja",
            "Xpt4nH5VWWoqwaFqnMz2A6emrignOrJAAP73xcjDFnhZSsXcqUoh0S1en3JcVwwMFDkn0Tw4sfgxTkybF2mc798GfIPM",
            "MCZEM95H5QF5PG1NZlMT7robUE51XGkGrIP/43FRkflu+wsR1qm8ejbzPUib2y18GNplYu+gj4i0nnSobfffhRDz4ZCt",
            "fGWCoVe/QewNYuc6p/Xt4k5qnwoLDg1PawLYSYIk8Abo4OyMCOTjNfz9a+uRpcF8/TQ5fFscunwAAAA=",
        ),
    ] {
        let bytes = STANDARD.decode(fixture).unwrap();
        for upsampling in [UpsamplingMethod::Simple, UpsamplingMethod::Bilinear] {
            let mut options = WebPDecodeOptions::default();
            options.lossy_upsampling = upsampling;
            let open = || WebPDecoder::new_with_options(Cursor::new(&bytes), options.clone()).unwrap();
            let mut original = open();
            let (width, height) = original.dimensions();
            assert!(height > 48);
            let stride = width as usize * 3;
            let mut expected = vec![0; stride * height as usize];
            original.read_image(&mut expected).unwrap();
            let mut scratch = vec![0; stride];
            let mut rows = 0;
            open().read_static_lossy_color_rows(&mut scratch, |y, row| {
                assert_eq!(y, rows);
                assert_eq!(row, &expected[y as usize * stride..][..stride]);
                rows += 1;
                Ok(())
            }).unwrap();
            assert_eq!(rows, height);
            let error = open().read_static_lossy_color_rows(&mut scratch, |_, _| {
                Err(DecodingError::InvalidParameter("sink stopped".into()))
            }).unwrap_err();
            assert!(matches!(error, DecodingError::InvalidParameter(reason) if reason == "sink stopped"));
        }
    }
}

#[test]
fn direct_lossless_rgba_matches_the_default_decoder() {
    for alpha in [false, true] {
        let bytes = lossless(19, 7, 53, alpha);
        let decoder = || image_webp::WebPDecoder::new(Cursor::new(bytes.as_slice())).unwrap();
        let mut original = decoder();
        let mut expected = vec![0; original.output_buffer_size().unwrap()];
        original.read_image(&mut expected).unwrap();
        let mut rgba = vec![0; 19 * 7 * 4];
        decoder().read_static_lossless_rgba(&mut rgba).unwrap();
        let mut streamed = decoder();
        let mut scratch = vec![0; streamed.lossless_row_scratch_bytes().unwrap()];
        let mut rows = 0;
        streamed.read_static_lossless_rgba_rows(&mut scratch, |y, row| {
            assert_eq!(y, rows);
            assert_eq!(row, &rgba[y as usize * 19 * 4..][..19 * 4]);
            rows += 1;
            Ok(())
        }).unwrap();
        assert_eq!(rows, 7);
        if alpha {
            assert_eq!(rgba, expected);
        } else {
            for (actual, expected) in rgba.chunks_exact(4).zip(expected.chunks_exact(3)) {
                assert_eq!(&actual[..3], expected);
                assert_eq!(actual[3], 255);
            }
        }
        assert!(decoder().read_static_lossless_rgba(&mut rgba[1..]).is_err());
    }
}

#[test]
fn wide_static_and_animated_geometry_is_native_only() {
    let inspect_container = |bytes: &[u8], limits| {
        inspect_with_limits(&mut Cursor::new(bytes), bytes.len() as u64, true, limits, &|| Ok(()))
    };
    let mut wide = lossless(16000, 3, 71, true);
    let info = inspect_container(&wide, CanvasLimits::Native).unwrap();
    assert_eq!((info.width, info.height, info.frames), (16000, 3, 1));
    assert!(inspect_container(&wide, CanvasLimits::Resident).is_err());
    let bits = u32::from_le_bytes(wide[21..25].try_into().unwrap());
    wide[21..25].copy_from_slice(&((bits & !0x3fff) | 16000).to_le_bytes());
    assert!(inspect_container(&wide, CanvasLimits::Native).is_err());

    let mut animated = fixture(1, false);
    animated[24..27].copy_from_slice(&le24(15999));
    assert_eq!(inspect_container(&animated, CanvasLimits::Native).unwrap().width, 16000);
    assert!(inspect_container(&animated, CanvasLimits::Resident).is_err());
}

#[test]
fn original_container_bounds_profiles_and_frame_dimensions_are_checked() {
    let valid = fixture(4, true);
    let info = inspect(&mut Cursor::new(&valid), valid.len() as u64, true, &|| {
        Ok(())
    })
    .unwrap();
    assert_eq!((info.width, info.height, info.frames), (270, 19, 4));
    assert!(info.lossy && info.lossless && info.alpha_chunk && info.animated);
    assert!(info.profile.is_some() && info.exif.is_some());
    for length in 0..valid.len() {
        assert!(inspect(
            &mut Cursor::new(&valid[..length]),
            length as u64,
            true,
            &|| Ok(())
        )
        .is_err());
    }
    let mut missing = fixture(1, false);
    missing[20] |= 32;
    assert!(inspect(
        &mut Cursor::new(&missing),
        missing.len() as u64,
        true,
        &|| Ok(())
    )
    .is_err());
    assert!(inspect(
        &mut Cursor::new(&missing),
        missing.len() as u64,
        false,
        &|| Ok(())
    )
    .is_ok());
    let mut duplicate = valid[8..].to_vec();
    chunk(&mut duplicate, b"ICCP", b"bad");
    let duplicate = riff(duplicate);
    assert!(inspect(
        &mut Cursor::new(&duplicate),
        duplicate.len() as u64,
        true,
        &|| Ok(())
    )
    .is_err());
    let mut mismatched = fixture(1, false);
    let at = mismatched.windows(4).position(|s| s == b"VP8L").unwrap();
    mismatched[at + 9] ^= 1;
    assert!(inspect(
        &mut Cursor::new(&mismatched),
        mismatched.len() as u64,
        false,
        &|| Ok(())
    )
    .is_err());
    assert_eq!(
        inspect(&mut Cursor::new(&valid), valid.len() as u64, true, &|| Err(
            "cancel".into()
        ))
        .unwrap_err(),
        "cancel"
    );
}

#[test]
fn animation_decoder_skips_metadata_between_frames_and_preserves_disposal() {
    let bytes = fixture(4, false);
    let mut decoder = image_webp::WebPDecoder::new(Cursor::new(&bytes)).unwrap();
    let mut pixels = vec![0; decoder.output_buffer_size().unwrap()];
    for index in 0..4 {
        decoder.read_frame(&mut pixels).unwrap();
        if index == 1 {
            assert_ne!(&pixels[(2 * 270 + 2) * 4..(2 * 270 + 2) * 4 + 4], &[0; 4]);
        }
        if index >= 2 {
            // Frame 1 disposes to transparent before the third frame, whose
            // rectangle starts at (4,4), leaving (2,2) cleared.
            assert_eq!(&pixels[(2 * 270 + 2) * 4..(2 * 270 + 2) * 4 + 4], &[0; 4]);
        }
    }
}

#[test]
fn disposal_always_clears_the_previous_rgba_rectangle() {
    for opaque in [false, true] {
        let mut body = b"WEBP".to_vec();
        chunk(&mut body, b"VP8X", &[0x12, 0, 0, 0, 3, 0, 0, 1, 0, 0]);
        chunk(&mut body, b"ANIM", &[0; 6]);
        let first = lossless(4, 2, 19, false);
        let second = lossless(1, 1, 71, true);
        let last = if opaque {
            lossy(false)
        } else {
            let mut bytes = Vec::new();
            image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
                .write_image(&[0; 32], 4, 2, image::ExtendedColorType::Rgba8)
                .unwrap();
            bytes
        };
        for (x, w, h, flags, bytes) in [
            (0, 4, 2, 2, first),
            (1, 1, 1, 3, second),
            (0, if opaque { 2 } else { 4 }, 2, 0, last),
        ] {
            let mut frame = Vec::new();
            for value in [x, 0, w - 1, h - 1, 10] {
                frame.extend(le24(value));
            }
            frame.push(flags);
            frame.extend_from_slice(&bytes[12..]);
            chunk(&mut body, b"ANMF", &frame);
        }
        let bytes = riff(body);
        let mut decoder = image_webp::WebPDecoder::new(Cursor::new(&bytes)).unwrap();
        let clear = [9, 11, 13, 17];
        decoder.set_background_color(clear).unwrap();
        let mut pixels = vec![0; decoder.output_buffer_size().unwrap()];
        decoder.read_frame(&mut pixels).unwrap();
        let original = pixels.clone();
        decoder.read_frame(&mut pixels).unwrap();
        decoder.read_frame(&mut pixels).unwrap();
        assert_eq!(&pixels[8..12], &clear);
        assert_eq!(&pixels[12..16], &original[12..16]);
        if !opaque {
            assert_eq!(&pixels[..8], &original[..8]);
        }
    }
}
