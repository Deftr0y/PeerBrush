use super::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

struct ObserveRows;
impl Drop for ObserveRows {
    fn drop(&mut self) {
        MASK_ROW_OBSERVER.with(|observer| {
            observer.take();
        });
    }
}
fn observe_rows(callback: impl FnMut(u32, usize, usize) + 'static) -> ObserveRows {
    MASK_ROW_OBSERVER.with(|observer| {
        assert!(observer.borrow().is_none());
        observer.replace(Some(Box::new(callback)));
    });
    ObserveRows
}
fn values(w: u32, h: u32, depth: u16) -> Vec<u16> {
    (0..h)
        .flat_map(|y| {
            (0..w).map(move |x| {
                let v = match x % 17 {
                    0..=2 => 0,
                    3..=5 => 65535,
                    _ => ((u64::from(x) * 137 + u64::from(y) * 313 + 12347) % 65536) as u16,
                };
                if depth == 16 {
                    v
                } else {
                    v / 257 * 257
                }
            })
        })
        .collect()
}
fn channel(values: &[u16], w: u32, h: u32, depth: u16, kind: u16, layout: Layout) -> Vec<u8> {
    let mut raw = values
        .iter()
        .flat_map(|v| {
            if depth == 16 {
                v.to_be_bytes().to_vec()
            } else {
                vec![(v / 257) as u8]
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(raw.len(), w as usize * h as usize * usize::from(depth / 8));
    let width = w as usize * usize::from(depth / 8);
    let mut out = kind.to_be_bytes().to_vec();
    if kind == 1 {
        let rows = (0..h as usize)
            .map(|y| {
                let mut packed = vec![128]; // PackBits no-op is legal.
                for part in raw[y * width..(y + 1) * width].chunks(128) {
                    if part.iter().all(|v| *v == part[0]) {
                        packed.extend([(1i16 - part.len() as i16) as i8 as u8, part[0]]);
                    } else {
                        packed.push(part.len() as u8 - 1);
                        packed.extend(part);
                    }
                }
                packed
            })
            .collect::<Vec<_>>();
        for row in &rows {
            if layout.large() {
                u32b(&mut out, row.len() as u32);
            } else {
                u16b(&mut out, row.len() as u16);
            }
        }
        for row in rows {
            out.extend(row);
        }
    } else if kind >= 2 {
        if kind == 3 && width != 0 {
            for row in raw.chunks_exact_mut(width) {
                if depth == 16 {
                    let mut previous = 0u16;
                    for word in row.chunks_exact_mut(2) {
                        let current = u16::from_be_bytes([word[0], word[1]]);
                        word.copy_from_slice(&current.wrapping_sub(previous).to_be_bytes());
                        previous = current;
                    }
                } else {
                    let mut previous = 0u8;
                    for byte in row {
                        let current = *byte;
                        *byte = current.wrapping_sub(previous);
                        previous = current;
                    }
                }
            }
        }
        let mut zip = ZlibEncoder::new(Vec::new(), Compression::fast());
        zip.write_all(&raw).unwrap();
        out.extend(zip.finish().unwrap());
    } else {
        out.extend(raw);
    }
    out
}

#[test]
fn rows_match_legacy_native_masks_with_tile_edges_negative_offsets_and_group_frames() {
    for depth in [8, 16] {
        let words = values(513, 259, depth);
        for layout in [Layout::Psd, Layout::Psb] {
            for kind in 0..4 {
                let bytes = channel(&words, 513, 259, depth, kind, layout);
                let legacy = if depth == 16 {
                    decode_channel_16(&bytes, 513, 259, layout).unwrap()
                } else {
                    decode_channel(&bytes, 513, 259, layout)
                        .unwrap()
                        .into_iter()
                        .map(|v| u16::from(v) * 257)
                        .collect()
                };
                assert_eq!(legacy, words);
                for (frame, offset) in [
                    ([269, 261], [-247, -3]),
                    ([521, 267], [7, 5]),
                    ([9, 5], [-700, -500]),
                ] {
                    let mut expected = Raster::new_depth(frame[0], frame[1], depth);
                    for y in 0..259 {
                        for x in 0..513 {
                            let v = legacy[y as usize * 513 + x as usize];
                            expected.set16(x + offset[0], y + offset[1], [v, v, v, 65535]);
                        }
                    }
                    let rows = Rc::new(RefCell::new(Vec::new()));
                    let observed = rows.clone();
                    let _guard = observe_rows(move |y, bytes, lengths| {
                        observed.borrow_mut().push((y, bytes, lengths))
                    });
                    let mut actual = Raster::new_depth(frame[0], frame[1], depth);
                    stream_mask_channel(
                        &bytes,
                        513,
                        259,
                        &mut actual,
                        offset,
                        layout,
                        &Default::default(),
                    )
                    .unwrap();
                    assert_eq!(actual, expected, "depth={depth}, kind={kind}");
                    assert_eq!(actual.depth, depth);
                    let rows = rows.borrow();
                    assert_eq!(rows.len(), 259);
                    for (index, &(y, bytes, lengths)) in rows.iter().enumerate() {
                        assert_eq!(y as usize, index);
                        assert_eq!(bytes, 513 * usize::from(depth / 8));
                        assert_eq!(lengths, if kind == 1 { 259 } else { 0 });
                    }
                    if offset == [-700, -500] {
                        assert_eq!(actual.bytes(), 0);
                    }
                }
            }
        }
    }
}

// Independent standard record and channel construction, without private sources.
// White layer RGB makes current merged-composite matting exact at either depth.
fn fixture(depth: u16, kind: u16, layout: Layout, enabled: bool, flags: u8) -> (Vec<u8>, Vec<u16>) {
    let (w, h, mw, mh) = (269, 3, 513, 4);
    let words = values(mw, mh, depth);
    let mut info = vec![];
    u16b(&mut info, (-1i16) as u16);
    for v in [0, 0, h as i32, w as i32] {
        i32b(&mut info, v);
    }
    u16b(&mut info, 5);
    let white = vec![65535; (w * h) as usize];
    let channels = [
        channel(&white, w, h, depth, kind, layout),
        channel(&white, w, h, depth, kind, layout),
        channel(&white, w, h, depth, kind, layout),
        channel(&white, w, h, depth, kind, layout),
        channel(&words, mw, mh, depth, kind, layout),
    ];
    for (id, bytes) in [0i16, 1, 2, -1, -2].into_iter().zip(&channels) {
        u16b(&mut info, id as u16);
        layout.write_length(&mut info, bytes.len() as u64).unwrap();
    }
    info.extend(b"8BIMnorm");
    info.extend([255, 0, 0, 0]);
    let mut mask = vec![];
    for v in [-1, -249, 3, 264] {
        i32b(&mut mask, v);
    }
    mask.extend([127, flags | if enabled { 0 } else { 2 }, 0, 0]);
    let mut extra = vec![];
    block(&mut extra, &mask);
    u32b(&mut extra, 0);
    extra.extend([4, b'M', b'a', b's', b'k', 0, 0, 0]);
    block(&mut info, &extra);
    for bytes in channels {
        info.extend(bytes);
    }
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut layers = vec![];
    if depth == 16 {
        layout.write_length(&mut layers, 0).unwrap();
        u32b(&mut layers, 0);
        layout_tag(&mut layers, b"Lr16", &info, layout);
        layout_tag(&mut layers, b"Mt16", &[], layout);
    } else {
        layout.write_length(&mut layers, info.len() as u64).unwrap();
        layers.extend(info);
        u32b(&mut layers, 0);
    }
    let mut out = b"8BPS".to_vec();
    u16b(&mut out, if layout.large() { 2 } else { 1 });
    out.extend([0; 6]);
    u16b(&mut out, 4);
    u32b(&mut out, h);
    u32b(&mut out, w);
    u16b(&mut out, depth);
    u16b(&mut out, 3);
    u32b(&mut out, 0);
    u32b(&mut out, 0);
    layout.write_length(&mut out, layers.len() as u64).unwrap();
    out.extend(layers);
    u16b(&mut out, 0);
    for _ in 0..3 {
        out.extend(&channel(&white, w, h, depth, 0, layout)[2..]);
    }
    let alpha = (0..h)
        .flat_map(|y| {
            let words = &words;
            (0..w).map(move |x| {
                if !enabled {
                    65535
                } else if x < 264 {
                    words[((y + 1) * mw + x + 249) as usize]
                } else {
                    127 * 257
                }
            })
        })
        .collect::<Vec<_>>();
    out.extend(&channel(&alpha, w, h, depth, 0, layout)[2..]);
    (out, words)
}
fn layout_tag(out: &mut Vec<u8>, key: &[u8; 4], bytes: &[u8], layout: Layout) {
    out.extend(b"8BIM");
    out.extend(key);
    if layout.tag_large(key) {
        layout.write_length(out, bytes.len() as u64).unwrap();
    } else {
        u32b(out, bytes.len() as u32);
    }
    out.extend(bytes);
    out.resize(out.len() + (4 - bytes.len() % 4) % 4, 0);
}

#[test]
fn standard_psd_psb_all_compressions_keep_mask_words_defaults_disabled_state_and_current_composite()
{
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            for kind in 0..4 {
                for enabled in [false, true] {
                    let (bytes, words) = fixture(depth, kind, layout, enabled, 0);
                    let original = bytes.clone();
                    let doc = decode(&bytes).unwrap();
                    assert!(!doc.read_only, "{:?}", doc.warnings);
                    assert_eq!(doc.bit_depth, depth);
                    assert_eq!(doc.layers.len(), 1);
                    let mask = doc.layers[0].mask.as_ref().unwrap();
                    assert!(mask.linked);
                    assert_eq!(mask.enabled, enabled);
                    assert_eq!(mask.steps.len(), 2);
                    assert_eq!(mask.steps[0].kind, "fill");
                    assert_eq!(mask.steps[0].value, 127.);
                    assert_eq!(mask.steps[1].kind, "paint");
                    assert_eq!(mask.steps[1].pixels.depth, depth);
                    for y in 0..3 {
                        for x in 0..269 {
                            let expected = if x < 264 {
                                let v = words[((y + 1) * 513 + x + 249) as usize];
                                [v, v, v, 65535]
                            } else {
                                [0; 4]
                            };
                            assert_eq!(mask.steps[1].pixels.get16(x, y), expected);
                        }
                    }
                    let png = doc.export_png().unwrap();
                    for saved in [encode(&doc).unwrap(), encode_psb(&doc).unwrap()] {
                        let reopened = decode(&saved).unwrap();
                        assert!(!reopened.read_only);
                        assert_eq!(reopened.export_png().unwrap(), png);
                        assert_eq!(
                            reopened.layers[0].mask.as_ref().unwrap().steps[1].pixels,
                            mask.steps[1].pixels
                        );
                    }
                    assert_eq!(bytes, original);
                }
            }
        }
    }
}

#[test]
fn row_cancellation_discards_partial_standard_decode_and_cannot_override_unsupported_mask_flags() {
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            for kind in 0..4 {
                let (bytes, _) = fixture(depth, kind, layout, true, 0);
                let original = bytes.clone();
                let control = crate::loading::Control::default();
                let cancel = control.clone();
                let seen = Rc::new(Cell::new(0));
                let observed = seen.clone();
                let guard = observe_rows(move |y, _, _| {
                    observed.set(observed.get() + 1);
                    if y == 1 {
                        cancel.cancel();
                    }
                });
                let result = decode_reader(&mut std::io::Cursor::new(&bytes), &control);
                assert!(result.unwrap_err().contains("Opening canceled"));
                assert_eq!(seen.get(), 2);
                assert_eq!(bytes, original);
                drop(guard);
                let (protected, _) = fixture(depth, kind, layout, true, 4);
                let rows = Rc::new(Cell::new(0));
                let count = rows.clone();
                let _guard = observe_rows(move |_, _, _| count.set(count.get() + 1));
                let doc = decode(&protected).unwrap();
                assert!(doc.read_only);
                assert!(doc
                    .warnings
                    .iter()
                    .any(|warning| warning.contains("mask flags")));
                assert_eq!(
                    rows.get(),
                    0,
                    "Unsupported masks do not enter editable decoding"
                );
            }
        }
    }
}

#[test]
fn malformed_packets_zip_completion_zero_sizes_and_original_plane_budgets_stay_bounded() {
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            let run = |bytes: &[u8], w, h| {
                stream_mask_channel(
                    bytes,
                    w,
                    h,
                    &mut Raster::new_depth(3, 2, depth),
                    [0, 0],
                    layout,
                    &Default::default(),
                )
            };
            for kind in 0..4 {
                for (w, h) in [(0, 0), (0, 2), (2, 0)] {
                    run(&channel(&[], w, h, depth, kind, layout), w, h).unwrap();
                }
                let words = values(3, 2, depth);
                let good = channel(&words, 3, 2, depth, kind, layout);
                for missing in [1, 2] {
                    assert!(run(&good[..good.len() - missing], 3, 2).is_err());
                }
                let mut trailing = good.clone();
                trailing.extend([17, 23]);
                run(&trailing, 3, 2).unwrap(); // Existing channel slices tolerate trailing bytes.
                if kind >= 2 {
                    let mut corrupt = good.clone();
                    *corrupt.last_mut().unwrap() ^= 0x80;
                    assert!(run(&corrupt, 3, 2).is_err());
                    let extra = channel(&values(7, 1, depth), 7, 1, depth, 2, layout);
                    assert!(run(&extra, 3, 2).is_err());
                }
            }
            let mut bad = 1u16.to_be_bytes().to_vec();
            if layout.large() {
                u32b(&mut bad, u32::MAX);
            } else {
                u16b(&mut bad, 2);
                bad.extend([127, 0]);
            }
            assert!(run(&bad, 1, 1).is_err());
            assert!(run(&99u16.to_be_bytes(), 1, 1).is_err());
            let cap = if depth == 16 {
                256 * 1024 * 1024 / 2
            } else {
                32 * 1024 * 1024
            };
            let observed = Rc::new(Cell::new(false));
            let called = observed.clone();
            let _guard = observe_rows(move |_, _, _| called.set(true));
            assert!(run(&[0, 0], cap + 1, 1).unwrap_err().contains("budget"));
            assert!(
                !observed.get(),
                "Admission precedes row buffers, tiles and callbacks"
            );
        }
    }
}
