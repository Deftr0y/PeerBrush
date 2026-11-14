use image::{DynamicImage, ImageFormat};
use peerbrush::{
    engine::{Document, Engine},
    image_import::{decode_file, BUDGET},
};
use serde_json::json;
use std::{io::Cursor, path::PathBuf};

struct File(PathBuf);
impl File {
    fn new(extension: &str, bytes: &[u8]) -> Self {
        let path = std::env::temp_dir().join(format!(
            "peerbrush-jpeg-{}.{}",
            uuid::Uuid::new_v4(),
            extension
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn original() -> Vec<u8> {
    let image =
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 1, image::Rgb([170, 60, 25])));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Jpeg).unwrap();
    bytes.into_inner()
}
fn with_parts(source: &[u8], parts: &[(u8, u8, &[u8])]) -> Vec<u8> {
    let mut bytes = source[..2].to_vec();
    for &(sequence, count, profile) in parts {
        let mut part = b"ICC_PROFILE\0".to_vec();
        part.extend_from_slice(&[sequence, count]);
        part.extend_from_slice(profile);
        bytes.extend_from_slice(&[0xff, 0xe2]);
        bytes.extend_from_slice(&((part.len() + 2) as u16).to_be_bytes());
        bytes.extend_from_slice(&part);
    }
    bytes.extend_from_slice(&source[2..]);
    bytes
}

#[test]
fn fragmented_jpeg_profile_conversion_matches_identical_png_samples() {
    let original = original();
    let source = File::new("jpg", &original);
    let raw = decode_file(&source.0, &json!({}), BUDGET).unwrap();
    let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    let middle = profile.len() / 2;
    let jpeg = File::new(
        "jpeg",
        &with_parts(
            &original,
            &[(2, 2, &profile[middle..]), (1, 2, &profile[..middle])],
        ),
    );
    let mut info = png::Info::with_size(2, 1);
    info.color_type = png::ColorType::Rgba;
    info.bit_depth = png::BitDepth::Eight;
    info.icc_profile = Some(std::borrow::Cow::Borrowed(&profile));
    let mut png = Vec::new();
    {
        let mut writer = png::Encoder::with_info(&mut png, info)
            .unwrap()
            .write_header()
            .unwrap();
        writer
            .write_image_data(&[raw.get(0, 0), raw.get(1, 0)].concat())
            .unwrap();
        writer.finish().unwrap();
    }
    let png = File::new("png", &png);
    let expected = decode_file(&png.0, &json!({}), BUDGET).unwrap();
    let actual = decode_file(&jpeg.0, &json!({}), BUDGET).unwrap();
    assert_eq!(actual.get(0, 0), expected.get(0, 0));
    assert_eq!(actual.get(1, 0), expected.get(1, 0));
    assert_ne!(actual.get(0, 0), raw.get(0, 0));
}

#[test]
fn malformed_declared_jpeg_profiles_refuse_import_place_and_paste_atomically() {
    let original = original();
    let good = File::new("jpg", &original);
    let profile = moxcms::ColorProfile::new_display_p3().encode().unwrap();
    for parts in [
        vec![(1, 2, profile.as_slice())],
        vec![(0, 1, profile.as_slice())],
        vec![(2, 1, profile.as_slice())],
        vec![(1, 1, profile.as_slice()), (1, 1, profile.as_slice())],
        vec![(1, 1, b"invalid-profile".as_slice())],
        vec![(1, 1, b"".as_slice())],
    ] {
        let bad = File::new("jpeg", &with_parts(&original, &parts));
        assert!(decode_file(&bad.0, &json!({}), BUDGET).is_err());
        for depth in [8, 16] {
            for op in ["image.import", "image.place", "image.paste"] {
                let mut engine = Engine::new();
                engine.resident_state_mut().expect("resident fixture").document = Document::new_depth(4, 4, depth).unwrap();
                let target = engine.resident_state().expect("resident fixture").document.layers[0].id.clone();
                let before = serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap();
                assert!(engine
                    .edit(
                        "human",
                        &[
                            json!({"op":"image.import","path":good.0}),
                            json!({"op":op,"path":bad.0,"layer":target})
                        ],
                        Some(0),
                        None,
                        "Rejected JPEG"
                    )
                    .is_err());
                assert_eq!(serde_json::to_value(&engine.resident_state().expect("resident fixture").document).unwrap(), before);
                assert!(engine.resident_state().expect("resident fixture").undo.is_empty());
            }
        }
    }
}
