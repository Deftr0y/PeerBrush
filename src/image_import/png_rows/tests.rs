use super::*;
use crate::image_import::{Encoded, BUDGET};
use serde_json::{json, Value};
use std::{cell::RefCell, rc::Rc};

type Callback = Box<dyn FnMut(Option<u32>, usize, usize, usize)>;
thread_local! { static OBSERVER: RefCell<Option<Callback>> = const { RefCell::new(None) }; }
struct Observe;
impl Drop for Observe {
    fn drop(&mut self) {
        OBSERVER.with(|slot| {
            slot.take();
        });
    }
}
fn watching(callback: impl FnMut(Option<u32>, usize, usize, usize) + 'static) -> Observe {
    OBSERVER.with(|slot| {
        assert!(slot.borrow().is_none());
        slot.replace(Some(Box::new(callback)));
    });
    Observe
}
pub(super) fn observe(row: Option<u32>, input: usize, native: usize, stored: usize) {
    OBSERVER.with(|slot| {
        let mut callback = slot.take();
        if let Some(callback) = &mut callback {
            callback(row, input, native, stored);
        }
        slot.replace(callback);
    });
}

fn words(width: u32, height: u32, depth: u16, rgb: bool) -> Vec<u16> {
    (0..height)
        .flat_map(|y| {
            (0..width).flat_map(move |x| {
                let pixel = [
                    (12347 + x * 137 + y * 71) as u16,
                    (23459 + x * 47 + y * 113) as u16,
                    (34571 + x * 19 + y * 83) as u16,
                    if rgb {
                        65535
                    } else if (x + y) % 7 == 0 {
                        0
                    } else {
                        (12001 + x * 31 + y * 47) as u16
                    },
                ];
                pixel.map(|value| {
                    if depth == 16 {
                        value
                    } else {
                        u16::from(crate::raster::project16(value)) * 257
                    }
                })
            })
        })
        .collect()
}
fn chunk(output: &mut Vec<u8>, name: &[u8; 4], data: &[u8]) {
    output.extend_from_slice(&(data.len() as u32).to_be_bytes());
    output.extend_from_slice(name);
    output.extend_from_slice(data);
    let mut crc = 0xffffffffu32;
    for byte in name.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ if crc & 1 != 0 { 0xedb88320 } else { 0 };
        }
    }
    output.extend_from_slice(&(!crc).to_be_bytes());
}
fn zipped(raw: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut zip = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    zip.write_all(raw).unwrap();
    zip.finish().unwrap()
}
fn scanlines(
    width: u32,
    height: u32,
    depth: u16,
    rgb: bool,
    samples: &[u16],
    interlaced: bool,
) -> Vec<u8> {
    let mut raw = Vec::new();
    let passes = if interlaced {
        vec![
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
    } else {
        vec![(0, 0, 1, 1)]
    };
    for (left, top, dx, dy) in passes {
        if left >= width || top >= height {
            continue;
        }
        for y in (top..height).step_by(dy) {
            raw.push(0);
            for x in (left..width).step_by(dx) {
                let at = ((y * width + x) * 4) as usize;
                for value in &samples[at..at + if rgb { 3 } else { 4 }] {
                    if depth == 16 {
                        raw.extend_from_slice(&value.to_be_bytes());
                    } else {
                        raw.push((value / 257) as u8);
                    }
                }
            }
        }
    }
    raw
}
fn png(
    width: u32,
    height: u32,
    depth: u16,
    rgb: bool,
    samples: &[u16],
    interlaced: bool,
) -> Vec<u8> {
    let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut header = width.to_be_bytes().to_vec();
    header.extend_from_slice(&height.to_be_bytes());
    header.extend_from_slice(&[
        depth as u8,
        if rgb { 2 } else { 6 },
        0,
        0,
        u8::from(interlaced),
    ]);
    chunk(&mut output, b"IHDR", &header);
    chunk(
        &mut output,
        b"IDAT",
        &zipped(&scanlines(width, height, depth, rgb, samples, interlaced)),
    );
    chunk(&mut output, b"IEND", &[]);
    output
}
fn with_chunk(bytes: &[u8], name: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut output = bytes[..33].to_vec();
    chunk(&mut output, name, data);
    output.extend_from_slice(&bytes[33..]);
    output
}
fn replace_idat(bytes: &[u8], data: &[u8]) -> Vec<u8> {
    assert_eq!(&bytes[37..41], b"IDAT");
    let count = u32::from_be_bytes(bytes[33..37].try_into().unwrap()) as usize;
    let mut output = bytes[..33].to_vec();
    chunk(&mut output, b"IDAT", data);
    output.extend_from_slice(&bytes[45 + count..]);
    output
}
// Independent former full-image route, kept only as a small exact parity oracle.
fn legacy(bytes: &[u8]) -> Raster {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::with_format(Cursor::new(bytes), image::ImageFormat::Png);
    reader.limits(crate::image_import::limits());
    let mut decoder = reader.into_decoder().unwrap();
    let depth = if decoder.color_type().bits_per_pixel()
        / u16::from(decoder.color_type().channel_count())
        > 8
    {
        16
    } else {
        8
    };
    let profile = decoder.icc_profile().unwrap();
    let orientation = decoder.orientation().unwrap();
    let mut image = image::DynamicImage::from_decoder(decoder).unwrap();
    image.apply_orientation(orientation);
    let (width, height) = (image.width(), image.height());
    if depth == 16 {
        let mut raw = image.into_rgba16().into_raw();
        if let Some(profile) = profile.filter(|profile| !color_profile::is_srgb_profile(profile)) {
            color_profile::convert16(&profile, &mut raw).unwrap();
        }
        Raster::from_rgba16(width, height, &raw).unwrap()
    } else {
        let mut raw = image.into_rgba8().into_raw();
        if let Some(profile) = profile.filter(|profile| !color_profile::is_srgb_profile(profile)) {
            color_profile::convert8(&profile, &mut raw).unwrap();
        }
        Raster::from_rgba(width, height, &raw).unwrap()
    }
}
fn linear_profile() -> Vec<u8> {
    fn fixed(value: f64) -> [u8; 4] {
        ((value * 65536.).round() as i32).to_be_bytes()
    }
    let mut entries = Vec::new();
    for (name, xyz) in [
        (*b"rXYZ", [0.4360747, 0.2225045, 0.0139322]),
        (*b"gXYZ", [0.3850649, 0.7168786, 0.0971045]),
        (*b"bXYZ", [0.1430804, 0.0606169, 0.7141733]),
        (*b"wtpt", [0.9642, 1., 0.8249]),
    ] {
        let mut data = b"XYZ \0\0\0\0".to_vec();
        for value in xyz {
            data.extend(fixed(value));
        }
        entries.push((name, data));
    }
    for name in [*b"rTRC", *b"gTRC", *b"bTRC"] {
        entries.push((name, b"curv\0\0\0\0\0\0\0\0".to_vec()));
    }
    let mut bytes = vec![0; 132 + entries.len() * 12];
    bytes[8..12].copy_from_slice(&0x04200000u32.to_be_bytes());
    bytes[12..24].copy_from_slice(b"mntrRGB XYZ ");
    for (index, value) in [2026u16, 10, 9, 0, 0, 0].iter().enumerate() {
        bytes[24 + index * 2..26 + index * 2].copy_from_slice(&value.to_be_bytes());
    }
    bytes[36..40].copy_from_slice(b"acsp");
    for (index, value) in [0.9642, 1., 0.8249].iter().enumerate() {
        bytes[68 + index * 4..72 + index * 4].copy_from_slice(&fixed(*value));
    }
    bytes[128..132].copy_from_slice(&(entries.len() as u32).to_be_bytes());
    for (index, (name, data)) in entries.iter().enumerate() {
        let at = 132 + index * 12;
        bytes[at..at + 4].copy_from_slice(name);
        let offset = bytes.len() as u32;
        bytes[at + 4..at + 8].copy_from_slice(&offset.to_be_bytes());
        bytes[at + 8..at + 12].copy_from_slice(&(data.len() as u32).to_be_bytes());
        bytes.extend(data);
    }
    let length = bytes.len() as u32;
    bytes[..4].copy_from_slice(&length.to_be_bytes());
    bytes
}

#[test]
fn native_rgb_rgba_rows_match_original_words_and_full_decoder_across_tile_boundaries() {
    for depth in [8, 16] {
        for rgb in [false, true] {
            for (width, height) in [(259, 257), (8192, 3)] {
                let expected = words(width, height, depth, rgb);
                let bytes = png(width, height, depth, rgb, &expected, false);
                let records = Rc::new(RefCell::new(Vec::new()));
                let seen = records.clone();
                let _watch = watching(move |row, input, native, _| {
                    seen.borrow_mut().push((row, input, native));
                });
                let actual = Encoded::png(bytes.clone())
                    .unwrap()
                    .decode(&json!({}), BUDGET)
                    .unwrap();
                assert_eq!(
                    (actual.width, actual.height, actual.depth),
                    (width, height, depth)
                );
                assert!(
                    actual.rgba16() == expected,
                    "Every native sample, including hidden RGB, is retained"
                );
                assert!(
                    actual == legacy(&bytes),
                    "The former full-image decoder has exact native parity"
                );
                let rows = records.borrow();
                assert_eq!(
                    rows.iter().filter(|row| row.0.is_some()).count(),
                    height as usize
                );
                assert_eq!(rows.last().unwrap().0, None);
                for (row, input, native) in rows.iter().filter(|row| row.0.is_some()) {
                    assert!((1..=height).contains(&row.unwrap()));
                    assert_eq!(
                        *input,
                        width as usize * if rgb { 3 } else { 4 } * usize::from(depth / 8)
                    );
                    assert_eq!(*native, width as usize * 4 * usize::from(depth / 8));
                }
            }
        }
    }
}

#[test]
fn sparse_native_rows_skip_empty_tiles_without_losing_hidden_rgb_or_rgb_transparency_keys() {
    for depth in [8, 16] {
        let (width, height) = (513, 514);
        let mut expected = vec![0; width as usize * height as usize * 4];
        for (x, y) in [(0, 0), (256, 256), (512, 513)] {
            let at = ((y * width + x) * 4) as usize;
            expected[at..at + 4].copy_from_slice(&if depth == 16 {
                [12347, 23459, 34571, 0]
            } else {
                [41 * 257, 83 * 257, 127 * 257, 0]
            });
        }
        let bytes = png(width, height, depth, false, &expected, false);
        let actual = Encoded::png(bytes.clone())
            .unwrap()
            .decode(&json!({}), BUDGET)
            .unwrap();
        assert!(actual.rgba16() == expected);
        assert_eq!(
            if depth == 16 {
                actual.samples16.len()
            } else {
                actual.tiles.len()
            },
            3
        );
        assert!(actual == legacy(&bytes));
        let expected = words(17, 9, depth, true);
        let bytes = png(17, 9, depth, true, &expected, false);
        let key = expected[..3]
            .iter()
            .flat_map(|value| (if depth == 16 { *value } else { *value / 257 }).to_be_bytes())
            .collect::<Vec<_>>();
        let bytes = with_chunk(&bytes, b"tRNS", &key);
        let actual = Encoded::png(bytes.clone())
            .unwrap()
            .decode(&json!({}), BUDGET)
            .unwrap();
        assert_eq!(actual.get16(0, 0)[3], 0);
        assert_eq!(actual.get16(0, 0)[..3], expected[..3]);
        assert!(actual == legacy(&bytes));
    }
}

#[test]
fn supported_profile_rows_match_whole_image_conversion_and_keep_native_alpha() {
    for depth in [8, 16] {
        for profile in [color_profile::srgb_profile().to_vec(), linear_profile()] {
            let original = words(259, 17, depth, false);
            let bytes = png(259, 17, depth, false, &original, false);
            let payload = [b"native\0\0".as_slice(), zipped(&profile).as_slice()].concat();
            let bytes = with_chunk(&bytes, b"iCCP", &payload);
            let actual = Encoded::png(bytes.clone())
                .unwrap()
                .decode(&json!({}), BUDGET)
                .unwrap();
            assert!(
                actual == legacy(&bytes),
                "Row conversion must exactly match the former native-depth conversion"
            );
            let samples = actual.rgba16();
            for (input, output) in original.chunks_exact(4).zip(samples.chunks_exact(4)) {
                assert_eq!(input[3], output[3]);
            }
            if !color_profile::is_srgb_profile(&profile) {
                assert_ne!(samples[0], original[0]);
            }
            if depth == 16 {
                assert!(samples.iter().any(|value| value % 257 != 0));
            }
        }
    }
}

#[test]
fn other_png_layouts_and_orientations_use_the_existing_path_without_entering_row_decode() {
    let calls = Rc::new(RefCell::new(0));
    let seen = calls.clone();
    let _watch = watching(move |_, _, _, _| *seen.borrow_mut() += 1);
    for depth in [8, 16] {
        let expected = words(9, 7, depth, false);
        let interlaced = png(9, 7, depth, false, &expected, true);
        let actual = Encoded::png(interlaced.clone())
            .unwrap()
            .decode(&json!({}), BUDGET)
            .unwrap();
        assert!(actual.rgba16() == expected && actual == legacy(&interlaced));
        let plain = png(9, 7, depth, false, &expected, false);
        for orientation in 2..=8u16 {
            let mut exif = b"II\x2a\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0".to_vec();
            exif.extend_from_slice(&orientation.to_le_bytes());
            exif.extend_from_slice(&[0; 6]);
            let bytes = with_chunk(&plain, b"eXIf", &exif);
            let actual = Encoded::png(bytes.clone())
                .unwrap()
                .decode(&json!({}), BUDGET)
                .unwrap();
            assert!(actual == legacy(&bytes));
            assert_eq!(
                (actual.width, actual.height),
                if orientation >= 5 { (7, 9) } else { (9, 7) }
            );
        }
    }
    let mut palette = Vec::new();
    {
        let mut encoder = ::png::Encoder::new(&mut palette, 3, 1);
        encoder.set_color(::png::ColorType::Indexed);
        encoder.set_depth(::png::BitDepth::Eight);
        encoder.set_palette(vec![255, 0, 0, 0, 255, 0]);
        encoder.set_trns(vec![0, 127]);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0, 1, 0])
            .unwrap();
    }
    let actual = Encoded::png(palette.clone())
        .unwrap()
        .decode(&json!({}), BUDGET)
        .unwrap();
    assert!(actual == legacy(&palette));
    assert_eq!(actual.get(1, 0), [0, 255, 0, 127]);
    let mut gray = Vec::new();
    {
        let mut encoder = ::png::Encoder::new(&mut gray, 2, 1);
        encoder.set_color(::png::ColorType::Grayscale);
        encoder.set_depth(::png::BitDepth::Sixteen);
        encoder
            .write_header()
            .unwrap()
            .write_image_data(&[0x30, 0x3b, 0x7f, 0x81])
            .unwrap();
    }
    let actual = Encoded::png(gray.clone())
        .unwrap()
        .decode(&json!({}), BUDGET)
        .unwrap();
    assert!(actual == legacy(&gray));
    assert_eq!(actual.get16(0, 0), [12347, 12347, 12347, 65535]);
    for depth in [8, 16] {
        let mut animated = Vec::new();
        {
            let mut encoder = ::png::Encoder::new(&mut animated, 2, 1);
            encoder.set_color(::png::ColorType::Rgba);
            encoder.set_depth(if depth == 16 {
                ::png::BitDepth::Sixteen
            } else {
                ::png::BitDepth::Eight
            });
            encoder.set_animated(2, 0).unwrap();
            let mut writer = encoder.write_header().unwrap();
            for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
                let data = color
                    .into_iter()
                    .flat_map(|value| {
                        if depth == 16 {
                            vec![value, value]
                        } else {
                            vec![value]
                        }
                    })
                    .collect::<Vec<u8>>()
                    .repeat(2);
                writer.write_image_data(&data).unwrap();
            }
        }
        let encoded = Encoded::png(animated);
        if depth == 16 {
            assert!(encoded.err().unwrap().contains("16-bit APNG"));
        } else {
            let encoded = encoded.unwrap();
            assert!(encoded.decode(&json!({}), BUDGET).is_err());
            let image = encoded.decode(&json!({"frame":1}), BUDGET).unwrap();
            assert_eq!(image.get(1, 0), [0, 0, 255, 255]);
        }
    }
    assert_eq!(
        *calls.borrow(),
        0,
        "Unsupported row layouts are selected before decoding, never error fallbacks"
    );
}

fn state(shared: &crate::server::Shared) -> Value {
    let engine = shared.lock().unwrap();
    json!({"doc":engine.doc,"undo":engine.undo.len(),"redo":engine.redo.len(),"saved":engine.saved_revision})
}
struct File(std::path::PathBuf);
impl File {
    fn new(bytes: &[u8]) -> Self {
        use std::io::Write;
        let path = std::env::temp_dir().join(format!("pb-native-png-{}.png", crate::engine::id()));
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn row_and_final_cancellation_stale_human_work_and_file_changes_never_replace_the_source() {
    let canceled = Control::default();
    canceled.cancel();
    let mut input = Checked {
        input: Cursor::new(b"native".as_slice()),
        control: &canceled,
    };
    assert_eq!(
        input.read(&mut [0]).unwrap_err().kind(),
        std::io::ErrorKind::Other
    );
    assert_eq!(
        input.seek(SeekFrom::Start(0)).unwrap_err().kind(),
        std::io::ErrorKind::Other
    );
    for depth in [8, 16] {
        for action in ["cancel_row", "cancel_final", "source", "file"] {
            let original = words(259, 5, depth, false);
            let bytes = png(259, 5, depth, false, &original, false);
            let file = File::new(&bytes);
            let shared = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
            {
                let mut engine = shared.lock().unwrap();
                engine.doc = crate::engine::Document::new_depth(7, 5, depth).unwrap();
                engine.doc.layers[0]
                    .pixels
                    .set16(1, 1, [12347, 23459, 34571, 45679]);
                let target = engine.doc.layers[0].id.clone();
                engine
                    .edit(
                        "human",
                        &[json!({"op":"layer.update","layer":target,"name":"Kept human work"})],
                        Some(0),
                        None,
                        "Human work",
                    )
                    .unwrap();
            }
            let before = state(&shared);
            let expected = Rc::new(RefCell::new(before.clone()));
            let after_human = expected.clone();
            let source = shared.clone();
            let path = file.0.clone();
            let control = Control::default();
            let cancel = control.clone();
            let _watch = watching(move |row, _, _, _| {
                if (action == "cancel_row" && row == Some(2))
                    || (action == "cancel_final" && row.is_none())
                {
                    cancel.cancel();
                }
                if row != Some(2) {
                    return;
                }
                if action == "source" {
                    {
                        let mut engine = source
                            .try_lock()
                            .expect("Opening must not hold the engine lock while decoding rows");
                        let target = engine.doc.layers[0].id.clone();
                        engine
                            .edit(
                                "human",
                                &[json!({"op":"layer.update","layer":target,"opacity":0.75})],
                                Some(1),
                                None,
                                "Later human work",
                            )
                            .unwrap();
                    }
                    *after_human.borrow_mut() = state(&source);
                } else if action == "file" {
                    std::fs::write(&path, b"Changed owned synthetic file").unwrap();
                }
            });
            let error =
                crate::server::open_options(&shared, &file.0, &control, &json!({})).unwrap_err();
            assert!(if action.starts_with("cancel") {
                error.contains("Opening canceled")
            } else if action == "source" {
                error.contains("Canvas changed while opening")
            } else {
                error.contains("Image file changed")
            });
            assert!(
                state(&shared) == *expected.borrow(),
                "No native document, source/history or later human edit is discarded"
            );
            assert!(shared.lock().unwrap().loading.is_none());
            if action != "file" {
                assert_eq!(std::fs::read(&file.0).unwrap(), bytes);
            }
            drop(_watch);
            if action == "file" {
                std::fs::write(&file.0, &bytes).unwrap();
            }
            let other = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
            crate::server::open(&other, &file.0).unwrap();
            assert!(
                other.lock().unwrap().doc.layers[0].pixels.rgba16() == original,
                "Failed opening releases ownership and a fresh complete request can succeed"
            );
        }
    }
}

#[test]
fn corrupt_or_truncated_rows_and_final_png_chunks_refuse_without_fallback_or_source_mutation() {
    for depth in [8, 16] {
        let expected = words(259, 5, depth, false);
        let bytes = png(259, 5, depth, false, &expected, false);
        let raw = scanlines(259, 5, depth, false, &expected, false);
        let short = replace_idat(
            &bytes,
            &zipped(&raw[..raw.len() - (259 * 4 * usize::from(depth / 8) + 1)]),
        );
        let partial = replace_idat(&bytes, &zipped(&raw[..raw.len() - 1]));
        let mut checksum = zipped(&raw);
        *checksum.last_mut().unwrap() ^= 1;
        let adler = replace_idat(&bytes, &checksum);
        let mut footer = bytes.clone();
        *footer.last_mut().unwrap() ^= 1;
        let mut idat_crc = bytes.clone();
        let count = u32::from_be_bytes(bytes[33..37].try_into().unwrap()) as usize;
        idat_crc[41 + count] ^= 1;
        let truncated = bytes[..bytes.len() - 1].to_vec();
        for broken in [short, partial, adler, footer, idat_crc, truncated] {
            let file = File::new(&broken);
            let shared = Arc::new(std::sync::Mutex::new(crate::engine::Engine::new()));
            let before = state(&shared);
            assert!(crate::server::open(&shared, &file.0).is_err());
            assert!(state(&shared) == before);
            assert_eq!(std::fs::read(&file.0).unwrap(), broken);
        }
        let encoded = Encoded::png(bytes).unwrap();
        let calls = Rc::new(RefCell::new(0));
        let seen = calls.clone();
        let _watch = watching(move |_, _, _, _| *seen.borrow_mut() += 1);
        assert!(encoded.decode(&json!({}), 1).is_err());
        assert_eq!(
            *calls.borrow(),
            0,
            "The existing decoded-coexistence quota rejects before native row allocation"
        );
    }
}
