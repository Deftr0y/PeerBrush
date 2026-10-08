use flate2::{write::ZlibEncoder, Compression};
use peerbrush::{
    disk_cache::Store,
    engine::{Document, Engine},
    loading::Control,
    psd, server,
};
use std::{
    io::{Read, Seek, SeekFrom, Write},
    sync::{Arc, Mutex},
};
fn flat(depth: u16, kind: u16, w: u32, h: u32) -> (Vec<u8>, Vec<u16>) {
    let max = if depth == 16 { 65535u32 } else { 255 };
    let mut planes = vec![vec![]; 4];
    let mut expected = vec![];
    for y in 0..h {
        for x in 0..w {
            let a = match (x + y) % 3 {
                0 => 0,
                1 => max / 2,
                _ => max,
            };
            let mut color = [0u16; 4];
            color[3] = a as u16;
            for c in 0..3 {
                let original = ((x * 971 + y * 173 + c * 3257) % max) as u64;
                let m = ((original * a as u64 + max as u64 * (max - a) as u64 + max as u64 / 2)
                    / max as u64) as u16;
                planes[c as usize].push(m);
                color[c as usize] = if a == 0 {
                    0
                } else {
                    (((m as i64 + a as i64 - max as i64) * max as i64 + a as i64 / 2) / a as i64)
                        .clamp(0, max as i64) as u16
                };
            }
            planes[3].push(a as u16);
            expected.extend(color);
        }
    }
    let mut rows = vec![];
    for p in &planes {
        for row in p.chunks_exact(w as usize) {
            let mut bytes = vec![];
            let mut previous = 0;
            for &v in row {
                let out = if kind == 3 {
                    let out = if depth == 16 {
                        v.wrapping_sub(previous)
                    } else {
                        (v as u8).wrapping_sub(previous as u8) as u16
                    };
                    previous = v;
                    out
                } else {
                    v
                };
                if depth == 16 {
                    bytes.extend(out.to_be_bytes());
                } else {
                    bytes.push(out as u8);
                }
            }
            rows.push(bytes);
        }
    }
    let mut out = b"8BPS".to_vec();
    out.extend(1u16.to_be_bytes());
    out.extend([0; 6]);
    out.extend(4u16.to_be_bytes());
    out.extend(h.to_be_bytes());
    out.extend(w.to_be_bytes());
    out.extend(depth.to_be_bytes());
    out.extend(3u16.to_be_bytes());
    out.extend([0; 8]);
    let mut layer = vec![0; 8];
    layer.extend(b"8BIM");
    layer.extend(if depth == 16 { b"Mt16" } else { b"Mtrn" });
    layer.extend([0; 4]);
    out.extend((layer.len() as u32).to_be_bytes());
    out.extend(layer);
    out.extend(kind.to_be_bytes());
    if kind == 1 {
        let mut packed = vec![];
        for row in &rows {
            let mut p = vec![];
            for chunk in row.chunks(128) {
                p.push(chunk.len() as u8 - 1);
                p.extend(chunk);
            }
            out.extend((p.len() as u16).to_be_bytes());
            packed.push(p);
        }
        for p in packed {
            out.extend(p);
        }
    } else if kind >= 2 {
        let mut zip = ZlibEncoder::new(vec![], Compression::fast());
        for row in rows {
            zip.write_all(&row).unwrap();
        }
        out.extend(zip.finish().unwrap());
    } else {
        for row in rows {
            out.extend(row);
        }
    }
    (out, expected)
}
struct Fragmented {
    input: std::io::Cursor<Vec<u8>>,
    max: usize,
    control: Control,
    cancel: bool,
}
impl Read for Fragmented {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        self.max = self.max.max(b.len());
        if self.cancel && self.control.status().stage == "Decoding saved image" {
            self.control.cancel();
        }
        let n = b.len().min(97);
        self.input.read(&mut b[..n])
    }
}
impl Seek for Fragmented {
    fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
        self.input.seek(p)
    }
}
#[test]
fn row_decode_all_compressions_crosses_tiles_without_changing_precision() {
    for depth in [8, 16] {
        for kind in 0u16..=3 {
            let (bytes, expected) = flat(depth, kind, 259, 5);
            let control = Control::default();
            let mut reader = Fragmented {
                input: std::io::Cursor::new(bytes),
                max: 0,
                control: control.clone(),
                cancel: false,
            };
            let doc = psd::decode_reader(&mut reader, &control).unwrap();
            assert_eq!(doc.bit_depth, depth);
            assert!(!doc.read_only);
            let raster = &doc.layers[0].pixels;
            let words = if depth == 16 {
                raster.rgba16()
            } else {
                raster.rgba().into_iter().map(u16::from).collect()
            };
            assert!(
                words == expected,
                "precision mismatch at depth {depth}, compression {kind}"
            );
            let preview = control.take_preview().unwrap();
            assert_eq!((preview.0, preview.1), (259, 5));
            assert!(reader.max <= 65536);
            assert_eq!(control.status().stage, "Ready");
        }
    }
}
#[test]
fn layer_channels_preserve_alpha_first_order_and_native_words_in_all_compressions() {
    for depth in [8, 16] {
        for kind in 0u16..=3 {
            let (w, h) = (259u32, 3u32);
            let color = if depth == 16 {
                [60001u16, 12347, 34569, 32768]
            } else {
                [201, 47, 69, 128]
            };
            let mut channels = vec![];
            for c in [3, 2, 0, 1] {
                let mut rows = vec![];
                for _ in 0..h {
                    let mut row = vec![];
                    for x in 0..w {
                        let value = if kind == 3 && x > 0 { 0 } else { color[c] };
                        if depth == 16 {
                            row.extend(value.to_be_bytes());
                        } else {
                            row.push(value as u8);
                        }
                    }
                    rows.push(row);
                }
                let mut bytes = kind.to_be_bytes().to_vec();
                if kind == 1 {
                    let mut packed = vec![];
                    for row in &rows {
                        let mut p = vec![];
                        for chunk in row.chunks(128) {
                            p.push(chunk.len() as u8 - 1);
                            p.extend(chunk);
                        }
                        bytes.extend((p.len() as u16).to_be_bytes());
                        packed.push(p);
                    }
                    for p in packed {
                        bytes.extend(p);
                    }
                } else if kind >= 2 {
                    let mut zip = ZlibEncoder::new(vec![], Compression::fast());
                    for row in rows {
                        zip.write_all(&row).unwrap();
                    }
                    bytes.extend(zip.finish().unwrap());
                } else {
                    for row in rows {
                        bytes.extend(row);
                    }
                }
                channels.push((if c == 3 { -1i16 } else { c as i16 }, bytes));
            }
            let mut info = (-1i16).to_be_bytes().to_vec();
            for n in [0, 0, h, w] {
                info.extend(n.to_be_bytes());
            }
            info.extend(4u16.to_be_bytes());
            for (id, bytes) in &channels {
                info.extend(id.to_be_bytes());
                info.extend((bytes.len() as u32).to_be_bytes());
            }
            info.extend(b"8BIMnorm");
            info.extend([255, 0, 0, 0]);
            let mut extra = vec![0; 8];
            extra.extend([3, b'S', b'r', b'c']);
            info.extend((extra.len() as u32).to_be_bytes());
            info.extend(extra);
            for (_, bytes) in channels {
                info.extend(bytes);
            }
            let mut section = vec![];
            if depth == 8 {
                section.extend((info.len() as u32).to_be_bytes());
                section.extend(info);
                section.extend([0; 4]);
            } else {
                section.extend([0; 8]);
                section.extend(b"8BIMLr16");
                section.extend((info.len() as u32).to_be_bytes());
                let pad = (4 - info.len() % 4) % 4;
                section.extend(info);
                section.resize(section.len() + pad, 0);
            }
            let (bytes, _) = flat(depth, kind, w, h);
            let old_end = 38 + u32::from_be_bytes(bytes[34..38].try_into().unwrap()) as usize;
            let mut file = bytes[..34].to_vec();
            file.extend((section.len() as u32).to_be_bytes());
            file.extend(section);
            file.extend(&bytes[old_end..]);
            let doc = psd::decode(&file).unwrap();
            assert!(!doc.read_only);
            let raster = &doc.layers[0].pixels;
            if depth == 16 {
                assert_eq!(raster.get16(258, 2), color);
            } else {
                assert_eq!(raster.get(258, 2), color.map(|v| v as u8));
            }
        }
    }
}
#[test]
fn cancellation_and_truncation_never_return_a_partial_editable_document() {
    for kind in 0..=3 {
        let (bytes, _) = flat(16, kind, 260, 32);
        let control = Control::default();
        let mut reader = Fragmented {
            input: std::io::Cursor::new(bytes.clone()),
            max: 0,
            control: control.clone(),
            cancel: true,
        };
        assert!(psd::decode_reader(&mut reader, &control)
            .unwrap_err()
            .contains("canceled"));
        assert!(control.take_preview().is_none());
        assert!(psd::decode(&bytes[..bytes.len() - 4]).is_err());
    }
    let control = Control::default();
    control.cancel();
    assert!(
        psd::decode_reader(&mut std::io::Cursor::new(vec![0; 26]), &control)
            .unwrap_err()
            .contains("canceled")
    );
}
#[test]
fn disk_lru_respects_budget_and_rejects_corrupt_or_wrong_precision_entries() {
    let root = std::env::temp_dir().join(format!("peerbrush-cache-{}", uuid::Uuid::new_v4()));
    let bytes = vec![31; 256];
    let mut store = Store::new(root.clone(), 600).unwrap();
    store.put("a", 8, 8, 8, &bytes);
    store.put("b", 8, 8, 8, &bytes);
    assert!(store.bytes() <= 600);
    assert!(store.get("a", 8, 8, 8).is_some());
    store.put("c", 8, 8, 8, &bytes);
    assert!(store.get("b", 8, 8, 8).is_none());
    assert!(store.get("c", 8, 8, 16).is_none());
    store.put("oversized", 9, 9, 8, &vec![0; 324]);
    assert!(store.bytes() <= 600);
    drop(store);
    assert!(!root.exists());
    let mut store = Store::new(root.clone(), 600).unwrap();
    store.put("native", 8, 4, 16, &bytes);
    assert!(store.get("native", 8, 4, 16).unwrap() == bytes);
    let file = std::fs::read_dir(&root)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|s| s == "cache"))
        .unwrap();
    let mut b = std::fs::read(&file).unwrap();
    *b.last_mut().unwrap() ^= 1;
    std::fs::write(file, b).unwrap();
    assert!(store.get("native", 8, 4, 16).is_none());
    assert_eq!(store.bytes(), 0);
}
#[test]
fn canceled_stale_failed_and_invalid_opens_preserve_document_and_history() {
    let shared = Arc::new(Mutex::new(Engine::new()));
    let original = shared.lock().unwrap().doc.id.clone();
    let control = Control::default();
    control.bind(original.clone(), 999);
    assert!(server::open_progress(&shared, std::path::Path::new("missing.psd"), &control).is_err());
    assert!(shared.lock().unwrap().loading.is_none());
    let dir = std::env::temp_dir().join(format!("peerbrush-load-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("native.psd");
    let mut doc = Document::new_depth(32, 32, 16).unwrap();
    doc.layers[0]
        .pixels
        .set16(2, 3, [60001, 12347, 34569, 65535]);
    std::fs::write(&path, psd::encode(&doc).unwrap()).unwrap();
    let control = Control::default();
    control.cancel();
    assert!(server::open_progress(&shared, &path, &control).is_err());
    assert_eq!(shared.lock().unwrap().doc.id, original);
    assert!(shared.lock().unwrap().undo.is_empty());
    server::open(&shared, &path).unwrap();
    let reused = Control::default();
    server::open_progress(&shared, &path, &reused).unwrap();
    assert!(server::open_progress(&shared, &path, &reused)
        .unwrap_err()
        .contains("already started"));
    let e = shared.lock().unwrap();
    assert_eq!(e.doc.bit_depth, 16);
    assert_eq!(
        e.doc.layers[0].pixels.get16(2, 3),
        [60001, 12347, 34569, 65535]
    );
    assert!(e.loading.is_none());
    drop(e);
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(dir).unwrap();
}
