use image::{DynamicImage, ImageFormat};
use peerbrush::{
    engine::{self, Engine},
    server, workspace,
};
use serde_json::json;
use std::{
    fs,
    io::Cursor,
    path::PathBuf,
    sync::{Arc, Mutex},
};

struct File(PathBuf);
impl File {
    fn new(extension: &str, bytes: &[u8]) -> Self {
        let path =
            std::env::temp_dir().join(format!("pb-image-open-{}.{}", engine::id(), extension));
        fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
fn shared() -> server::Shared {
    Arc::new(Mutex::new(Engine::new()))
}
fn png(depth: u16) -> Vec<u8> {
    if depth == 16 {
        peerbrush::raster::png16(3, 2, &[12345, 23457, 45679, 32123].repeat(6)).unwrap()
    } else {
        peerbrush::raster::png(3, 2, &[123, 45, 67, 89].repeat(6)).unwrap()
    }
}

#[test]
fn opening_images_creates_independent_unsaved_projects_and_exact_native_psd_sources() {
    for depth in [8, 16] {
        let file = File::new("png", &png(depth));
        let root = shared();
        let layer = root.lock().unwrap().doc.layers[0].id.clone();
        server::dispatch(&root,"edit",&json!({"actor":"human","commands":[{"op":"layer.update","layer":layer,"name":"Keep existing work"}]})).unwrap();
        let before = root.lock().unwrap().state();
        let history = root.lock().unwrap().undo.len();
        let result = server::dispatch(
            &root,
            "projects",
            &json!({"action":"open","actor":"human","path":file.0}),
        )
        .unwrap();
        let opened = workspace::get(&root, result["project_id"].as_str().unwrap()).unwrap();
        assert_eq!(root.lock().unwrap().state(), before);
        assert_eq!(root.lock().unwrap().undo.len(), history);
        let e = opened.lock().unwrap();
        assert_eq!((e.doc.width, e.doc.height, e.doc.bit_depth), (3, 2, depth));
        assert!(e.path.is_none() && e.file_version.is_none());
        assert_eq!(e.saved_revision, u64::MAX);
        assert!(e.undo.is_empty() && !e.doc.read_only && e.doc.srgb_tagged);
        let expected = if depth == 16 {
            [12345, 23457, 45679, 32123]
        } else {
            [123 * 257, 45 * 257, 67 * 257, 89 * 257]
        };
        assert_eq!(e.doc.layers[0].pixels.get16(1, 1), expected);
        let encoded = peerbrush::psd::encode(&e.doc).unwrap();
        let decoded = peerbrush::psd::decode(&encoded).unwrap();
        assert_eq!(decoded.bit_depth, depth);
        assert_eq!(decoded.layers[0].pixels.get16(1, 1), expected);
        drop(e);
        assert!(
            server::dispatch(&root, "projects", &json!({"action":"open","path":file.0}))
                .unwrap_err()
                .starts_with("File already open")
        );
    }
}

#[test]
fn jpeg_and_static_formats_keep_dimensions_and_use_shared_decoders() {
    for (extension, format) in [
        ("jpg", ImageFormat::Jpeg),
        ("bmp", ImageFormat::Bmp),
        ("tiff", ImageFormat::Tiff),
        ("webp", ImageFormat::WebP),
        ("qoi", ImageFormat::Qoi),
    ] {
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(7, 5, image::Rgb([170, 60, 25])))
            .write_to(&mut bytes, format)
            .unwrap();
        let file = File::new(extension, &bytes.into_inner());
        let s = shared();
        server::open(&s, &file.0).unwrap();
        let e = s.lock().unwrap();
        assert_eq!((e.doc.width, e.doc.height, e.doc.bit_depth), (7, 5, 8));
        assert_eq!(e.doc.layers[0].pixels.get(3, 2)[3], 255);
    }
}

#[test]
fn animated_images_require_an_explicit_choice_and_open_selected_composited_frame() {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
    encoder
        .encode_frames(
            [[255, 0, 0, 255], [0, 255, 0, 255]]
                .into_iter()
                .map(|c| image::Frame::new(image::RgbaImage::from_pixel(4, 3, image::Rgba(c)))),
        )
        .unwrap();
    drop(encoder);
    let file = File::new("gif", &bytes);
    let s = shared();
    let before = s.lock().unwrap().doc.id.clone();
    assert!(server::open(&s, &file.0)
        .unwrap_err()
        .to_lowercase()
        .contains("frame"));
    assert_eq!(s.lock().unwrap().doc.id, before);
    server::open_options(&s, &file.0, &Default::default(), &json!({"frame":1})).unwrap();
    assert_eq!(
        s.lock().unwrap().doc.layers[0].pixels.get(1, 1),
        [0, 255, 0, 255]
    );
}

#[test]
fn unsupported_or_canceled_open_preserves_work_and_releases_source_claims() {
    let invalid = File::new("png", b"invalid PNG");
    let s = shared();
    let before = s.lock().unwrap().doc.id.clone();
    assert!(server::open(&s, &invalid.0).is_err());
    assert_eq!(s.lock().unwrap().doc.id, before);
    fs::write(&invalid.0, png(16)).unwrap();
    let control = peerbrush::loading::Control::default();
    control.cancel();
    assert!(server::open_options(&s, &invalid.0, &control, &json!({}))
        .unwrap_err()
        .contains("canceled"));
    assert!(s.lock().unwrap().loading.is_none());
    server::open(&s, &invalid.0).unwrap();
    let unsupported = File::new("pdf", b"%PDF-1.4");
    let id = s.lock().unwrap().doc.id.clone();
    assert!(server::open(&s, &unsupported.0)
        .unwrap_err()
        .contains("Unsupported"));
    assert_eq!(s.lock().unwrap().doc.id, id);
}

#[test]
fn image_sources_cannot_be_exported_over_and_save_as_releases_original() {
    let file = File::new("png", &png(16));
    let original = fs::read(&file.0).unwrap();
    let s = shared();
    server::open(&s, &file.0).unwrap();
    assert!(server::dispatch(
        &s,
        "document",
        &json!({"action":"export","actor":"human","path":file.0,"overwrite":true})
    )
    .unwrap_err()
    .contains("working source"));
    let other = shared();
    assert!(server::dispatch(
        &other,
        "document",
        &json!({"action":"export","actor":"human","path":file.0,"overwrite":true})
    )
    .unwrap_err()
    .starts_with("File already open"));
    assert_eq!(fs::read(&file.0).unwrap(), original);
    let destination = File::new("psd", b"");
    fs::remove_file(&destination.0).unwrap();
    server::save(&s, &destination.0).unwrap();
    assert_eq!(fs::read(&file.0).unwrap(), original);
    server::open(&other, &file.0).unwrap();
    assert_eq!(s.lock().unwrap().doc.bit_depth, 16);
}

#[test]
fn svg_opens_with_editable_vector_source_and_document_open_requires_discard() {
    let file=File::new("svg",br##"<svg xmlns="http://www.w3.org/2000/svg" width="9" height="7"><rect width="9" height="7" fill="#e95420"/></svg>"##);
    let s = shared();
    let layer = s.lock().unwrap().doc.layers[0].id.clone();
    server::dispatch(
        &s,
        "edit",
        &json!({"actor":"human","commands":[{"op":"layer.update","layer":layer,"name":"Unsaved"}]}),
    )
    .unwrap();
    let id = s.lock().unwrap().doc.id.clone();
    assert!(server::dispatch(
        &s,
        "document",
        &json!({"action":"open","actor":"human","path":file.0})
    )
    .unwrap_err()
    .contains("Unsaved"));
    assert_eq!(s.lock().unwrap().doc.id, id);
    server::dispatch(
        &s,
        "document",
        &json!({"action":"open","actor":"human","path":file.0,"discard":true}),
    )
    .unwrap();
    let e = s.lock().unwrap();
    assert_eq!((e.doc.width, e.doc.height), (9, 7));
    assert!(e.doc.layers[0].source.is_some());
    assert_eq!(e.doc.layers[0].pixels.get(4, 3), [233, 84, 32, 255]);
}
