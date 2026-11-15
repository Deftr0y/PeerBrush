use image::ImageEncoder;
use image_webp::{DecodingError, WebPDecodeOptions, WebPDecoder};
use std::io::Cursor;

// The lossy 2x2 fixture is from image-webp 0.2.4's decoder tests, retained
// under the licenses in vendor/image-webp. Only its VP8 payload is reused.
const VP8: [u8; 48] = [
    0xd0, 0x01, 0x00, 0x9d, 0x01, 0x2a, 0x02, 0x00, 0x02, 0x00, 0x02, 0x00, 0x34, 0x25, 0xa0, 0x02,
    0x74, 0xba, 0x01, 0xf8, 0x00, 0x03, 0xb0, 0x00, 0xfe, 0xf0, 0xc4, 0x0b, 0xff, 0x20, 0xb9, 0x61,
    0x75, 0xc8, 0xd7, 0xff, 0x20, 0x3f, 0xe4, 0x07, 0xfc, 0x80, 0xff, 0xf8, 0xf2, 0x00, 0x00, 0x00,
];

fn chunk(bytes: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(data);
    if data.len() % 2 != 0 {
        bytes.push(0);
    }
}
fn riff(body: Vec<u8>) -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
    bytes.extend(body);
    bytes
}
fn fixture(animated: bool) -> (Vec<u8>, Vec<u8>) {
    let pixels = vec![
        17, 29, 43, 0, 71, 113, 149, 31, 227, 181, 59, 127, 31, 47, 61, 255, 199, 83, 137, 255, 7,
        11, 13, 255,
    ];
    let mut bytes = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
        .write_image(&pixels, 3, 2, image::ExtendedColorType::Rgba8)
        .unwrap();
    if animated {
        let mut body = b"WEBP".to_vec();
        chunk(&mut body, b"VP8X", &[0x12, 0, 0, 0, 2, 0, 0, 1, 0, 0]);
        chunk(&mut body, b"ANIM", &[0; 6]);
        let mut frame = vec![0, 0, 0, 0, 0, 0, 2, 0, 0, 1, 0, 0, 1, 0, 0, 2];
        frame.extend_from_slice(&bytes[12..]);
        chunk(&mut body, b"ANMF", &frame);
        bytes = riff(body);
    }
    (bytes, pixels)
}
fn decode(bytes: &[u8], limit: usize, animated: bool) -> Result<Vec<u8>, DecodingError> {
    let mut options = WebPDecodeOptions::default();
    options.lossless_huffman_limit = limit;
    let mut reader = WebPDecoder::new_with_options(Cursor::new(bytes), options)?;
    let mut output = vec![0; reader.output_buffer_size().unwrap()];
    if animated {
        reader.read_frame(&mut output)?;
    } else {
        reader.read_image(&mut output)?;
    }
    Ok(output)
}

#[test]
fn lossless_huffman_limit_applies_to_static_and_animation_decoding() {
    for animated in [false, true] {
        let (bytes, pixels) = fixture(animated);
        assert!(matches!(
            decode(&bytes, 128 * 1024, animated),
            Err(DecodingError::MemoryLimitExceeded)
        ));
        let admitted = decode(&bytes, 4 * 1024 * 1024, animated).unwrap();
        assert_eq!(admitted, decode(&bytes, usize::MAX, animated).unwrap());
        // The upstream encoder may clear hidden RGB; compare visible samples.
        for (actual, expected) in admitted.chunks_exact(4).zip(pixels.chunks_exact(4)) {
            assert_eq!(actual[3], expected[3]);
            if expected[3] != 0 {
                assert_eq!(actual, expected);
            }
        }
    }
}

#[test]
fn declared_large_huffman_group_table_is_refused_before_allocating_groups() {
    let mut data = vec![0x2f, 0, 0, 0, 0];
    let mut bit = 40usize;
    let mut put = |value: u32, count: usize| {
        for shift in 0..count {
            if bit / 8 == data.len() {
                data.push(0);
            }
            data[bit / 8] |= (((value >> shift) & 1) as u8) << (bit % 8);
            bit += 1;
        }
    };
    put(0, 1); // No transforms.
    put(0, 1); // No color cache.
    put(1, 1);
    put(0, 3); // One metadata pixel at a four-pixel block size.
    put(0, 1); // Metadata image has no color cache or recursive metadata image.
    for symbol in [255, 255, 0, 255, 0] {
        put(1, 1);
        put(0, 1); // Simple, single-symbol tree.
        if symbol > 1 {
            put(1, 1);
            put(symbol, 8);
        } else {
            put(0, 1);
            put(symbol, 1);
        }
    }
    drop(put);
    // Metadata pixel R=G=255 declares group 65535. No group payload is needed
    // to reach the admission check, which must precede group allocation.
    data.extend_from_slice(&[0; 16]);
    let mut body = b"WEBP".to_vec();
    chunk(&mut body, b"VP8L", &data);
    assert!(matches!(
        decode(&riff(body), 256 * 1024, false),
        Err(DecodingError::MemoryLimitExceeded)
    ));
}

#[test]
fn compressed_alpha_uses_the_same_huffman_admission_for_static_and_animated_images() {
    let alpha = [0, 31, 127, 255];
    let pixels: Vec<u8> = alpha
        .into_iter()
        .flat_map(|value| [0, value, 0, 255])
        .collect();
    let mut encoded = Vec::new();
    image::codecs::webp::WebPEncoder::new_lossless(&mut encoded)
        .write_image(&pixels, 2, 2, image::ExtendedColorType::Rgba8)
        .unwrap();
    assert_eq!(&encoded[12..16], b"VP8L");
    let length = u32::from_le_bytes(encoded[16..20].try_into().unwrap()) as usize;
    let mut alph = vec![1]; // Compressed alpha, no filtering or preprocessing.
    alph.extend_from_slice(&encoded[25..20 + length]);
    for animated in [false, true] {
        let mut frame = Vec::new();
        chunk(&mut frame, b"ALPH", &alph);
        chunk(&mut frame, b"VP8 ", &VP8);
        let mut body = b"WEBP".to_vec();
        chunk(
            &mut body,
            b"VP8X",
            &[
                if animated { 0x12 } else { 0x10 },
                0,
                0,
                0,
                1,
                0,
                0,
                1,
                0,
                0,
            ],
        );
        if animated {
            chunk(&mut body, b"ANIM", &[0; 6]);
            let mut data = vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 2];
            data.extend(frame);
            chunk(&mut body, b"ANMF", &data);
        } else {
            body.extend(frame);
        }
        let bytes = riff(body);
        assert!(matches!(
            decode(&bytes, 128 * 1024, animated),
            Err(DecodingError::MemoryLimitExceeded)
        ));
        let output = decode(&bytes, 4 * 1024 * 1024, animated).unwrap();
        assert_eq!(output, decode(&bytes, usize::MAX, animated).unwrap());
        assert_eq!(
            output.chunks_exact(4).map(|p| p[3]).collect::<Vec<_>>(),
            alpha
        );
    }
}

fn lossy(animated: bool, alpha: bool, embedded_width: u16, kind: &[u8; 4]) -> Vec<u8> {
    let mut frame = Vec::new();
    if alpha {
        chunk(&mut frame, b"ALPH", &[0, 0, 31, 127, 255]);
    }
    let mut vp8 = VP8;
    vp8[6..8].copy_from_slice(&embedded_width.to_le_bytes());
    chunk(&mut frame, kind, &vp8);
    let mut body = b"WEBP".to_vec();
    let flags = if animated { 2 } else { 0 } | if alpha { 16 } else { 0 };
    chunk(&mut body, b"VP8X", &[flags, 0, 0, 0, 1, 0, 0, 1, 0, 0]);
    if animated {
        chunk(&mut body, b"ANIM", &[0; 6]);
        let mut header = vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 1, 0, 0, 1, 0, 0, 2];
        header.extend(frame);
        chunk(&mut body, b"ANMF", &header);
    } else {
        body.extend(frame);
    }
    riff(body)
}

fn decode_lossy(bytes: &[u8], limit: usize) -> Result<Vec<u8>, DecodingError> {
    let mut options = WebPDecodeOptions::default();
    options.lossy_partition_limit = limit;
    let mut decoder = WebPDecoder::new_with_options(Cursor::new(bytes), options)?;
    let mut output = vec![0; decoder.output_buffer_size().unwrap()];
    decoder.read_image(&mut output)?;
    Ok(output)
}

#[test]
fn lossy_partition_admission_covers_static_and_animated_alpha_paths() {
    for animated in [false, true] {
        for alpha in [false, true] {
            let bytes = lossy(animated, alpha, 2, b"VP8 ");
            for limit in [0, 16, 32] {
                assert!(matches!(
                    decode_lossy(&bytes, limit),
                    Err(DecodingError::MemoryLimitExceeded)
                ));
            }
            assert_eq!(
                decode_lossy(&bytes, 4096).unwrap(),
                decode_lossy(&bytes, usize::MAX).unwrap()
            );
            // In particular, ALPH+VP8 animation must reject the embedded size
            // before allocating its YUV planes or writing the declared frame.
            for width in [1, 8192] {
                assert!(matches!(
                    decode_lossy(&lossy(animated, alpha, width, b"VP8 "), 4096),
                    Err(DecodingError::InconsistentImageSizes)
                ));
            }
        }
    }
    assert!(matches!(
        decode_lossy(&lossy(true, true, 2, b"JUNK"), 4096),
        Err(DecodingError::ChunkHeaderInvalid(_))
    ));
    let mut short_frame = lossy(true, true, 2, b"VP8 ");
    assert_eq!(&short_frame[44..48], b"ANMF");
    let length = u32::from_le_bytes(short_frame[48..52].try_into().unwrap());
    short_frame[48..52].copy_from_slice(&(length - 1).to_le_bytes());
    // RIFF alignment still reaches the same following byte, but the VP8
    // payload exceeds the ANMF payload after ALPH's mandatory odd-byte pad.
    assert!(matches!(
        decode_lossy(&short_frame, 4096),
        Err(DecodingError::ChunkHeaderInvalid(_))
    ));
}

#[test]
fn lossy_admission_refuses_declared_and_unbounded_final_partition_growth() {
    let simple = |payload: &[u8]| {
        let mut body = b"WEBP".to_vec();
        chunk(&mut body, b"VP8 ", payload);
        riff(body)
    };
    let mut large_declaration = VP8;
    let tag = (0x1f_ffffu32 << 5) | 0x10;
    large_declaration[..3].copy_from_slice(&tag.to_le_bytes()[..3]);
    assert!(matches!(
        decode_lossy(&simple(&large_declaration), 64 * 1024),
        Err(DecodingError::MemoryLimitExceeded)
    ));

    let mut long_tail = VP8.to_vec();
    long_tail.resize(128 * 1024, 0);
    assert!(matches!(
        decode_lossy(&simple(&long_tail), 64 * 1024),
        Err(DecodingError::MemoryLimitExceeded)
    ));
    assert_eq!(
        decode_lossy(&simple(&long_tail), 512 * 1024).unwrap(),
        decode_lossy(&simple(&VP8), 4096).unwrap()
    );
}
