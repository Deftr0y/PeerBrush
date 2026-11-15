use super::tests::{exercise_edits, Fixture};
use image::ImageEncoder;
use serde_json::json;

pub(crate) fn fixture(gray: bool) -> Vec<u8> {
    fixture_geometry(270, 19, gray)
}

pub(crate) fn fixture_geometry(width: u32, height: u32, gray: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    let channels = if gray { 1 } else { 3 };
    let pixels: Vec<u8> = (0..width * height * channels)
        .map(|n| ((n * 17 + n / 23) % 256) as u8)
        .collect();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 91)
        .write_image(
            &pixels,
            width,
            height,
            if gray {
                image::ExtendedColorType::L8
            } else {
                image::ExtendedColorType::Rgb8
            },
        )
        .unwrap();
    bytes
}

fn segment(bytes: &mut Vec<u8>, marker: u8, data: &[u8]) {
    bytes.extend_from_slice(&[0xff, marker]);
    bytes.extend_from_slice(&((data.len() + 2) as u16).to_be_bytes());
    bytes.extend_from_slice(data);
}

/// Independent zero-DCT JPEG: non-interleaved scans and progressive DC/AC scans.
pub(crate) fn scanned(progressive: bool, channels: u8) -> Vec<u8> {
    scanned_geometry(progressive, channels, 17, 9)
}

pub(crate) fn scanned_geometry(progressive: bool, channels: u8, width: u16, height: u16) -> Vec<u8> {
    scan_fixture(progressive, false, channels, width, height)
}

pub(crate) fn interleaved_geometry(width: u16, height: u16) -> Vec<u8> {
    scan_fixture(false, true, 3, width, height)
}

fn scan_fixture(progressive: bool, interleaved: bool, channels: u8, width: u16, height: u16) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xd8];
    if channels == 4 {
        segment(&mut bytes, 0xee, b"Adobe\0d\0\0\0\0\0");
    }
    let mut quant = vec![1; 65];
    quant[0] = 0;
    segment(&mut bytes, 0xdb, &quant);
    let mut tables = Vec::new();
    for class in [0, 0x10] {
        tables.extend_from_slice(&[class, 1]);
        tables.extend_from_slice(&[0; 15]);
        tables.push(0);
    }
    segment(&mut bytes, 0xc4, &tables);
    let mut frame = vec![8];
    frame.extend_from_slice(&height.to_be_bytes());
    frame.extend_from_slice(&width.to_be_bytes());
    frame.push(channels);
    for id in 1..=channels {
        frame.extend_from_slice(&[id, 0x11, 0]);
    }
    segment(&mut bytes, if progressive { 0xc2 } else { 0xc0 }, &frame);
    let zeros = |bytes: &mut Vec<u8>, bits: usize| {
        bytes.extend(std::iter::repeat_n(0, bits / 8));
        if bits % 8 != 0 {
            bytes.push((1u16 << (8 - bits % 8)) as u8 - 1);
        }
    };
    let blocks = usize::from(width).div_ceil(8) * usize::from(height).div_ceil(8);
    if progressive || interleaved {
        let mut scan = vec![channels];
        for id in 1..=channels {
            scan.extend_from_slice(&[id, 0]);
        }
        scan.extend_from_slice(&[0, if progressive { 0 } else { 63 }, 0]);
        segment(&mut bytes, 0xda, &scan);
        zeros(&mut bytes, blocks * channels as usize * if progressive { 1 } else { 2 });
    }
    for id in (1..=channels).filter(|_| progressive || !interleaved) {
        segment(
            &mut bytes,
            0xda,
            &[1, id, 0, if progressive { 1 } else { 0 }, 63, 0],
        );
        zeros(&mut bytes, if progressive { blocks } else { 2 * blocks });
    }
    bytes.extend_from_slice(&[0xff, 0xd9]);
    bytes
}

pub(crate) fn profiled(source: &[u8], valid: bool) -> Vec<u8> {
    let mut bytes = source[..2].to_vec();
    let profile = if valid {
        moxcms::ColorProfile::new_display_p3().encode().unwrap()
    } else {
        vec![0; 132]
    };
    let mut part = b"ICC_PROFILE\0\x01\x01".to_vec();
    part.extend_from_slice(&profile);
    segment(&mut bytes, 0xe2, &part);
    segment(
        &mut bytes,
        0xe1,
        b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0",
    );
    bytes.extend_from_slice(&source[2..]);
    bytes
}

#[test]
fn native_jpeg_import_place_paste_and_patch_match_resident_samples_and_undo() {
    let root = Fixture::new();
    let path = root.path().join("source.JPEG");
    for bytes in [
        fixture(false),
        fixture(true),
        profiled(&fixture(false), true),
        scanned(true, 3),
        scanned(true, 1),
        scanned(false, 3),
    ] {
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            let original = super::canvas_tests::document(depth);
            for command in [
                json!({"op":"image.import","path":path,"x":-3,"y":2,"parent":"B"}),
                json!({"op":"image.place","path":path,"layer":"outside","new_layer":false,"rect":[-2,1,9,8]}),
                json!({"op":"image.paste","path":path,"layer":"outside","origin":[-4,2]}),
                json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":2,"rect":[0,0,8,7]}),
            ] {
                exercise_edits(original.clone(), &[command], true);
            }
        }
    }
}

#[test]
fn jpeg_raw_patches_keep_cmyk_projection_and_ignore_import_color_metadata() {
    let root = Fixture::new();
    let path = root.path().join("raw.jpg");
    for bytes in [
        scanned(false, 4),
        scanned(true, 4),
        profiled(&fixture(false), false),
    ] {
        std::fs::write(&path, bytes).unwrap();
        for depth in [8, 16] {
            let original = super::canvas_tests::document(depth);
            exercise_edits(
                original.clone(),
                &[json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":2})],
                true,
            );
            for op in ["image.import", "image.place", "image.paste"] {
                exercise_edits(
                    original.clone(),
                    &[json!({"op":op,"path":path,"layer":"outside"})],
                    false,
                );
            }
        }
    }
}

#[test]
fn jpeg_refused_streams_and_late_reservations_retire_before_adoption() {
    super::qoi_tests::failed_streams("jpg", fixture(false));
}
