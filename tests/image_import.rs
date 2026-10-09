use image::{DynamicImage, ImageFormat};
use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    image_import::{self, Encoded, BUDGET},
    raster::Raster,
    server,
};
use serde_json::{json, Value};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

struct File(PathBuf);
impl File {
    fn new(ext: &str, bytes: &[u8]) -> Self {
        let path =
            std::env::temp_dir().join(format!("peerbrush-import-{}.{}", uuid::Uuid::new_v4(), ext));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn rgba(color: [u8; 4]) -> image::RgbaImage {
    image::RgbaImage::from_pixel(2, 1, image::Rgba(color))
}
fn encoded(format: ImageFormat) -> Vec<u8> {
    let image =
        DynamicImage::ImageRgb8(image::RgbImage::from_pixel(2, 1, image::Rgb([170, 60, 25])));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, format).unwrap();
    bytes.into_inner()
}
fn decode(path: &Path, choice: Value) -> Raster {
    image_import::decode_file(path, &choice, BUDGET).unwrap()
}
fn gif() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = image::codecs::gif::GifEncoder::new(&mut bytes);
    encoder
        .encode_frames([
            image::Frame::new(rgba([255, 0, 0, 255])),
            image::Frame::new(rgba([0, 255, 0, 255])),
        ])
        .unwrap();
    drop(encoder);
    bytes
}
fn chunk(bytes: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(data);
    let mut crc = 0xffffffffu32;
    for &byte in name.iter().chain(data) {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    bytes.extend_from_slice(&(!crc).to_be_bytes());
}
fn apng(depth: u8) -> Vec<u8> {
    use std::io::Write;
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = Vec::new();
    header.extend_from_slice(&2u32.to_be_bytes());
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&[depth, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &header);
    chunk(&mut out, b"acTL", &[0, 0, 0, 2, 0, 0, 0, 0]);
    for i in 0..2u32 {
        let mut control = Vec::new();
        control.extend_from_slice(&(if i == 0 { 0u32 } else { 1u32 }).to_be_bytes());
        control.extend_from_slice(&2u32.to_be_bytes());
        control.extend_from_slice(&1u32.to_be_bytes());
        control.extend_from_slice(&[0; 8]);
        control.extend_from_slice(&[0, 1, 0, 10, 0, 0]);
        chunk(&mut out, b"fcTL", &control);
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&[0]).unwrap();
        let color = if i == 0 {
            [255, 0, 0, 255]
        } else {
            [0, 0, 255, 255]
        };
        for _ in 0..2 {
            for v in color {
                if depth == 16 {
                    z.write_all(&[v, v]).unwrap();
                } else {
                    z.write_all(&[v]).unwrap();
                }
            }
        }
        let compressed = z.finish().unwrap();
        if i == 0 {
            chunk(&mut out, b"IDAT", &compressed);
        } else {
            let mut data = 2u32.to_be_bytes().to_vec();
            data.extend(compressed);
            chunk(&mut out, b"fdAT", &data);
        }
    }
    chunk(&mut out, b"IEND", &[]);
    out
}
fn riff_chunk(bytes: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    bytes.extend_from_slice(name);
    bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
    bytes.extend_from_slice(data);
    if data.len() % 2 != 0 {
        bytes.push(0);
    }
}
fn animated_webp() -> Vec<u8> {
    let mut body = b"WEBP".to_vec();
    riff_chunk(&mut body, b"VP8X", &[2, 0, 0, 0, 1, 0, 0, 0, 0, 0]);
    riff_chunk(&mut body, b"ANIM", &[0; 6]);
    for color in [[255, 0, 0, 255], [0, 255, 0, 255]] {
        let mut file = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(rgba(color))
            .write_to(&mut file, ImageFormat::WebP)
            .unwrap();
        let mut frame = vec![0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 100, 0, 0, 2];
        frame.extend_from_slice(&file.into_inner()[12..]);
        riff_chunk(&mut body, b"ANMF", &frame);
    }
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend(body);
    out
}
fn icon() -> Vec<u8> {
    let images = [
        peerbrush::raster::png(2, 1, &[255; 8]).unwrap(),
        peerbrush::raster::png(3, 2, &[80; 24]).unwrap(),
    ];
    let mut out = vec![0, 0, 1, 0, 2, 0];
    let mut offset = 38;
    for (i, image) in images.iter().enumerate() {
        out.extend_from_slice(&[if i == 0 { 2 } else { 3 }, if i == 0 { 1 } else { 2 }, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += image.len();
    }
    for image in images {
        out.extend(image);
    }
    out
}
// Independent uncompressed classic-TIFF fixture with RGB, alpha and orientation.
fn tiff(pages: &[(u16, u16, [u16; 4])]) -> Vec<u8> {
    let mut out = b"II\x2a\x00\x08\x00\x00\x00".to_vec();
    let mut previous = None;
    for &(depth, orientation, color) in pages {
        let at = out.len();
        if let Some(link) = previous {
            out[link..link + 4].copy_from_slice(&(at as u32).to_le_bytes());
        }
        let entries = 12;
        let bits = at + 2 + entries * 12 + 4;
        let pixels = bits + 8;
        out.extend_from_slice(&(entries as u16).to_le_bytes());
        let fields = [
            (256, 4, 1, 2),
            (257, 4, 1, 1),
            (258, 3, 4, bits as u32),
            (259, 3, 1, 1),
            (262, 3, 1, 2),
            (273, 4, 1, pixels as u32),
            (274, 3, 1, orientation as u32),
            (277, 3, 1, 4),
            (278, 4, 1, 1),
            (279, 4, 1, 8 * (depth as u32 / 8)),
            (284, 3, 1, 1),
            (338, 3, 1, 2),
        ];
        for (tag, kind, count, value) in fields {
            out.extend_from_slice(&(tag as u16).to_le_bytes());
            out.extend_from_slice(&(kind as u16).to_le_bytes());
            out.extend_from_slice(&(count as u32).to_le_bytes());
            out.extend_from_slice(&value.to_le_bytes());
        }
        previous = Some(out.len());
        out.extend_from_slice(&0u32.to_le_bytes());
        for _ in 0..4 {
            out.extend_from_slice(&depth.to_le_bytes());
        }
        for _ in 0..2 {
            for v in color {
                if depth == 16 {
                    out.extend_from_slice(&v.to_le_bytes());
                } else {
                    out.push((v / 257) as u8);
                }
            }
        }
    }
    out
}

#[test]
fn common_static_formats_share_bounded_decoding() {
    for (ext, format) in [
        ("png", ImageFormat::Png),
        ("jpg", ImageFormat::Jpeg),
        ("bmp", ImageFormat::Bmp),
        ("gif", ImageFormat::Gif),
        ("tif", ImageFormat::Tiff),
        ("webp", ImageFormat::WebP),
        ("ico", ImageFormat::Ico),
        ("ppm", ImageFormat::Pnm),
        ("tga", ImageFormat::Tga),
        ("qoi", ImageFormat::Qoi),
    ] {
        let file = File::new(ext, &encoded(format));
        let info = image_import::inspect(&file.0).unwrap();
        assert_eq!(info.count, 1, "{ext}");
        let raster = decode(&file.0, json!({}));
        assert_eq!(
            (raster.width, raster.height, raster.depth),
            (2, 1, 8),
            "{ext}"
        );
        assert_eq!(raster.get(0, 0)[3], 255);
        assert!(image_import::decode_file(&file.0, &json!({}), 1).is_err());
    }
}
#[test]
fn animations_require_explicit_frames_and_preserve_composited_color() {
    for (ext, bytes, expected) in [
        ("gif", gif(), [0, 255, 0, 255]),
        ("webp", animated_webp(), [0, 255, 0, 255]),
        ("png", apng(8), [0, 0, 255, 255]),
    ] {
        let file = File::new(ext, &bytes);
        let info = image_import::inspect(&file.0).unwrap();
        assert_eq!((info.choice, info.count), (Some("frame"), 2));
        for choice in [
            json!({}),
            json!({"frame":2}),
            json!({"frame":-1}),
            json!({"frame":0.5}),
            json!({"page":0}),
        ] {
            assert!(
                image_import::decode_file(&file.0, &choice, BUDGET).is_err(),
                "{ext}: {choice}"
            );
        }
        assert_eq!(
            decode(&file.0, json!({"frame":1})).get(0, 0),
            expected,
            "{ext}"
        );
    }
    assert!(Encoded::png(apng(16))
        .err()
        .unwrap()
        .contains("original precision"));
}
#[test]
fn native16_pages_and_icons_never_choose_or_narrow_silently() {
    let words = [12347, 33559, 51237, 45679];
    let file = File::new("tiff", &tiff(&[(8, 1, [257; 4]), (16, 6, words)]));
    let original = std::fs::read(&file.0).unwrap();
    let info = image_import::inspect(&file.0).unwrap();
    assert_eq!((info.choice, info.count), (Some("page"), 2));
    assert!(image_import::decode_file(&file.0, &json!({}), BUDGET).is_err());
    let raster = decode(&file.0, json!({"page":1}));
    assert_eq!((raster.width, raster.height, raster.depth), (1, 2, 16));
    assert_eq!(raster.get16(0, 1), words);
    assert_eq!(std::fs::read(&file.0).unwrap(), original);
    let file = File::new("ico", &icon());
    assert!(image_import::decode_file(&file.0, &json!({}), BUDGET).is_err());
    let raster = decode(&file.0, json!({"variant":1}));
    assert_eq!((raster.width, raster.height), (3, 2));
    assert_eq!(raster.get(0, 0), [80; 4]);
    let mut pnm = b"P6\n2 1\n65535\n".to_vec();
    for _ in 0..2 {
        for word in &words[..3] {
            pnm.extend_from_slice(&word.to_be_bytes());
        }
    }
    let file = File::new("ppm", &pnm);
    let raster = decode(&file.0, json!({}));
    assert_eq!(raster.depth, 16);
    assert_eq!(raster.get16(1, 0), [words[0], words[1], words[2], 65535]);
}

#[test]
fn standard_icc_tag_and_embedded_png_icon_keep_native_words() {
    let words = [12347, 33559, 51237, 45679];
    let png = peerbrush::raster::png16_with_profile(
        2,
        1,
        &[words, words].concat(),
        Some(peerbrush::color_profile::srgb_profile()),
    )
    .unwrap();
    let file = File::new("png", &png);
    assert_eq!(decode(&file.0, json!({})).get16(1, 0), words);
    let mut icon = vec![0, 0, 1, 0, 1, 0, 2, 1, 0, 0, 1, 0, 64, 0];
    icon.extend_from_slice(&(png.len() as u32).to_le_bytes());
    icon.extend_from_slice(&22u32.to_le_bytes());
    icon.extend(png);
    let file = File::new("ico", &icon);
    assert_eq!(decode(&file.0, json!({})).get16(1, 0), words);
}
#[test]
fn native16_import_and_placement_promote_and_roundtrip_with_one_undo() {
    let words = [12347, 33559, 51237, 45679];
    let bytes = peerbrush::raster::png16(2, 1, &[words, words].concat()).unwrap();
    let file = File::new("png", &bytes);
    for existing in [false, true] {
        let mut engine = Engine::new();
        engine.doc = Document::new(4, 4).unwrap();
        let target = engine.doc.layers[0].id.clone();
        let command = if existing {
            json!({"op":"image.place","path":file.0,"layer":target,"new_layer":false,"mode":"replace","rect":[1,1,3,2]})
        } else {
            json!({"op":"image.import","path":file.0,"x":1,"y":1})
        };
        engine
            .edit("human", &[command], Some(0), None, "Import native16")
            .unwrap();
        assert_eq!(engine.doc.bit_depth, 16);
        assert!(engine.doc.srgb_tagged);
        let layer = &engine.doc.layers[0];
        assert_eq!(
            layer
                .pixels
                .get16(if existing { 1 } else { 0 }, if existing { 1 } else { 0 }),
            words
        );
        let encoded = peerbrush::psd::encode(&engine.doc).unwrap();
        let reopened = peerbrush::psd::decode(&encoded).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(
            reopened.export_png().unwrap(),
            engine.doc.export_png().unwrap()
        );
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.bit_depth, 8);
        assert_eq!(engine.doc.layers.len(), 1);
        assert_eq!(engine.doc.layers[0].pixels.get(1, 1), [0; 4]);
    }
}

#[test]
fn jpeg_precision_and_four_channel_headers_require_explicit_conversion() {
    for (precision, channels, reason) in [(8, 4, "CMYK"), (12, 3, "precision")] {
        let mut jpeg = vec![0xff, 0xd8, 0xff, 0xc0];
        jpeg.extend_from_slice(&(8u16 + channels as u16 * 3).to_be_bytes());
        jpeg.extend_from_slice(&[precision, 0, 1, 0, 1, channels]);
        for id in 1..=channels {
            jpeg.extend_from_slice(&[id, 0x11, 0]);
        }
        jpeg.extend_from_slice(&[0xff, 0xd9]);
        let file = File::new("jpg", &jpeg);
        assert!(image_import::inspect(&file.0)
            .err()
            .unwrap()
            .contains(reason));
    }
}

#[test]
fn draft_and_commit_share_the_cumulative_import_budget_and_preserve_work() {
    // One small encoded file expands to a real dense 32 MiB raster. Multiple
    // copies must fail before a draft/commit can accumulate unbounded storage.
    let mut out = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        2048,
        4096,
        image::Rgba([10, 20, 30, 255]),
    ))
    .write_to(&mut out, ImageFormat::Png)
    .unwrap();
    let file = File::new("png", &out.into_inner());
    let mut engine = Engine::new();
    engine.doc = Document::new(4, 4).unwrap();
    let before = engine.doc.export_png().unwrap();
    let commands = vec![json!({"op":"image.import","path":file.0}); 9];
    assert!(Engine::preview_edits(engine.doc.clone(), &commands)
        .err()
        .unwrap()
        .contains("256 MiB"));
    assert!(engine
        .edit("human", &commands, Some(0), None, "Oversized imports")
        .err()
        .unwrap()
        .contains("256 MiB"));
    assert_eq!(engine.doc.layers.len(), 1);
    assert_eq!(engine.doc.revision, 0);
    assert!(!engine.doc.srgb_tagged);
    assert!(engine.undo.is_empty());
    assert_eq!(engine.doc.export_png().unwrap(), before);
}
#[test]
fn batch_failures_locks_reservations_and_stale_guards_are_atomic() {
    let file = File::new("bmp", &encoded(ImageFormat::Bmp));
    let mut engine = Engine::new();
    engine.doc = Document::new(4, 4).unwrap();
    let mut folder = Layer::new("Imported", "group", 4, 4);
    let parent = folder.id.clone();
    folder.locked = true;
    engine.doc.layers.insert(0, folder);
    let import = json!({"op":"image.import","path":file.0,"parent":parent});
    assert!(engine
        .edit("human", &[import.clone()], None, None, "Import")
        .is_err());
    engine.doc.layers[0].locked = false;
    let _lease = engine
        .reserve(
            "other",
            "Reserved folder",
            vec![Scope {
                target: Some(parent.clone()),
                rect: None,
            }],
        )
        .unwrap();
    assert!(engine
        .edit("human", &[import.clone()], None, None, "Import")
        .is_err());
    engine.leases.clear();
    for commands in [
        vec![
            import.clone(),
            json!({"op":"image.import","path":file.0,"page":0}),
        ],
        vec![json!({"op":"image.import","path":file.0,"source_revision":1})],
        vec![json!({"op":"image.import","path":file.0,"document_id":"closed"})],
    ] {
        let before = engine.doc.export_png().unwrap();
        assert!(engine
            .edit("human", &commands, Some(0), None, "Import")
            .is_err());
        assert_eq!(engine.doc.revision, 0);
        assert_eq!(engine.doc.layers.len(), 2);
        assert!(engine.undo.is_empty());
        assert_eq!(engine.doc.export_png().unwrap(), before);
    }
    engine
        .edit("human", &[import], Some(0), None, "Import")
        .unwrap();
    assert_eq!(
        engine.doc.layers[1].parent.as_deref(),
        Some(parent.as_str())
    );
}
#[test]
fn format_mismatch_malformed_containers_float_and_hdr_are_rejected() {
    let mismatch = File::new("jpg", &encoded(ImageFormat::Png));
    assert!(image_import::inspect(&mismatch.0).is_err());
    for (ext, bytes) in [
        ("gif", gif()[..18].to_vec()),
        ("ico", vec![0, 0, 1, 0, 255, 255]),
        ("tif", b"II\x2a\x00\xff\xff\xff\xff".to_vec()),
        ("webp", b"RIFF\xff\xff\xff\xffWEBP".to_vec()),
    ] {
        let file = File::new(ext, &bytes);
        assert!(image_import::inspect(&file.0).is_err());
    }
    let image = DynamicImage::ImageRgb32F(image::Rgb32FImage::from_pixel(
        2,
        1,
        image::Rgb([0.1, 0.2, 0.3]),
    ));
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Tiff).unwrap();
    let file = File::new("tif", &bytes.into_inner());
    assert!(image_import::inspect(&file.0).is_err());
    let png = encoded(ImageFormat::Png);
    let mut hdr = png[..33].to_vec();
    chunk(&mut hdr, b"cICP", &[9, 16, 0, 1]);
    hdr.extend_from_slice(&png[33..]);
    assert!(Encoded::png(hdr).is_err());
}
#[test]
fn protocol_exposes_import_inspection_and_precise_frame_targeting() {
    let file = File::new("gif", &gif());
    let shared = Arc::new(Mutex::new(Engine::new()));
    let result = server::mcp(
        &shared,
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_image_info","arguments":{"path":file.0}}}),
    );
    assert_eq!(result["result"]["isError"], false);
    let value: Value =
        serde_json::from_str(result["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(value["choice"], "frame");
    assert_eq!(value["count"], 2);
    assert!(shared.lock().unwrap().undo.is_empty());
    let info = server::capabilities();
    for format in [
        "PNG", "JPEG", "BMP", "GIF", "TIFF", "WebP", "ICO", "Netpbm", "TGA", "QOI",
    ] {
        assert!(info["formats"]["import"]
            .as_array()
            .unwrap()
            .contains(&json!(format)));
    }
    assert!(info["color_profiles"]["source"]
        .as_str()
        .unwrap()
        .contains("original channel depth"));
    assert!(info["image_placement"]
        .as_str()
        .unwrap()
        .contains("frame/page/variant"));
    assert!(info["image_import"]["extensions"]
        .as_array()
        .unwrap()
        .contains(&json!("webp")));
    let command = json!({"op":"image.import","path":file.0,"frame":1});
    shared
        .lock()
        .unwrap()
        .edit("human", &[command], Some(0), None, "Chosen frame")
        .unwrap();
    assert_eq!(
        shared.lock().unwrap().doc.layers[0].pixels.get(0, 0),
        [0, 255, 0, 255]
    );
}
