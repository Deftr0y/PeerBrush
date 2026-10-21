use super::*;
use serde_json::{json, Value};
use std::cell::Cell;

fn compressed(bytes: &[u8]) -> Vec<u8> {
    let mut zip = ZlibEncoder::new(Vec::new(), Compression::fast());
    zip.write_all(bytes).unwrap();
    zip.finish().unwrap()
}

// The original 1 MiB-chunk capped reader is a fixture oracle, never a load fallback.
fn legacy(bytes: &[u8], limit: usize) -> Option<Embedded> {
    let mut json = Vec::new();
    let mut zip = ZlibDecoder::new(bytes).take(limit as u64 + 1);
    let mut chunk = vec![0; 1024 * 1024];
    loop {
        let n = zip.read(&mut chunk).ok()?;
        if n == 0 {
            break;
        }
        json.extend_from_slice(&chunk[..n]);
    }
    (json.len() <= limit)
        .then(|| serde_json::from_slice(&json).ok())
        .flatten()
}

fn small_json() -> Vec<u8> {
    serde_json::to_vec(&Embedded {
        format: 1,
        standard_hash: 123,
        document: Document::new(2, 1).unwrap(),
    })
    .unwrap()
}

#[test]
fn bounded_source_requires_json_end_real_zlib_eof_checksum_and_exact_decoded_budget() {
    let control = crate::loading::Control::default();
    let json = small_json();
    let zipped = compressed(&json);
    for limit in [
        json.len(),
        json.len() + 1,
        128 * 1024 * 1024,
        512 * 1024 * 1024,
    ] {
        let expected = legacy(&zipped, limit).unwrap();
        let actual = embedded_source(ZlibDecoder::new(zipped.as_slice()), limit, &control).unwrap();
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
    }
    for limit in [0, json.len() - 1] {
        assert!(legacy(&zipped, limit).is_none());
        assert!(embedded_source(ZlibDecoder::new(zipped.as_slice()), limit, &control).is_err());
    }
    let mut whitespace = json.clone();
    whitespace.extend(std::iter::repeat_n(b' ', 64 * 1024 + 3));
    let padded = compressed(&whitespace);
    assert!(embedded_source(
        ZlibDecoder::new(padded.as_slice()),
        whitespace.len(),
        &control
    )
    .is_ok());
    assert!(embedded_source(
        ZlibDecoder::new(padded.as_slice()),
        whitespace.len() - 1,
        &control
    )
    .is_err());
    // The payload is complete JSON, but decompression still has to validate the footer.
    for cut in 1..=4 {
        let truncated = &padded[..padded.len() - cut];
        assert!(legacy(truncated, whitespace.len()).is_none());
        assert!(embedded_source(ZlibDecoder::new(truncated), whitespace.len(), &control).is_err());
    }
    let mut bad_checksum = padded.clone();
    *bad_checksum.last_mut().unwrap() ^= 1;
    assert!(legacy(&bad_checksum, whitespace.len()).is_none());
    assert!(embedded_source(
        ZlibDecoder::new(bad_checksum.as_slice()),
        whitespace.len(),
        &control
    )
    .is_err());
    for invalid in [
        b"{".as_slice(),
        b"null".as_slice(),
        b"[]".as_slice(),
        b"\xff".as_slice(),
    ] {
        let bytes = compressed(invalid);
        assert!(legacy(&bytes, 4096).is_none());
        assert!(embedded_source(ZlibDecoder::new(bytes.as_slice()), 4096, &control).is_err());
    }
    for suffix in [b" {}".as_slice(), b" null".as_slice(), b"x".as_slice()] {
        let bytes = compressed(&[json.as_slice(), suffix].concat());
        assert!(legacy(&bytes, json.len() + suffix.len()).is_none());
        assert!(embedded_source(
            ZlibDecoder::new(bytes.as_slice()),
            json.len() + suffix.len(),
            &control
        )
        .is_err());
    }
    // Preserve the existing single-zlib-stream contract: encoded bytes after
    // StreamEnd are not extra decoded JSON, and were ignored by the old reader.
    let trailing = [zipped.as_slice(), b"encoded trailing bytes"].concat();
    let expected = legacy(&trailing, json.len()).unwrap();
    let actual =
        embedded_source(ZlibDecoder::new(trailing.as_slice()), json.len(), &control).unwrap();
    assert_eq!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(expected).unwrap()
    );
}

#[test]
fn buffered_source_checks_cancellation_on_refill_without_per_byte_native_reads() {
    struct Observed<'a, R> {
        inner: R,
        calls: &'a Cell<usize>,
        maximum: &'a Cell<usize>,
        cancel: Option<&'a crate::loading::Control>,
    }
    impl<R: Read> Read for Observed<'_, R> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.calls.set(self.calls.get() + 1);
            self.maximum.set(self.maximum.get().max(bytes.len()));
            let n = self.inner.read(bytes)?;
            if n != 0 {
                if let Some(control) = self.cancel {
                    control.cancel();
                }
            }
            Ok(n)
        }
    }
    let mut json = small_json();
    json.extend(std::iter::repeat_n(b' ', 2 * 64 * 1024));
    let compressed = compressed(&json);
    for cancel in [false, true] {
        let control = crate::loading::Control::default();
        let calls = Cell::new(0);
        let maximum = Cell::new(0);
        let input = Observed {
            inner: ZlibDecoder::new(compressed.as_slice()),
            calls: &calls,
            maximum: &maximum,
            cancel: cancel.then_some(&control),
        };
        let result = embedded_source(input, json.len(), &control);
        assert_eq!(result.is_err(), cancel);
        assert_eq!(maximum.get(), 64 * 1024);
        if cancel {
            assert_eq!(
                result.err().unwrap().io_error_kind(),
                Some(std::io::ErrorKind::Other)
            );
            assert_eq!(
                calls.get(),
                1,
                "The next refill refuses before more decompression"
            );
            assert!(control.check().unwrap_err().contains("Opening canceled"));
        } else {
            assert!(
                (3..=5).contains(&calls.get()),
                "Not one native read per JSON byte"
            );
        }
    }
    let control = crate::loading::Control::default();
    control.cancel();
    let calls = Cell::new(0);
    let maximum = Cell::new(0);
    let input = Observed {
        inner: ZlibDecoder::new(compressed.as_slice()),
        calls: &calls,
        maximum: &maximum,
        cancel: None,
    };
    assert_eq!(
        embedded_source(input, json.len(), &control)
            .err()
            .unwrap()
            .io_error_kind(),
        Some(std::io::ErrorKind::Other)
    );
    assert_eq!(calls.get(), 0);
}

#[test]
fn bounded_slice_source_checks_final_eof_cancel_and_never_retries_read_errors() {
    struct FinalCancel<'a> {
        bytes: &'a [u8],
        control: &'a crate::loading::Control,
    }
    impl Read for FinalCancel<'_> {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            let n = self.bytes.read(output)?;
            if n == 0 {
                self.control.cancel();
            }
            Ok(n)
        }
    }
    let control = crate::loading::Control::default();
    // Invalid JSON makes cancellation-before-parse distinguishable from a
    // syntax error, without relying on timing or a global observation hook.
    let result = embedded_source(
        FinalCancel {
            bytes: b"not JSON",
            control: &control,
        },
        64,
        &control,
    );
    assert_eq!(
        result.err().unwrap().io_error_kind(),
        Some(std::io::ErrorKind::Other)
    );
    assert!(control.check().unwrap_err().contains("Opening canceled"));

    struct Interrupted<'a>(&'a Cell<usize>);
    impl Read for Interrupted<'_> {
        fn read(&mut self, _output: &mut [u8]) -> std::io::Result<usize> {
            self.0.set(self.0.get() + 1);
            Err(std::io::ErrorKind::Interrupted.into())
        }
    }
    let control = crate::loading::Control::default();
    let calls = Cell::new(0);
    let result = embedded_source(Interrupted(&calls), 64, &control);
    assert_eq!(
        result.err().unwrap().io_error_kind(),
        Some(std::io::ErrorKind::Interrupted)
    );
    assert_eq!(
        calls.get(),
        1,
        "The capped read loop does not retry an interrupted reader"
    );
}

fn fixture(depth: u16) -> crate::engine::Engine {
    let mut engine = crate::engine::Engine::new();
    engine.doc = Document::new_depth(32, 24, depth).unwrap();
    let pixels = &mut engine.doc.layers[0].pixels;
    pixels.set16(0, 0, [12347, 23459, 34571, 0]);
    pixels.set16(1, 0, [50001, 30003, 10001, 32769]);
    pixels.set16(31, 23, [60001, 40003, 20009, 65535]);
    let mut mask = Raster::new_depth(32, 24, depth);
    mask.set16(1, 0, [12345, 12345, 12345, 65535]);
    engine.doc.layers[0].mask = Some(Mask {
        linked: false,
        enabled: false,
        cache_key: id(),
        steps: vec![MaskStep {
            id: id(),
            kind: "paint".into(),
            enabled: true,
            weight: 1.,
            value: 0.,
            pixels: mask,
            settings: Value::Null,
        }],
    });
    engine
        .edit(
            "human",
            &[json!({"op":"source.add","x":7,"y":3,"source":{
        "width":16,"height":12,"matrix":[1,0,0,1,0,0],"content":{
            "kind":"shape","shape":"rectangle","bounds":[2.125,2.25,13.875,9.75],
            "points":[[2,2],[14,2],[14,10],[2,10]],"closed":true,
            "fill":[12345,23457,34569,50001],"stroke":[50001,30003,10001,65535],
            "stroke_width":1.23456789012345}}})],
            None,
            None,
            "Editable native shape",
        )
        .unwrap();
    engine
}

fn payload(bytes: &[u8]) -> (&[u8], usize, usize, usize, usize) {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let resources = &bytes[at + 4..at + 4 + size];
    assert_eq!(&resources[..6], b"8BIM\x0f\xa0");
    let length = 6 + (usize::from(resources[6]) + 1).div_ceil(2) * 2;
    let n = u32::from_be_bytes(resources[length..length + 4].try_into().unwrap()) as usize;
    let data = &resources[length + 4..length + 4 + n];
    assert_eq!(&data[..4], b"PBR1");
    (&data[4..], at, size, length, n)
}

fn replaced(bytes: &[u8], compressed: &[u8]) -> Vec<u8> {
    let (_, at, size, length, old) = payload(bytes);
    let resources = &bytes[at + 4..at + 4 + size];
    let mut replacement = resources[..length].to_vec();
    let n = compressed.len() + 4;
    replacement.extend((n as u32).to_be_bytes());
    replacement.extend(b"PBR1");
    replacement.extend(compressed);
    if n % 2 != 0 {
        replacement.push(0);
    }
    replacement.extend(&resources[length + 4 + old.div_ceil(2) * 2..]);
    [
        bytes[..at].to_vec(),
        (replacement.len() as u32).to_be_bytes().to_vec(),
        replacement,
        bytes[at + 4 + size..].to_vec(),
    ]
    .concat()
}

fn current_standard_composite(bytes: &[u8], layout: Layout, doc: &Document) {
    let mut cursor = Cursor::new(bytes);
    cursor.bytes(26).unwrap();
    cursor.block().unwrap();
    cursor.block().unwrap();
    layout.block(&mut cursor).unwrap();
    assert_eq!(cursor.u16().unwrap(), 0, "Raw standard merged channels");
    let image = image::load_from_memory(&doc.export_png().unwrap()).unwrap();
    let rgba = image.to_rgba16();
    let divisor = if doc.bit_depth == 16 { 1 } else { 257 };
    let maximum = if doc.bit_depth == 16 {
        65535u64
    } else {
        255u64
    };
    for channel in 0..4 {
        for pixel in rgba.pixels() {
            let alpha = u64::from(pixel[3] / divisor);
            let sample = u64::from(pixel[channel] / divisor);
            let expected = if channel == 3 {
                alpha
            } else {
                (sample * alpha + maximum * (maximum - alpha) + maximum / 2) / maximum
            };
            let actual = if doc.bit_depth == 16 {
                u64::from(cursor.u16().unwrap())
            } else {
                u64::from(cursor.u8().unwrap())
            };
            assert_eq!(actual, expected);
        }
    }
    assert_eq!(cursor.pos, bytes.len());
}

#[test]
fn bounded_editable_native_sources_match_legacy_reader_masks_png_history_and_standard_composite() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let before = serde_json::to_value(&engine.doc).unwrap();
        let png = engine.doc.export_png().unwrap();
        assert_eq!(engine.undo.len(), 1);
        assert_eq!(engine.redo.len(), 0);
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&engine.doc, layout).unwrap();
            let (zipped, _, _, _, _) = payload(&bytes);
            let expected = legacy(
                zipped,
                if depth == 16 {
                    512 * 1024 * 1024
                } else {
                    128 * 1024 * 1024
                },
            )
            .unwrap();
            let actual = embedded_source(
                ZlibDecoder::new(zipped),
                if depth == 16 {
                    512 * 1024 * 1024
                } else {
                    128 * 1024 * 1024
                },
                &crate::loading::Control::default(),
            )
            .unwrap();
            assert_eq!(
                serde_json::to_value(&actual).unwrap(),
                serde_json::to_value(&expected).unwrap()
            );
            let reopened = decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(
                serde_json::to_value(&reopened).unwrap(),
                serde_json::to_value(&expected.document).unwrap()
            );
            assert_eq!(reopened.export_png().unwrap(), png);
            let image = image::load_from_memory(&reopened.export_png().unwrap()).unwrap();
            assert_eq!(
                image.color(),
                if depth == 16 {
                    image::ColorType::Rgba16
                } else {
                    image::ColorType::Rgba8
                }
            );
            assert_eq!(reopened.layers.len(), engine.doc.layers.len());
            for (loaded, source) in reopened.layers.iter().zip(&engine.doc.layers) {
                assert_eq!(
                    (&loaded.id, loaded.x, loaded.y, &loaded.source),
                    (&source.id, source.x, source.y, &source.source)
                );
                assert_eq!(loaded.pixels, source.pixels);
                assert_eq!(
                    serde_json::to_value(&loaded.mask).unwrap(),
                    serde_json::to_value(&source.mask).unwrap()
                );
            }
            // No serializer or standard plane/composite byte changes are needed.
            assert_eq!(encode_layout(&expected.document, layout).unwrap(), bytes);
            current_standard_composite(&bytes, layout, &engine.doc);
            let mut invalid = serde_json::to_value(&expected).unwrap();
            invalid["format"] = json!(u32::MAX);
            let protected = decode(&replaced(
                &bytes,
                &compressed(&serde_json::to_vec(&invalid).unwrap()),
            ))
            .unwrap();
            assert!(protected.read_only);
            assert!(protected.layers.iter().all(|layer| layer.source.is_none()));
            assert_eq!(protected.bit_depth, depth);
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!(engine.undo.len(), 1);
            assert!(engine.redo.is_empty());
        }
        engine.undo("human").unwrap();
        assert!(engine.doc.layers.iter().all(|layer| layer.source.is_none()));
        engine.redo("human").unwrap();
        let mut after_redo = before.clone();
        after_redo["revision"] = json!(before["revision"].as_u64().unwrap() + 2);
        assert_eq!(serde_json::to_value(&engine.doc).unwrap(), after_redo);
        assert_eq!(engine.doc.export_png().unwrap(), png);
        assert_eq!(engine.undo.len(), 1);
        assert!(engine.redo.is_empty());
    }
}

#[test]
fn invalid_streams_or_definition_gates_preserve_standard_protected_fallback_and_cancel() {
    for depth in [8, 16] {
        let engine = fixture(depth);
        let before = serde_json::to_value(&engine.doc).unwrap();
        for layout in [Layout::Psd, Layout::Psb] {
            let bytes = encode_layout(&engine.doc, layout).unwrap();
            let (zipped, _, _, _, _) = payload(&bytes);
            let embedded = legacy(zipped, 512 * 1024 * 1024).unwrap();
            let json = serde_json::to_value(&embedded).unwrap();
            let mut future = json.clone();
            future["format"] = json!(u32::MAX);
            let reference = decode(&replaced(
                &bytes,
                &compressed(&serde_json::to_vec(&future).unwrap()),
            ))
            .unwrap();
            assert!(reference.read_only);
            let expected = reference.export_png().unwrap();
            let mut bad_checksum = zipped.to_vec();
            *bad_checksum.last_mut().unwrap() ^= 1;
            let mut failures = vec![
                bad_checksum,
                zipped[..zipped.len() - 1].to_vec(),
                compressed(b"{"),
                compressed(&[serde_json::to_vec(&json).unwrap(), b" {}".to_vec()].concat()),
            ];
            for (key, value) in [
                ("format", json!(1)),
                ("standard_hash", json!(embedded.standard_hash ^ 1)),
            ] {
                let mut altered = json.clone();
                altered[key] = value;
                failures.push(compressed(&serde_json::to_vec(&altered).unwrap()));
            }
            for (key, value) in [
                ("width", json!(31)),
                ("height", json!(23)),
                ("bit_depth", json!(if depth == 8 { 16 } else { 8 })),
            ] {
                let mut altered = json.clone();
                altered["document"][key] = value;
                failures.push(compressed(&serde_json::to_vec(&altered).unwrap()));
            }
            for zipped in failures {
                let loaded = decode(&replaced(&bytes, &zipped)).unwrap();
                assert!(loaded.read_only);
                assert_eq!(loaded.bit_depth, depth);
                assert!(loaded.layers.iter().all(|layer| layer.source.is_none()));
                assert!(loaded
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("definitions were changed")));
                assert_eq!(loaded.export_png().unwrap(), expected);
            }
            let control = crate::loading::Control::default();
            control.cancel();
            assert!(decode_reader(&mut std::io::Cursor::new(&bytes), &control)
                .err()
                .unwrap()
                .contains("Opening canceled"));
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!(engine.undo.len(), 1);
            assert!(engine.redo.is_empty());
        }
    }
}

#[test]
#[ignore = "opt-in same-source capped-reader component timing; not a latency SLA"]
fn supplementary_restore_component_timings_keep_exact_dense_sparse_native_documents() {
    for depth in [8, 16] {
        for dense in [false, true] {
            let mut document = Document::new_depth(256, 192, depth).unwrap();
            for y in 0..192 {
                for x in 0..256 {
                    if dense || (x == 0 && y == 0) || (x == 255 && y == 191) {
                        document.layers[0].pixels.set16(
                            x,
                            y,
                            [
                                (12347 + x * 137 + y * 71) as u16,
                                (23459 + x * 47 + y * 113) as u16,
                                (34571 + x * 19 + y * 83) as u16,
                                if (x + y) % 7 == 0 { 0 } else { 32769 },
                            ],
                        );
                    }
                }
            }
            let expected = Embedded {
                format: if depth == 16 { 5 } else { 1 },
                standard_hash: 123,
                document,
            };
            let json = serde_json::to_vec(&expected).unwrap();
            let zipped = compressed(&json);
            let limit = if depth == 16 {
                512 * 1024 * 1024
            } else {
                128 * 1024 * 1024
            };
            let control = crate::loading::Control::default();
            // Both paths retain a capped full JSON buffer and use from_slice.
            // Original: 1 MiB transfer + take; current: guarded 64 KiB transfer.
            // This fixed old-first component sample does not isolate parsing,
            // establish a cold-cache measurement or assert a latency threshold.
            let start = std::time::Instant::now();
            let old = legacy(&zipped, limit).unwrap();
            let old_ms = start.elapsed().as_secs_f64() * 1000.;
            let start = std::time::Instant::now();
            let new =
                embedded_source(ZlibDecoder::new(zipped.as_slice()), limit, &control).unwrap();
            let new_ms = start.elapsed().as_secs_f64() * 1000.;
            let expected = serde_json::to_value(expected).unwrap();
            assert_eq!(serde_json::to_value(old).unwrap(), expected);
            assert_eq!(serde_json::to_value(new).unwrap(), expected);
            println!("PB_PSD_SOURCE_RESTORE depth={depth} density={} decoded_bytes={} compressed_bytes={} old_ms={old_ms:.3} new_ms={new_ms:.3}",
                if dense { "dense" } else { "sparse" }, json.len(), zipped.len());
        }
    }
}

type OpenObserver = Box<dyn FnMut(&'static str, usize)>;
thread_local! {
    static OPEN_OBSERVER: std::cell::RefCell<Option<OpenObserver>> = const { std::cell::RefCell::new(None) };
}
pub(crate) fn observe_phase(phase: &'static str, bytes: usize) {
    let callback = OPEN_OBSERVER.with(|slot| slot.borrow_mut().take());
    if let Some(mut callback) = callback {
        callback(phase, bytes);
        OPEN_OBSERVER.with(|slot| *slot.borrow_mut() = Some(callback));
    }
}
struct WatchOpen;
impl WatchOpen {
    fn new(callback: impl FnMut(&'static str, usize) + 'static) -> Self {
        OPEN_OBSERVER.with(|slot| assert!(slot.borrow_mut().replace(Box::new(callback)).is_none()));
        Self
    }
}
impl Drop for WatchOpen {
    fn drop(&mut self) {
        OPEN_OBSERVER.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
const OPEN_PHASES: [&str; 14] = [
    "request_start",
    "decoder_start",
    "sections_read",
    "composite_decoded",
    "preview_ready",
    "json_start",
    "json_inflated",
    "json_parsed",
    "source_decoded",
    "source_accepted",
    "decoder_returned",
    "source_verified",
    "replaced",
    "request_return",
];

#[test]
fn open_phase_observer_is_scoped_and_removes_itself_around_callbacks() {
    use std::{cell::RefCell, rc::Rc};
    let phases = Rc::new(RefCell::new(Vec::new()));
    let seen = phases.clone();
    let watch = WatchOpen::new(move |phase, bytes| {
        seen.borrow_mut().push((phase, bytes));
        observe_phase("nested", 99);
    });
    observe_phase("json_start", 12);
    observe_phase("json_inflated", 34);
    drop(watch);
    observe_phase("after_drop", 56);
    assert_eq!(
        *phases.borrow(),
        vec![("json_start", 12), ("json_inflated", 34)]
    );
}

#[test]
#[ignore = "Opt-in actual 2048-square shared Open phases; run alone from a clean build"]
fn actual_dense_psd_open_phase_diagnostic() {
    use std::{
        cell::RefCell,
        fs,
        rc::Rc,
        sync::{Arc, Mutex},
        time::Instant,
    };
    let build: Value = serde_json::from_str(crate::build_info::json()).unwrap();
    assert_eq!(build["tree"], "clean");
    assert_eq!(build["format"], "PeerBrushBuildInfoV1");
    assert_eq!(build["product"], "PeerBrush");
    assert_eq!(build["commit"].as_str().unwrap().len(), 40);
    let parent = std::env::temp_dir().canonicalize().unwrap();
    let directory = parent.join(format!("pb-open-phases-{}", crate::engine::id()));
    fs::create_dir(&directory).unwrap();
    assert_eq!(
        directory.canonicalize().unwrap().parent(),
        Some(parent.as_path())
    );
    let mut records = Vec::new();
    for depth in [8, 16] {
        let source = directory.join(format!("native-{depth}.psd"));
        let mut document = Document::new_depth(2048, 2048, depth).unwrap();
        document.name = source.file_name().unwrap().to_string_lossy().into_owned();
        document.srgb_tagged = true;
        for y in 0..2048u16 {
            for x in 0..2048u16 {
                let alpha = if (u32::from(x) + 3 * u32::from(y)) % 17 == 0 {
                    0
                } else if (u32::from(x) + u32::from(y)) % 7 == 0 {
                    65535
                } else {
                    12001u16
                        .wrapping_add(x.wrapping_mul(31))
                        .wrapping_add(y.wrapping_mul(47))
                };
                document.layers[0].pixels.set16(
                    i32::from(x),
                    i32::from(y),
                    [
                        x.wrapping_mul(239)
                            .wrapping_add(y.wrapping_mul(911))
                            .wrapping_add(17),
                        x.wrapping_mul(13) ^ y.wrapping_mul(907) ^ 0x873b,
                        x.wrapping_add(y).wrapping_mul(613).wrapping_add(0x4ef3),
                        alpha,
                    ],
                );
            }
        }
        let pixels = &document.layers[0].pixels;
        assert_eq!(pixels.depth, depth);
        assert_eq!(pixels.get16(0, 0)[3], 0);
        assert_ne!(pixels.get16(0, 0)[..3], [0; 3]);
        assert!(pixels.get16(1, 1)[3] > 0 && pixels.get16(1, 1)[3] < 65535);
        assert_eq!(pixels.get16(7, 0)[3], 65535);
        if depth == 16 {
            assert!(pixels.get16(1, 1)[..3].iter().any(|value| value % 257 != 0));
        }
        let bytes = encode(&document).unwrap();
        assert!(bytes.len() <= 256 * 1024 * 1024);
        current_standard_composite(&bytes, Layout::Psd, &document);
        let expected = serde_json::to_value(&document).unwrap();
        let expected_png = document.export_png().unwrap();
        let image = image::load_from_memory(&expected_png).unwrap();
        assert_eq!(
            image.color(),
            if depth == 16 {
                image::ColorType::Rgba16
            } else {
                image::ColorType::Rgba8
            }
        );
        drop(image);
        {
            use std::io::Write;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&source)
                .unwrap()
                .write_all(&bytes)
                .unwrap();
        }
        let version = crate::file_safety::version(&source).unwrap();
        for repetition in 1..=3 {
            let shared = Arc::new(Mutex::new(crate::engine::Engine::new()));
            let (document_id, revision) = {
                let engine = shared.lock().unwrap();
                (engine.doc.id.clone(), engine.doc.revision)
            };
            let request = json!({"action":"open","document_id":document_id.clone(),"expected_revision":revision,"path":source,"actor":"human"});
            let phases = Rc::new(RefCell::new(Vec::new()));
            let seen = phases.clone();
            let watch = WatchOpen::new(move |phase, bytes| {
                seen.borrow_mut().push((phase, Instant::now(), bytes));
            });
            let started = Instant::now();
            phases.borrow_mut().push(("request_start", started, 0));
            crate::server::dispatch(&shared, "document", &request).unwrap();
            let finished = Instant::now();
            drop(watch);
            phases.borrow_mut().push(("request_return", finished, 0));
            let phases = phases.borrow();
            assert_eq!(
                phases.iter().map(|phase| phase.0).collect::<Vec<_>>(),
                OPEN_PHASES
            );
            let total_ms = finished.duration_since(started).as_secs_f64() * 1000.;
            assert!(total_ms.is_finite() && total_ms > 0.);
            let mut sum = 0.;
            let mut durations = serde_json::Map::new();
            for adjacent in phases.windows(2) {
                let elapsed = adjacent[1].1.duration_since(adjacent[0].1).as_secs_f64() * 1000.;
                assert!(elapsed.is_finite() && elapsed >= 0.);
                sum += elapsed;
                assert!(durations
                    .insert(adjacent[1].0.into(), json!(elapsed))
                    .is_none());
            }
            assert!((sum - total_ms).abs() <= 1e-6);
            let compressed = phases
                .iter()
                .find(|phase| phase.0 == "json_start")
                .unwrap()
                .2;
            let decoded = phases
                .iter()
                .find(|phase| phase.0 == "json_inflated")
                .unwrap()
                .2;
            assert!(
                compressed > 0
                    && decoded > 0
                    && decoded
                        <= if depth == 16 {
                            512 * 1024 * 1024
                        } else {
                            128 * 1024 * 1024
                        }
            );
            let opened_document;
            {
                let engine = shared.lock().unwrap();
                opened_document = engine.doc.id.clone();
                assert!(
                    !opened_document.is_empty()
                        && opened_document != document.id
                        && opened_document != document_id
                );
                // Engine::replace deliberately creates a fresh runtime document ID.
                let mut actual = serde_json::to_value(&engine.doc).unwrap();
                actual["id"] = expected["id"].clone();
                assert!(
                    actual == expected,
                    "All persisted native source definitions and stored words remain exact"
                );
                assert!(engine.doc.layers[0].pixels == document.layers[0].pixels);
                assert_eq!(engine.doc.layers.len(), 1);
                assert!(
                    engine.doc.layers[0].source.is_none() && engine.doc.layers[0].mask.is_none()
                );
                assert!(
                    engine.doc.warnings.is_empty()
                        && !engine.doc.read_only
                        && engine.doc.srgb_tagged
                );
                assert_eq!(engine.doc.bit_depth, depth);
                assert!(engine.undo.is_empty() && engine.redo.is_empty());
                assert_eq!(engine.saved_revision, engine.doc.revision);
                assert!(engine.loading.is_none() && engine.file_ownership.is_some());
                assert_eq!(engine.file_version, Some(version));
                assert_eq!(engine.path.as_deref(), Some(source.as_path()));
                assert_eq!(engine.doc.export_png().unwrap(), expected_png);
            }
            assert_eq!(crate::file_safety::version(&source).unwrap(), version);
            let other = Arc::new(Mutex::new(crate::engine::Engine::new()));
            let before = other.lock().unwrap().doc.id.clone();
            assert!(crate::server::open(&other, &source)
                .unwrap_err()
                .starts_with("File already open"));
            assert_eq!(other.lock().unwrap().doc.id, before);
            {
                let engine = shared.lock().unwrap();
                assert_eq!(engine.doc.id, opened_document);
                let mut actual = serde_json::to_value(&engine.doc).unwrap();
                actual["id"] = expected["id"].clone();
                assert!(actual == expected);
                assert!(engine.undo.is_empty() && engine.redo.is_empty());
            }
            let phase_count = durations.len();
            records.push(json!({"depth":depth,"repetition":repetition,"width":2048,"height":2048,
                "encoded_bytes":bytes.len(),"compressed_json_bytes":compressed,"decoded_json_bytes":decoded,
                "phase_ms":durations,"total_ms":total_ms,"phase_sum_ms":sum,"phase_count":phase_count,
                "native_source_and_hidden_rgb_exact":true,"native_png_exact":true,"current_standard_composite_exact":true,
                "source_file_and_ownership_preserved":true,"empty_history_preserved":true,"embedded_source_accepted":true}));
        }
        // Timed reads and all checks must leave the complete original file unchanged.
        assert_eq!(fs::read(&source).unwrap(), bytes);
    }
    assert_eq!(records.len(), 6);
    for depth in [8, 16] {
        assert_eq!(
            records
                .iter()
                .filter(|record| record["depth"] == depth)
                .count(),
            3
        );
    }
    // Only this newly created, checked synthetic directory is eligible for cleanup.
    let metadata = fs::symlink_metadata(&directory).unwrap();
    assert!(metadata.is_dir() && !metadata.file_type().is_symlink());
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        assert_eq!(metadata.file_attributes() & 0x400, 0);
    }
    assert_eq!(
        directory.canonicalize().unwrap().parent(),
        Some(parent.as_path())
    );
    fs::remove_dir_all(&directory).unwrap();
    assert!(!directory.exists());
    println!(
        "\n{}",
        json!({"format":"PeerBrushOpenPhaseDiagnosticV1","build":build,"native_window":false,
        "normal_application":false,"before_after_comparison":false,"cli_ipc_included":false,
        "workload":"actual guarded shared-server Open of one-layer dense native 2048-square PSD",
        "profile":if cfg!(debug_assertions) {"test-debug-assertions"} else {"test-release"},
        "note":"Observer/test-profile overhead included. Generation, correctness checks and encoding excluded. Cache, allocation and fixed depth order are uncontrolled. Final source intervals include lock acquisition and checks; these are not total lock-held times. No normal-main/input/memory/16k claim.",
        "records":records})
    );
}
