use super::*;
use crate::{
    engine::native::worker::tests::Fixture,
    image_import::{
        png_admission,
        prepared_png::{
            tests::{adam7_fixture, chunk, fixture},
            FileLimits, FilePng,
        },
    },
};
use serde_json::json;
use std::io::Write;

fn compare(bytes: Vec<u8>) {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base = NativeDocument::new_blank(4, 4, 8, &runtime.resident(), &runtime.disk(), &|| Ok(()))
        .unwrap();
    let path = fixture.path().join("source.png");
    std::fs::write(&path, &bytes).unwrap();
    let expected = crate::image_import::Encoded::png(bytes)
        .unwrap()
        .decode(&json!({}), crate::image_import::BUDGET)
        .unwrap();
    let mut file = FilePng::open(
        &session,
        &path,
        FileLimits::SELECTION,
        ColorPolicy::ImportSource,
        &mut || Ok(()),
    )
    .unwrap();
    let input = png_admission::stage_file(&session, &mut file, &mut || Ok(())).unwrap();
    let disk = runtime.disk().used().unwrap();
    let output = prepare(&base, &session, &input, &|| Ok(())).unwrap();
    file.recheck().unwrap();
    let surface = output.surface();
    assert_eq!(
        (surface.width(), surface.height(), surface.depth()),
        (expected.width, expected.height, expected.depth)
    );
    for y in 0..surface.height() as i32 {
        let row = surface
            .prepare_region(
                Region::new([0, y, surface.width() as i32, y + 1]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        for x in 0..surface.width() as i32 {
            assert_eq!(
                row.pixel16(x, y).unwrap(),
                expected.get16(x, y),
                "at {x},{y}"
            );
        }
    }
    output.abort(&session, &|| session.cleanup_check()).unwrap();
    assert_eq!(runtime.disk().used().unwrap(), disk);
    input.abort(&session).unwrap();
    assert_eq!(runtime.disk().used().unwrap(), 0);
    drop(file);
    drop((base, session, runtime, connection));
}

#[test]
fn prepared_png_import_matches_all_orientations_color_layouts_and_native_icc_rows() {
    for orientation in 1..=8 {
        for (color, bits) in [
            (png::ColorType::Rgba, png::BitDepth::Eight),
            (png::ColorType::Rgba, png::BitDepth::Sixteen),
            (png::ColorType::GrayscaleAlpha, png::BitDepth::Sixteen),
            (png::ColorType::Indexed, png::BitDepth::Two),
        ] {
            for profile in [
                None,
                Some(moxcms::ColorProfile::new_display_p3().encode().unwrap()),
            ] {
                compare(fixture(color, bits, profile, orientation));
            }
        }
    }
}

#[test]
fn prepared_import_handles_adam7_and_rotations_across_tile_boundaries() {
    for depth in [8, 16] {
        let original = adam7_fixture(depth);
        let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
        let mut compressor =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        compressor.write_all(&profile).unwrap();
        let mut tag = b"Profile\0\0".to_vec();
        tag.extend(compressor.finish().unwrap());
        let mut bytes = original[..33].to_vec();
        chunk(&mut bytes, b"iCCP", &tag);
        bytes.extend_from_slice(&original[33..]);
        compare(bytes);
        for orientation in 1..=8u8 {
            let mut info = png::Info::with_size(267, 259);
            info.color_type = png::ColorType::Rgba;
            info.bit_depth = if depth == 16 {
                png::BitDepth::Sixteen
            } else {
                png::BitDepth::Eight
            };
            let mut exif = b"II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            exif.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
            info.exif_metadata = Some(exif.into());
            let mut pixels = Vec::new();
            for y in 0..259 {
                for x in 0..267 {
                    for value in [
                        12347 + x as u16 * 37,
                        23459 + y as u16 * 41,
                        34571,
                        if (x + y) % 3 == 0 { 0 } else { 49153 },
                    ] {
                        if depth == 16 {
                            pixels.extend(value.to_be_bytes());
                        } else {
                            pixels.push(crate::raster::project16(value));
                        }
                    }
                }
            }
            let mut bytes = Vec::new();
            let encoder = png::Encoder::with_info(&mut bytes, info).unwrap();
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&pixels).unwrap();
            writer.finish().unwrap();
            compare(bytes);
        }
    }
}

#[test]
fn normalization_refuses_unvalidated_color_and_retires_interrupted_outputs() {
    let fixture_root = Fixture::new();
    let (runtime, connection) =
        fixture_root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let base =
        NativeDocument::new_blank(4, 4, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let path = fixture_root.path().join("source.png");
    std::fs::write(
        &path,
        fixture(png::ColorType::Rgba, png::BitDepth::Sixteen, None, 6),
    )
    .unwrap();
    for policy in [ColorPolicy::Raw, ColorPolicy::ImportSource] {
        let mut file = FilePng::open(&session, &path, FileLimits::SELECTION, policy, &mut || {
            Ok(())
        })
        .unwrap();
        let input = png_admission::stage_file(&session, &mut file, &mut || Ok(())).unwrap();
        let disk = runtime.disk().used().unwrap();
        let refused = prepare(&base, &session, &input, &|| {
            if runtime.disk().used().unwrap() > disk {
                Err("normalization interrupted".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(
            refused.contains(if policy == ColorPolicy::Raw {
                "color-validated"
            } else {
                "normalization interrupted"
            }),
            "{refused}"
        );
        assert_eq!(runtime.disk().used().unwrap(), disk);
        let pool = runtime.resident();
        let available = pool.limit() - pool.used();
        let held = pool.reserve_bytes(available, available).unwrap();
        assert!(prepare(&base, &session, &input, &|| Ok(())).is_err());
        drop(held);
        input.abort(&session).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 0);
    }
    drop((base, session, runtime, connection));
}

#[test]
fn import_source_admission_rejects_unmanaged_gamma_and_hdr() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(64 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let path = root.path().join("unmanaged.png");
    let source = fixture(png::ColorType::Rgba, png::BitDepth::Sixteen, None, 1);
    for (name, data) in [(b"gAMA", 100000u32.to_be_bytes()), (b"cICP", [9, 16, 0, 1])] {
        let mut bytes = source[..33].to_vec();
        chunk(&mut bytes, name, &data);
        bytes.extend_from_slice(&source[33..]);
        assert!(crate::image_import::Encoded::png(bytes.clone()).is_err());
        std::fs::write(&path, bytes).unwrap();
        assert!(FilePng::open(
            &session,
            &path,
            FileLimits::SELECTION,
            ColorPolicy::ImportSource,
            &mut || Ok(())
        )
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), 0);
    }
    drop((session, runtime, connection));
}
