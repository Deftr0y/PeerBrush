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
    mask_fixture(depth, kind, layout, enabled, flags, None, false)
}

fn mask_fixture(
    depth: u16, kind: u16, layout: Layout, enabled: bool, flags: u8,
    paired: Option<(u16, bool)>,
    extend_rendered: bool,
) -> (Vec<u8>, Vec<u16>) {
    let (w, h, mw, mh) = (269, 3, 513, 4);
    let words = values(mw, mh, depth);
    let mut info = vec![];
    u16b(&mut info, (-1i16) as u16);
    for v in [0, 0, h as i32, w as i32] {
        i32b(&mut info, v);
    }
    u16b(&mut info, 5 + u16::from(paired.is_some()));
    let white = vec![65535; (w * h) as usize];
    let rendered = if extend_rendered {
        let width = (mw + 3) as usize;
        let mut padded = vec![127 * 257; width * (mh + 2) as usize];
        for (y, source) in words.chunks_exact(mw as usize).enumerate() {
            let at = (y + 1) * width + 2;
            padded[at..at + source.len()].copy_from_slice(source);
        }
        channel(&padded, mw + 3, mh + 2, depth, kind, layout)
    } else {
        channel(&words, mw, mh, depth, kind, layout)
    };
    let mut channels = vec![
        channel(&white, w, h, depth, kind, layout),
        channel(&white, w, h, depth, kind, layout),
        channel(&white, w, h, depth, kind, layout),
        channel(&white, w, h, depth, kind, layout),
        rendered,
    ];
    if let Some((compression, different)) = paired {
        let mut user = words.clone();
        if different {
            // The difference is outside the visible layer frame.
            user[0] ^= if depth == 16 { 1 } else { 257 };
        }
        channels.push(channel(&user, mw, mh, depth, compression, layout));
    }
    for (id, bytes) in [0i16, 1, 2, -1, -2, -3].into_iter().zip(&channels) {
        u16b(&mut info, id as u16);
        layout.write_length(&mut info, bytes.len() as u64).unwrap();
    }
    info.extend(b"8BIMnorm");
    info.extend([255, 0, 0, 0]);
    let mut mask = vec![];
    let user_bounds = [-1, -249, 3, 264];
    let rendered_bounds = if extend_rendered { [-2, -251, 4, 265] } else { user_bounds };
    for v in rendered_bounds {
        i32b(&mut mask, v);
    }
    mask.extend([127, flags | if enabled { 0 } else { 2 }, 0, 0]);
    if paired.is_some() {
        mask[18] = mask[17];
        mask[19] = mask[16];
        mask[17] |= 8;
        for v in user_bounds {
            i32b(&mut mask, v);
        }
    }
    let mut extra = vec![];
    block(&mut extra, &mask);
    u32b(&mut extra, 0);
    extra.extend([4, b'M', b'a', b's', b'k', 0, 0, 0]);
    if paired.is_some() {
        tag(&mut extra, b"vmsk", &super::vector_mask::inverted_empty_mask_fixture());
    }
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

#[test]
fn paired_photoshop_masks_require_equal_native_samples_and_save_both_planes() {
    for depth in [8, 16] {
        for (layout, extend_rendered) in [Layout::Psd, Layout::Psb]
            .into_iter()
            .flat_map(|layout| [(layout, false), (layout, true)])
        {
            let (input, words) =
                mask_fixture(depth, 1, layout, true, 0, Some((3, false)), extend_rendered);
            for document in [
                decode(&input).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&input), &Default::default()).unwrap(),
            ] {
                assert!(!document.read_only, "{:?}", document.warnings);
                let layer = &document.layers[0];
                assert_eq!(layer.psd_metadata[0].key, "vmsk");
                let paint = &layer.mask.as_ref().unwrap().steps[1].pixels;
                let value = words[513 + 249];
                assert_eq!(paint.get16(0, 0), [value, value, value, 65535]);
                assert_eq!(paint.retained.as_ref().unwrap().pixels.get16(0, 0)[0], words[0]);

                let saved = encode_layout(&document, layout).unwrap();
                let index = structural::index(
                    &mut std::io::Cursor::new(&saved), structural::Limits::current(), || Ok(()),
                ).unwrap();
                let record = &index.layers[0];
                assert_eq!(record.mask.bytes, 36);
                let mask_bytes = |id| {
                    let channel = record.channels.iter().find(|channel| channel.id == id).unwrap();
                    &saved[channel.span.offset as usize..(channel.span.offset + channel.span.bytes) as usize]
                };
                assert_eq!(mask_bytes(-2), mask_bytes(-3));
                let restored = decode(&saved).unwrap();
                assert!(!restored.read_only, "{:?}", restored.warnings);
                assert_eq!(restored.layers[0].mask.as_ref().unwrap().steps[1].pixels, *paint);
                let legacy = adjustments::tests::replace_sources(&saved, layout, &document, 59);
                assert!(decode(&legacy).unwrap().read_only);
            }
            let (different, _) =
                mask_fixture(depth, 1, layout, true, 0, Some((3, true)), extend_rendered);
            for protected in [
                decode(&different).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&different), &Default::default()).unwrap(),
            ] {
                assert!(protected.read_only);
                assert!(protected.warnings.iter().any(|w| w.contains("mask channels differ")));
            }
        }
    }
}

#[test]
fn backed_photoshop_stages_native_color_and_off_frame_mask_samples() {
    use crate::engine::native::worker::tests::Fixture;

    for depth in [8, 16] {
        let fixture_dir = Fixture::new();
        let (runtime, _connection) = fixture_dir.runtime();
        let session = runtime.begin().unwrap();
        let fragment = session.fresh_fragment().unwrap();
        let check = || session.check();
        let (bytes, words) = fixture(depth, 0, Layout::Psb, true, 0);
        let path = fixture_dir.path().join("Masked.psb");
        std::fs::write(&path, &bytes).unwrap();
        let mut file = crate::file_safety::open_native_read(&path).unwrap();
        let version = crate::file_safety::native_source_version(&path, &mut file, &mut [0; 65536], &check).unwrap();
        let decoded = decode_backed(&mut file, &Default::default(), version, fragment.store().clone(), &check).unwrap();
        let layer = &decoded.document.layers[0];
        let sample = |surface: &crate::raster::Surface, x, y| {
            let region = crate::raster::backed::Region::new([x, y, x + 1, y + 1]).unwrap();
            surface.prepare_region(region, &mut || Ok(())).unwrap().pixel16(x, y).unwrap()
        };
        assert_eq!(sample(&decoded.color[&layer.id], 0, 0), [65535; 4]);
        let value = words[513 + 249];
        assert_eq!(sample(&decoded.masks[&layer.id], 0, 0), [value, value, value, 65535]);
        assert_eq!(sample(&decoded.masks[&layer.id], 264, 0), [0; 4]);
        let retained = words[6];
        assert_eq!(sample(&decoded.mask_originals[&layer.id], 6, 0), [retained, retained, retained, 65535]);
        drop(decoded);
        session.abort_fragment(fragment, &mut || session.cleanup_check()).unwrap();
    }
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

fn solid_color_definition(red: f64) -> Vec<u8> {
    let mut descriptor = 16u32.to_be_bytes().to_vec();
    descriptor.extend(b"\0\0\0\0\0\0\0\0null\0\0\0\x01\0\0\0\0Clr Objc");
    descriptor.extend(b"\0\0\0\0\0\0\0\0RGBC\0\0\0\x03");
    for (key, value) in [(b"Rd  ", red), (b"Grn ", 137.), (b"Bl  ", 231.)] {
        descriptor.extend([0; 4]);
        descriptor.extend(key);
        descriptor.extend(b"doub");
        descriptor.extend(value.to_be_bytes());
    }
    descriptor
}

fn solid_color_fixture(
    depth: u16, layout: Layout, red: f64, matches: bool,
    overlay: Option<(&[u8], [u16; 3])>,
) -> Vec<u8> {
    let descriptor = solid_color_definition(red);
    let mut appearance = overlay.map_or([42, 137, 231], |(_, color)| color);
    if !matches { appearance[0] += 1; }
    procedural_fill_fixture(depth, layout, b"SoCo", &descriptor, overlay.map(|(style, _)| style),
        [65535, 0], [65535, 0], [appearance.map(|v| v * 257), [65535; 3]])
}

fn procedural_fill_fixture<const WIDTH: usize>(
    depth: u16, layout: Layout, key: &[u8; 4], descriptor: &[u8], style: Option<&[u8]>,
    alpha: [u16; WIDTH], merged_alpha: [u16; WIDTH], appearance: [[u16; 3]; WIDTH],
) -> Vec<u8> {
    let width = WIDTH as u32;
    let channel_bytes = channel(&alpha, width, 1, depth, 0, layout);
    let mut info = (-1i16).to_be_bytes().to_vec();
    info.extend([0; 16]); // Procedural layer with no raster bounds.
    u16b(&mut info, 1);
    u16b(&mut info, (-2i16) as u16);
    layout.write_length(&mut info, channel_bytes.len() as u64).unwrap();
    info.extend(b"8BIMnorm\xff\0\x18\0");
    let mut extra = Vec::new();
    let mut mask = Vec::new();
    for bound in [0, 0, 1, width as i32] {
        i32b(&mut mask, bound);
    }
    mask.extend([0; 4]);
    block(&mut extra, &mask);
    u32b(&mut extra, 0);
    extra.extend(b"\x03RGB");
    tag(&mut extra, key, descriptor);
    if let Some(style) = style {
        tag(&mut extra, b"lfx2", style);
    }
    block(&mut info, &extra);
    info.extend(channel_bytes);
    single_layer_fixture(depth, layout, info, merged_alpha, appearance)
}

fn single_layer_fixture<const WIDTH: usize>(
    depth: u16, layout: Layout, mut info: Vec<u8>,
    merged_alpha: [u16; WIDTH], appearance: [[u16; 3]; WIDTH],
) -> Vec<u8> {
    let width = WIDTH as u32;
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut layers = Vec::new();
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
    u32b(&mut out, 1);
    u32b(&mut out, width);
    u16b(&mut out, depth);
    u16b(&mut out, 3);
    u32b(&mut out, 0);
    u32b(&mut out, 0);
    layout.write_length(&mut out, layers.len() as u64).unwrap();
    out.extend(layers);
    u16b(&mut out, 0);
    for component in 0..3 {
        let samples = appearance.map(|rgb| rgb[component]);
        out.extend(&channel(&samples, width, 1, depth, 0, layout)[2..]);
    }
    out.extend(&channel(&merged_alpha, width, 1, depth, 0, layout)[2..]);
    out
}

fn parameterized_mask_fixture(
    depth: u16,
    layout: Layout,
    density: Option<u8>,
    matches: bool,
) -> (Vec<u8>, [u16; 3]) {
    let mask = if depth == 16 { [0, 10531, 65535] } else { [0, 80 * 257, 65535] };
    let unit = if depth == 16 { 1 } else { 257 };
    let maximum = 65535 / unit;
    let alpha = mask.map(|sample| {
        let unmasked = u32::from(maximum - sample / unit);
        (maximum - ((unmasked * u32::from(density.unwrap_or(255)) + 127) / 255) as u16) * unit
    });
    let mut matte = alpha.map(|sample| [65535 - sample; 3]);
    if !matches {
        matte[1][0] += unit;
    }
    let bounds = [0, 0, 1, 3];
    let mut info = (-1i16).to_be_bytes().to_vec();
    for coordinate in bounds {
        i32b(&mut info, coordinate);
    }
    let channels = [(-1, [65535; 3]), (0, [0; 3]), (1, [0; 3]), (2, [0; 3]), (-2, mask)];
    u16b(&mut info, channels.len() as u16);
    for (id, _) in channels {
        u16b(&mut info, id as u16);
        layout.write_length(&mut info, 2 + 3 * u64::from(depth / 8)).unwrap();
    }
    info.extend(b"8BIMnorm\xff\0\0\0");
    let mut header = Vec::new();
    for coordinate in bounds {
        i32b(&mut header, coordinate);
    }
    header.extend([255, MASK_PARAMETERS]);
    if let Some(density) = density {
        header.extend([USER_MASK_DENSITY, density]);
    } else {
        header.push(USER_MASK_FEATHER);
        header.extend(0f64.to_be_bytes());
    }
    let mut extra = Vec::new();
    block(&mut extra, &header);
    u32b(&mut extra, 0);
    extra.extend(b"\x03RGB");
    block(&mut info, &extra);
    for (_, samples) in channels {
        info.extend(channel(&samples, 3, 1, depth, 0, layout));
    }
    (single_layer_fixture(depth, layout, info, alpha, matte), mask)
}

#[test]
fn raster_mask_density_import_keeps_original_native_samples_and_refuses_a_mismatched_composite() {
    let mut with_feather = [0; 28];
    with_feather[17..20].copy_from_slice(&[MASK_PARAMETERS, USER_MASK_DENSITY | USER_MASK_FEATHER, 153]);
    assert_eq!(standard_mask_header(&with_feather).unwrap().density, Some(153));
    with_feather[20..28].copy_from_slice(&0.5f64.to_be_bytes());
    assert!(standard_mask_header(&with_feather).is_none());
    with_feather[18] = USER_MASK_FEATHER;
    with_feather[19..].fill(0);
    assert!(standard_mask_header(&with_feather[..27]).unwrap().has_parameters);
    assert!(standard_mask_header(&with_feather).unwrap().has_parameters);
    with_feather[27] = 1;
    assert!(standard_mask_header(&with_feather).is_none());
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            for density in [0, 153, 255] {
                let (bytes, raw_mask) = parameterized_mask_fixture(depth, layout, Some(density), true);
                let decoded = decode(&bytes).unwrap();
                assert!(!decoded.read_only, "{:?}", decoded.warnings);
                let layer = &decoded.layers[0];
                let mask = layer.mask.as_ref().unwrap();
                assert_eq!(mask.steps.len(), 3);
                assert_eq!(mask.steps[2].kind, "density");
                assert_eq!(mask.steps[2].weight, 1.);
                assert_eq!(mask.steps[2].value, f32::from(density));
                for (x, sample) in raw_mask.into_iter().enumerate() {
                    assert_eq!(mask.steps[1].pixels.get16(x as i32, 0), [sample, sample, sample, 65535]);
                }
                assert!(!crate::compatibility::report(&decoded).unwrap().losses.iter().any(|loss| {
                    loss.code == crate::compatibility::LossCode::MaskStackBaked
                }));
                let encoded = encode_layout(&decoded, layout).unwrap();
                let standard = structural::index_for_decode(&mut std::io::Cursor::new(&encoded), || Ok(())).unwrap();
                let header = standard.layers[0].mask.offset as usize;
                assert_eq!(standard_mask_header(&encoded[header..header + 20]).unwrap().density, Some(density));
                let mask_channel = standard.layers[0].channels.iter().find(|channel| channel.id == -2).unwrap();
                let start = mask_channel.span.offset as usize;
                let length = 3 * usize::from(depth / 8);
                assert_eq!(&encoded[start..start + 2], &[0, 0]);
                assert_eq!(&encoded[start + 2..start + 2 + length], &channel(&raw_mask, 3, 1, depth, 0, layout)[2..]);
                assert_eq!(&encoded[encoded.len() - length..], &bytes[bytes.len() - length..]);
                let reopened = decode(&encoded).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                let saved_mask = reopened.layers[0].mask.as_ref().unwrap();
                assert_eq!(saved_mask.steps[2].value, f32::from(density));
                assert_eq!(saved_mask.steps[2].weight, mask.steps[2].weight);
                assert_eq!(saved_mask.steps[1].pixels.rgba16(), mask.steps[1].pixels.rgba16());
            }
            let (bytes, _) = parameterized_mask_fixture(depth, layout, Some(153), false);
            let rejected = decode(&bytes).unwrap();
            assert!(rejected.read_only);
            assert!(rejected.warnings.iter().any(|warning| warning.contains("native saved composite")));
            let (mut bytes, _) = parameterized_mask_fixture(depth, layout, Some(153), true);
            let index = structural::index_for_decode(&mut std::io::Cursor::new(&bytes), || Ok(())).unwrap();
            let mask = index.layers[0].mask.offset as usize;
            bytes[mask + 18] = USER_MASK_DENSITY | 2;
            assert!(decode(&bytes).unwrap().read_only);

            let (bytes, raw_mask) = parameterized_mask_fixture(depth, layout, None, true);
            let indexed = decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap();
            for doc in [decode(&bytes).unwrap(), indexed] {
                assert!(!doc.read_only, "{:?}", doc.warnings);
                let steps = &doc.layers[0].mask.as_ref().unwrap().steps;
                assert_eq!(steps.len(), 2);
                for (x, sample) in raw_mask.into_iter().enumerate() {
                    assert_eq!(steps[1].pixels.get16(x as i32, 0), [sample, sample, sample, 65535]);
                }
            }
            let saved = encode_layout(&decode(&bytes).unwrap(), layout).unwrap();
            let index = structural::index_for_decode(&mut std::io::Cursor::new(&saved), || Ok(())).unwrap();
            let header = index.layers[0].mask;
            assert_eq!(header.bytes, 20);
            let start = header.offset as usize;
            assert!(!standard_mask_header(&saved[start..start + 20]).unwrap().has_parameters);
            let (bytes, _) = parameterized_mask_fixture(depth, layout, None, false);
            assert!(decode(&bytes).unwrap().read_only);
        }
    }
}

pub(crate) fn native_gradient_grid_fixture(depth: u16, matches: bool, compression: u16) -> Vec<u8> {
    const WIDTH: usize = 8198;
    let layout = if depth == 16 { Layout::Psb } else { Layout::Psd };
    let definition = gradient_fill::descriptor(&serde_json::json!({
        "angle":0., "opacity":1., "pixel_grid":true, "use_gradient_alpha":true,
        "color_stops":[{"position":0., "color":[0.,0.,0.]}, {"position":1., "color":[1.,1.,1.]}],
        "alpha_stops":[]
    })).unwrap();
    // For an even-width, document-aligned ramp, integer sample x has
    // normalized position x / WIDTH. Compute nearest samples independently.
    let mut appearance = std::array::from_fn::<_, WIDTH, _>(|x| {
        let word = ((x * 65535 + WIDTH / 2) / WIDTH) as u16;
        let sample = if depth == 16 { word } else { u16::from(crate::raster::project16(word)) * 257 };
        [sample; 3]
    });
    if !matches {
        appearance[WIDTH - 1][0] ^= 1;
    }
    let mut bytes = procedural_fill_fixture(
        depth, layout, b"GdFl", definition.bytes(), None, [65535; WIDTH], [65535; WIDTH], appearance,
    );
    if compression != 0 {
        bytes.truncate(bytes.len() - 2 - 4 * WIDTH * usize::from(depth / 8));
        let samples = (0..3).flat_map(|c| appearance.iter().map(move |pixel| pixel[c]))
            .chain(std::iter::repeat_n(65535, WIDTH)).collect::<Vec<_>>();
        bytes.extend(channel(&samples, WIDTH as u32, 4, depth, compression, layout));
    }
    bytes
}

pub(crate) fn native_stroke_fixture(depth: u16, exact: bool, partial: bool) -> Vec<u8> {
    use super::styles::tests::descriptor;
    const WIDTH: u32 = 8324;
    let layout = if depth == 16 { Layout::Psb } else { Layout::Psd };
    let enumeration = |class: &[u8; 4], value: &[u8; 4]| {
        [&[0; 4][..], class.as_slice(), &[0; 4][..], value.as_slice()].concat()
    };
    let unit = |unit: &[u8; 4], value: f64| [unit.as_slice(), &value.to_be_bytes()].concat();
    let stroke = descriptor("FrFX", vec![
        ("enab", b"bool", vec![1]),
        ("present", b"bool", vec![1]),
        ("Styl", b"enum", enumeration(b"FStl", b"OutF")),
        ("PntT", b"enum", enumeration(b"FrFl", b"SClr")),
        ("Md  ", b"enum", enumeration(b"BlnM", b"Nrml")),
        ("Opct", b"UntF", unit(b"#Prc", 100.)),
        ("Sz  ", b"UntF", unit(b"#Pxl", 128.)),
        ("Clr ", b"Objc", descriptor("RGBC", vec![
            ("Rd  ", b"doub", 255f64.to_be_bytes().to_vec()),
            ("Grn ", b"doub", 0f64.to_be_bytes().to_vec()),
            ("Bl  ", b"doub", 0f64.to_be_bytes().to_vec()),
        ])),
    ]);
    let style = [[0, 0, 0, 0, 0, 0, 0, 16].as_slice(), &descriptor("Lefx", vec![
        ("masterFXSwitch", b"bool", vec![1]), ("FrFX", b"Objc", stroke),
    ])].concat();
    let half = if depth == 16 { 32768 } else { 128 * 257 };
    let source = [12347, 23459, 34571].map(|word| {
        if depth == 16 { word } else { u16::from(crate::raster::project16(word)) * 257 }
    });
    let mut info = (-1i16).to_be_bytes().to_vec();
    for bound in [0, 8191, 1, 8194] { i32b(&mut info, bound); }
    u16b(&mut info, 4);
    let mut channels = Vec::new();
    for (id, middle) in [-1i16, 0, 1, 2].into_iter().zip([
        if partial { half } else { 65535 }, source[0], source[1], source[2],
    ]) {
        let raw = channel(&[0, middle, 0], 3, 1, depth, 0, layout);
        u16b(&mut info, id as u16);
        layout.write_length(&mut info, raw.len() as u64).unwrap();
        channels.extend(raw);
    }
    info.extend(b"8BIMnorm\xff\0\0\0");
    let mut extra = vec![0; 8]; // No mask or blend ranges.
    extra.extend(b"\x03Ink");
    layout_tag(&mut extra, b"lfx2", &style, layout);
    block(&mut info, &extra);
    info.extend(channels);
    let mut layers = Vec::new();
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
    u32b(&mut out, 1);
    u32b(&mut out, WIDTH);
    u16b(&mut out, depth);
    u16b(&mut out, 3);
    out.extend([0; 8]);
    layout.write_length(&mut out, layers.len() as u64).unwrap();
    out.extend(layers);
    u16b(&mut out, 0);
    for (c, ink) in source.into_iter().chain([65535]).enumerate() {
        let mut merged = vec![if c == 3 { 0 } else { 65535 }; WIDTH as usize];
        let fringe = match c { 0 => 65535, 3 => half, _ => 65535 - half };
        let solid = if c == 0 || c == 3 { 65535 } else { 0 };
        merged[8064] = fringe;
        merged[8065..8320].fill(solid);
        merged[8192] = ink;
        merged[8320] = fringe;
        if !exact && c == 0 { merged[8192] ^= if depth == 16 { 1 } else { 257 }; }
        out.extend(&channel(&merged, WIDTH, 1, depth, 0, layout)[2..]);
    }
    out
}

#[test]
fn photoshop_pattern_fill_keeps_native_samples_tile_coverage_and_the_raster_mask() {
    use super::styles::tests::descriptor;
    let pattern_id = "tile-one";
    let mut id = (pattern_id.len() as u32).to_be_bytes().to_vec();
    for code in pattern_id.encode_utf16() { u16b(&mut id, code); }
    for depth in [8, 16] {
        let colors = if depth == 8 {
            [[21u16, 83, 171], [29, 107, 239]].map(|rgb| rgb.map(|v| v * 257))
        } else {
            [[5421, 21283, 43901], [7529, 27107, 61239]]
        };
        let tile_colors = [colors[0], colors[0], colors[0], colors[1], colors[0], colors[1]];
        for layout in [Layout::Psd, Layout::Psb] {
            for variant in ["opaque", "linked", "transparent", "mismatch", "missing",
                "offset", "offset_mismatch", "fractional", "rotated", "rotated_mismatch", "arbitrary_angle"] {
                let mut definition = 16u32.to_be_bytes().to_vec();
                let mut properties = vec![
                    ("Ptrn", b"Objc", descriptor("Ptrn", vec![("Idnt", b"TEXT", id.clone())])),
                    ("Lnkd", b"bool", vec![u8::from(variant == "linked")]),
                ];
                if matches!(variant, "offset" | "offset_mismatch" | "fractional") {
                    let x: f64 = if variant == "fractional" { 1.5 } else { 1. };
                    properties.push(("phase", b"Objc", descriptor("Pnt ", vec![
                        ("Hrzn", b"doub", x.to_be_bytes().to_vec()),
                        ("Vrtc", b"UntF", [b"#Pxl".as_slice(), &(-1f64).to_be_bytes()].concat()),
                    ])));
                }
                if matches!(variant, "rotated" | "rotated_mismatch" | "arbitrary_angle") {
                    let angle: f64 = if variant == "arbitrary_angle" { 15. } else { 90. };
                    properties.push(("Angl", b"UntF", [b"#Ang".as_slice(), &angle.to_be_bytes()].concat()));
                }
                definition.extend(descriptor("null", properties));
                let coverage = if variant == "transparent" {
                    if depth == 8 { 113 * 257 } else { 29123 }
                } else {
                    65535
                };
                let max = if depth == 8 { 255u64 } else { 65535 };
                let factor = 65535 / max;
                let a = u64::from(coverage) / factor;
                let mut first = colors[usize::from(matches!(variant, "offset" | "rotated"))].map(|sample| {
                    (((u64::from(sample) / factor * a + max * (max - a) + max / 2) / max) * factor) as u16
                });
                if variant == "mismatch" { first[0] += 257; }
                let mut bytes = procedural_fill_fixture(depth, layout, b"PtFl", &definition, None,
                    [65535, 0], [coverage, 0], [first, [65535; 3]]);
                if variant != "missing" {
                    let mut tile = Vec::new();
                    u32b(&mut tile, 1);
                    u32b(&mut tile, 3);
                    u16b(&mut tile, 2);
                    u16b(&mut tile, 3);
                    u32b(&mut tile, 0); // Empty Unicode name.
                    tile.push(pattern_id.len() as u8);
                    tile.extend(pattern_id.as_bytes());
                    u32b(&mut tile, 3);
                    let mut arrays = Vec::new();
                    for bound in [0, 0, 2, 3] { i32b(&mut arrays, bound); }
                    u32b(&mut arrays, 3);
                    for samples in (0..3).map(|c| Some(tile_colors.map(|rgb| rgb[c])))
                        .chain([None, (variant == "transparent").then_some([coverage, 65535, 65535, 65535, 65535, 65535])])
                    {
                        u32b(&mut arrays, u32::from(samples.is_some()));
                        let Some(samples) = samples else { continue; };
                        let mut record = Vec::new();
                        u32b(&mut record, u32::from(depth));
                        for bound in [0, 0, 2, 3] { i32b(&mut record, bound); }
                        u16b(&mut record, depth);
                        record.push(0); // Raw channel samples.
                        record.extend_from_slice(&channel(&samples, 3, 2, depth, 0, layout)[2..]);
                        block(&mut arrays, &record);
                    }
                    block(&mut tile, &arrays);
                    let mut patterns = Vec::new();
                    block(&mut patterns, &tile);
                    patterns.resize(patterns.len().next_multiple_of(4), 0);
                    let mut header = Cursor::new(&bytes);
                    header.bytes(26).unwrap();
                    header.block().unwrap();
                    header.block().unwrap();
                    let length_at = header.pos;
                    let length = header.length(layout.large()).unwrap();
                    let start = header.pos;
                    let mut layer_section = bytes[start..start + length].to_vec();
                    layout_tag(&mut layer_section, b"Patt", &patterns, layout);
                    let merged = bytes.split_off(start + length);
                    bytes.truncate(length_at);
                    layout.write_length(&mut bytes, layer_section.len() as u64).unwrap();
                    bytes.extend(layer_section);
                    bytes.extend(merged);
                }
                for document in [decode(&bytes).unwrap(), decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap()] {
                    assert_eq!(document.read_only, !matches!(variant, "opaque" | "transparent" | "offset" | "rotated"),
                        "{variant}: {:?}", document.warnings);
                    if document.read_only { continue; }
                    let layer = &document.layers[0];
                    assert_eq!(layer.kind, "fill");
                    assert!(layer.mask.is_some());
                    assert_eq!(layer.effects[0].kind, "pattern_overlay");
                    assert_eq!(layer.effects[0].settings["use_pattern_alpha"], true);
                    let phase = if variant == "offset" { [1., -1.] } else { [0., 0.] };
                    assert_eq!(layer.effects[0].settings["phase_x"].as_f64(), Some(phase[0]));
                    assert_eq!(layer.effects[0].settings["phase_y"].as_f64(), Some(phase[1]));
                    let angle = if variant == "rotated" { 90. } else { 0. };
                    assert_eq!(layer.effects[0].settings["bitmap_angle"].as_f64(), Some(angle));
                    let report = crate::compatibility::report(&document).unwrap();
                    assert_eq!(report.layers[0].standard, crate::compatibility::Representation::PatternFill);
                    let encoded = encode_layout(&document, layout).unwrap();
                    let restored = decode(&encoded).unwrap();
                    assert!(!restored.read_only, "{:?}", restored.warnings);
                    assert_eq!(restored.bit_depth, depth);
                    assert_eq!(restored.layers[0].effects[0].settings, layer.effects[0].settings);
                    assert_eq!(restored.export_png().unwrap(), document.export_png().unwrap());
                    let mut header = Cursor::new(&encoded);
                    header.bytes(26).unwrap();
                    header.block().unwrap();
                    let resources_at = header.pos;
                    header.block().unwrap();
                    let mut standard = encoded[..resources_at].to_vec();
                    u32b(&mut standard, 0);
                    standard.extend(&encoded[header.pos..]);
                    let independent = decode(&standard).unwrap();
                    assert!(!independent.read_only, "{:?}", independent.warnings);
                    assert_eq!(independent.layers[0].kind, "fill");
                    assert_eq!(independent.layers[0].effects[0].settings, layer.effects[0].settings);
                    assert_eq!(independent.export_png().unwrap(), document.export_png().unwrap());
                    let prior = super::adjustments::tests::replace_sources(&encoded, layout, &document, patterns::STANDARD_FORMAT);
                    assert_eq!(decode(&prior).unwrap().read_only, matches!(variant, "offset" | "rotated"));
                    let prior = super::adjustments::tests::replace_sources(&encoded, layout, &document, patterns::OFFSET_FORMAT);
                    assert_eq!(decode(&prior).unwrap().read_only, variant == "rotated");
                    let downgraded = super::adjustments::tests::replace_sources(&encoded, layout, &document, patterns::STANDARD_FORMAT - 1);
                    assert!(decode(&downgraded).unwrap().read_only);
                }
            }
        }
    }
}

#[test]
fn photoshop_gradient_fill_keeps_native_stops_coverage_and_mask() {
    use super::styles::tests::descriptor;
    let unit = |kind: &[u8; 4], value: f64| [kind.as_slice(), &value.to_be_bytes()].concat();
    let mut colors = 2u32.to_be_bytes().to_vec();
    for (position, value) in [(0i32, 0f64), (4096, 255.)] {
        let rgb = descriptor("RGBC", ["Rd  ", "Grn ", "Bl  "].into_iter()
            .map(|key| (key, b"doub", value.to_be_bytes().to_vec())).collect());
        colors.extend(b"Objc");
        colors.extend(descriptor("Clrt", vec![
            ("Clr ", b"Objc", rgb),
            ("Type", b"enum", b"\0\0\0\0Clry\0\0\0\0UsrS".to_vec()),
            ("Lctn", b"long", position.to_be_bytes().to_vec()),
            ("Mdpn", b"long", 50i32.to_be_bytes().to_vec()),
        ]));
    }
    for variant in ["opaque", "implicit_opaque", "transparent", "unknown", "dither"] {
        let mut definition = vec![
            ("GrdF", b"enum", b"\0\0\0\0GrdF\0\0\0\0CstS".to_vec()),
            ("Intr", b"long", 0i32.to_be_bytes().to_vec()),
            ("Clrs", b"VlLs", colors.clone()),
        ];
        if variant != "implicit_opaque" {
            let mut alpha = 1u32.to_be_bytes().to_vec();
            alpha.extend(b"Objc");
            alpha.extend(descriptor("TrnS", vec![
                ("Opct", b"UntF", unit(b"#Prc", if variant == "transparent" { 50. } else { 100. })),
                ("Lctn", b"long", 0i32.to_be_bytes().to_vec()),
                ("Mdpn", b"long", 50i32.to_be_bytes().to_vec()),
            ]));
            definition.push(("Trns", b"VlLs", alpha));
        }
        let mut fields = vec![
            ("Grad", b"Objc", descriptor("Grdn", definition)),
            ("Angl", b"UntF", unit(b"#Ang", 0.)),
            ("Type", b"enum", b"\0\0\0\0GrdT\0\0\0\0Lnr ".to_vec()),
            ("Rvrs", b"bool", vec![0]),
            ("Dthr", b"bool", vec![u8::from(variant == "dither")]),
            ("Algn", b"bool", vec![1]),
            ("Scl ", b"UntF", unit(b"#Prc", 100.)),
        ];
        if variant == "unknown" { fields.push(("unknownProperty", b"bool", vec![1])); }
        let mut payload = 16u32.to_be_bytes().to_vec();
        payload.extend(descriptor("null", fields));
        for depth in [8, 16] {
            for layout in [Layout::Psd, Layout::Psb] {
                for matches in [true, false] {
                    // Two integer sample positions on a black-to-white ramp: 0 and 1/2.
                    let half = if depth == 16 { 32768 } else { 128 * 257 };
                    let max = if depth == 16 { 65535u64 } else { 255 };
                    let factor = 65535 / max;
                    let alpha = if variant == "transparent" { half } else { 65535 };
                    let matte = |color: u16| {
                        let a = u64::from(alpha) / factor;
                        (((u64::from(color) / factor * a + max * (max - a) + max / 2) / max) * factor) as u16
                    };
                    let first = matte(0) + if matches { 0 } else { 257 };
                    let bytes = procedural_fill_fixture(depth, layout, b"GdFl", &payload, None,
                        [65535; 2], [alpha; 2], [[first; 3], [matte(half); 3]]);
                    for doc in [decode(&bytes).unwrap(), decode_reader(
                        &mut std::io::Cursor::new(&bytes), &Default::default(),
                    ).unwrap()] {
                        assert_eq!(!doc.read_only, matches && matches!(variant, "opaque" | "implicit_opaque" | "transparent"), "{variant}: {:?}", doc.warnings);
                        if doc.read_only { continue; }
                        assert_eq!(doc.bit_depth, depth);
                        let layer = &doc.layers[0];
                        assert_eq!(layer.kind, "fill");
                        assert!(layer.mask.is_some());
                        assert_eq!(layer.effects[0].kind, "gradient_overlay");
                        assert_eq!(layer.effects[0].settings["use_gradient_alpha"], true);
                        assert_eq!(layer.effects[0].settings["color_stops"][1]["color"], serde_json::json!([1., 1., 1.]));
                        let saved = if layout.large() { encode_psb(&doc) } else { encode(&doc) }.unwrap();
                        let report = crate::compatibility::report(&doc).unwrap();
                        assert_eq!(report.layers[0].standard, crate::compatibility::Representation::GradientFill);
                        let index = structural::index(&mut std::io::Cursor::new(&saved), structural::Limits::current(), || Ok(())).unwrap();
                        assert!(index.layers[0].tags.iter().any(|tag| tag.key == *b"GdFl"));
                        let mut header = Cursor::new(&saved);
                        header.bytes(26).unwrap(); header.block().unwrap();
                        let resource_length = header.pos;
                        header.block().unwrap();
                        let mut standard = saved[..resource_length].to_vec();
                        u32b(&mut standard, 0);
                        standard.extend_from_slice(&saved[header.pos..]);
                        let conventional = decode(&standard).unwrap();
                        assert!(!conventional.read_only, "{:?}", conventional.warnings);
                        assert_eq!(conventional.layers[0].kind, "fill");
                        assert_eq!(conventional.layers[0].effects[0].settings["alpha_stops"], layer.effects[0].settings["alpha_stops"]);
                        assert_eq!(conventional.export_png().unwrap(), doc.export_png().unwrap());
                        let downgraded = super::adjustments::tests::replace_sources(&saved, layout, &doc, 45);
                        assert!(decode(&downgraded).unwrap().read_only);
                        let reopened = decode(&saved).unwrap();
                        assert!(!reopened.read_only, "{:?}", reopened.warnings);
                        assert_eq!(reopened.bit_depth, depth);
                        assert_eq!(reopened.layers[0].effects[0].settings, layer.effects[0].settings);
                        assert_eq!(reopened.export_png().unwrap(), doc.export_png().unwrap());
                    }
                }
            }
        }
    }
}

fn solid_color_overlay_style(supported: bool, color: [u16; 3], blend_id: &str) -> Vec<u8> {
    use super::styles::tests::descriptor;
    let rgb = descriptor("RGBC", ["Rd  ", "Grn ", "Bl  "].into_iter().zip(color)
        .map(|(key, value)| (key, b"doub", f64::from(value).to_be_bytes().to_vec())).collect());
    let mut opacity = b"#Prc".to_vec();
    opacity.extend(100f64.to_be_bytes());
    let mut blend = b"\0\0\0\0BlnM".to_vec();
    u32b(&mut blend, if blend_id.len() == 4 { 0 } else { blend_id.len() as u32 });
    blend.extend(blend_id.as_bytes());
    let mut fields = vec![
        ("enab", b"bool", vec![1]),
        ("present", b"bool", vec![1]),
        ("Md  ", b"enum", blend),
        ("Opct", b"UntF", opacity),
        ("Clr ", b"Objc", rgb),
    ];
    if !supported { fields.push(("unknownProperty", b"bool", vec![1])); }
    let mut style = 0u32.to_be_bytes().to_vec();
    style.extend(16u32.to_be_bytes());
    style.extend(descriptor("null", vec![
        ("masterFXSwitch", b"bool", vec![1]),
        ("SoFi", b"Objc", descriptor("SoFi", fields)),
    ]));
    style
}

#[test]
fn photoshop_solid_color_styles_keep_color_effects_and_mask_editable() {
    let color = [80u16, 120, 200];
    for supported in [true, false] {
        let style = solid_color_overlay_style(supported, color, "Nrml");
        for depth in [8, 16] {
            for layout in [Layout::Psd, Layout::Psb] {
                for matches in [true, false] {
                    let bytes = solid_color_fixture(depth, layout, 42., matches, Some((&style, color)));
                    for doc in [decode(&bytes).unwrap(), decode_reader(
                        &mut std::io::Cursor::new(&bytes), &Default::default(),
                    ).unwrap()] {
                        assert_eq!(doc.read_only, !(supported && matches), "{:?}", doc.warnings);
                        if doc.read_only { continue; }
                        let layer = &doc.layers[0];
                        assert_eq!(layer.kind, "fill");
                        assert_eq!(layer.color, [42, 137, 231, 255]);
                        assert_eq!(layer.effects.len(), 1);
                        assert_eq!(layer.effects[0].kind, "color_overlay");
                        assert_eq!(layer.mask.as_ref().unwrap().steps[1].pixels.get16(1, 0)[0], 0);
                        assert_eq!(&doc.preview(None, 32, None, false).unwrap().2[..4], &[80, 120, 200, 255]);
                        let report = crate::compatibility::report(&doc).unwrap();
                        assert_eq!(report.layers[0].standard, crate::compatibility::Representation::SolidColor);
                        assert!(report.losses.iter().all(|loss| loss.code != crate::compatibility::LossCode::EffectsBaked));
                        let output = encode_layout(&doc, layout).unwrap();
                        let reopened = decode(&output).unwrap();
                        assert!(!reopened.read_only, "{:?}", reopened.warnings);
                        assert_eq!(reopened.bit_depth, depth);
                        assert_eq!(reopened.layers[0].kind, "fill");
                        assert_eq!(reopened.layers[0].color, layer.color);
                        assert_eq!(reopened.layers[0].effects[0].settings, layer.effects[0].settings);
                        assert_eq!(reopened.layers[0].mask.as_ref().unwrap().steps[1].pixels.get16(1, 0)[0], 0);
                        assert_eq!(reopened.export_png().unwrap(), doc.export_png().unwrap());
                    }
                }
            }
        }
    }
}

#[test]
fn photoshop_color_overlay_blend_ids_require_an_exact_composite() {
    let base = [42, 137, 231, 255];
    let tint = [198, 65, 107, 255];
    let definition = solid_color_definition(f64::from(base[0]));
    for (id, mode) in [
        ("darkerColor", "darker_color"), ("lighterColor", "lighter_color"),
        ("vividLight", "vivid_light"), ("linearLight", "linear_light"),
        ("pinLight", "pin_light"), ("hardMix", "hard_mix"),
        ("blendSubtraction", "subtract"), ("blendDivide", "divide"),
        ("H   ", "hue"), ("Strt", "saturation"),
        ("Clr ", "color"), ("Lmns", "luminosity"),
    ] {
        let style = solid_color_overlay_style(true, tint[..3].try_into().unwrap(), id);
        for depth in [8, 16] {
            let to_words = |rgba: [u16; 4]| rgba.map(|v| v * 257);
            let rendered = crate::raster::blend16(to_words(base), to_words(tint), 1., mode);
            let rgb: [u16; 3] = rendered[..3].try_into().unwrap();
            let rgb = if depth == 8 {
                rgb.map(|word| u16::from(crate::raster::project16(word)) * 257)
            } else {
                rgb
            };
            for layout in [Layout::Psd, Layout::Psb] {
                for matches in [true, false] {
                    let mut appearance = rgb;
                    if !matches {
                        appearance[0] ^= if depth == 16 { 1 } else { 257 };
                    }
                    let bytes = procedural_fill_fixture(
                        depth, layout, b"SoCo", &definition, Some(&style),
                        [65535, 0], [65535, 0], [appearance, [65535; 3]],
                    );
                    for doc in [decode(&bytes).unwrap(), decode_reader(
                        &mut std::io::Cursor::new(&bytes), &Default::default(),
                    ).unwrap()] {
                        assert_eq!(doc.read_only, !matches, "{id} {depth}: {:?}", doc.warnings);
                        if matches {
                            assert_eq!(doc.layers[0].effects[0].settings["blend"], mode);
                            let reopened = decode(&encode_layout(&doc, layout).unwrap()).unwrap();
                            assert!(!reopened.read_only, "{:?}", reopened.warnings);
                            assert_eq!(reopened.layers[0].effects[0].settings["blend"], mode);
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn resident_solid_fill_color_overlay_writes_base_channels_and_binds_the_style() {
    use crate::compatibility::{LossCode, Representation};
    let base = [47, 103, 211, 255];
    let full = [185, 69, 127].map(|v| f64::from(v) / 255.);
    let partial = [0.1875, 0.625, 0.90625];
    for (depth, opacity, color, blend, enabled, linked, format) in [
        (8, 1., full, "normal", true, None, COLOR_OVERLAY_FORMAT),
        (16, 1., full, "normal", true, None, COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, None, PRECISE_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, None, PRECISE_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, None, BLENDED_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, None, BLENDED_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, Some(true), MASKED_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, Some(true), MASKED_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", false, None, COLOR_OVERLAY_STATE_FORMAT),
        (16, 0.625, partial, "vivid_light", true, Some(false), COLOR_OVERLAY_STATE_FORMAT),
        (8, 0.625, partial, "multiply", true, None, COLOR_OVERLAY_LAYER_FORMAT),
        (16, 0.625, partial, "vivid_light", true, Some(true), COLOR_OVERLAY_LAYER_FORMAT),
        (8, 0.625, partial, "multiply", true, None, RASTER_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, None, RASTER_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, Some(false), MULTI_COLOR_OVERLAY_FORMAT),
        (16, 0.625, partial, "vivid_light", true, None, MULTI_COLOR_OVERLAY_FORMAT),
        (8, 0.625, partial, "multiply", true, None, HARD_SHADOW_FORMAT),
        (16, 0.625, partial, "normal", true, None, HARD_SHADOW_FORMAT),
        (8, 0.625, partial, "multiply", true, None, SHADOW_STACK_FORMAT),
        (16, 0.625, partial, "normal", true, None, SHADOW_STACK_FORMAT),
        (8, 0.625, partial, "normal", true, None, STROKE_STYLE_FORMAT),
        (16, 0.625, partial, "normal", true, None, STROKE_STYLE_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), GRADIENT_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), GRADIENT_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), PATTERN_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), PATTERN_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), MULTI_GRADIENT_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), MULTI_GRADIENT_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, Some(true), PLACED_PATTERN_OVERLAY_FORMAT),
        (16, 0.625, partial, "normal", true, Some(false), PLACED_PATTERN_OVERLAY_FORMAT),
        (8, 0.625, partial, "normal", true, None, MULTI_INNER_SHADOW_FORMAT),
        (16, 0.625, partial, "normal", true, None, MULTI_INNER_SHADOW_FORMAT),
        (8, 0.625, partial, "normal", true, None, MULTI_INSIDE_STROKE_FORMAT),
        (16, 0.625, partial, "normal", true, None, MULTI_INSIDE_STROKE_FORMAT),
    ] {
        let mut doc = Document::new_depth(2, 1, depth).unwrap();
        doc.layers[0].kind = "fill".into();
        doc.layers[0].color = base;
        let shadow_stack = format == SHADOW_STACK_FORMAT;
        let inner_stack = format == MULTI_INNER_SHADOW_FORMAT;
        let stroke_stack = format == MULTI_INSIDE_STROKE_FORMAT;
        let stroke = format == STROKE_STYLE_FORMAT;
        let gradient = format == GRADIENT_OVERLAY_FORMAT;
        let placed = format == PLACED_PATTERN_OVERLAY_FORMAT;
        let pattern = placed || format == PATTERN_OVERLAY_FORMAT;
        let gradients = format == MULTI_GRADIENT_OVERLAY_FORMAT;
        let has_pattern = pattern || gradients && depth == 16;
        let multi = format == MULTI_COLOR_OVERLAY_FORMAT || (shadow_stack || stroke || gradient || pattern) && depth == 16;
        let shadow = format == HARD_SHADOW_FORMAT;
        let raster = stroke || stroke_stack || shadow || shadow_stack || inner_stack || gradient || pattern || gradients || format == RASTER_COLOR_OVERLAY_FORMAT || multi && depth == 16;
        if raster {
            doc.layers[0].kind = "paint".into();
            doc.layers[0].x = -1;
            doc.layers[0].pixels = styles::tests::color_overlay_raster(depth);
            if stroke || stroke_stack {
                let mut pixel = doc.layers[0].pixels.get16(1, 0);
                pixel[3] = 65535;
                doc.layers[0].pixels.set16(1, 0, pixel);
            }
        }
        doc.layers[0].mask = linked.map(|linked| {
            let mut mask = styles::tests::color_overlay_mask(depth);
            mask.linked = linked;
            mask
        });
        doc.layers[0].effects.push(crate::effects::Effect {
            id: crate::engine::id(), kind: "color_overlay".into(), enabled, weight: 1.,
            settings: serde_json::json!({"opacity":opacity, "blend":blend, "color":color}).into(),
        });
        if multi { styles::tests::extend_color_overlay_stack(&mut doc.layers[0]); }
        if shadow { doc.layers[0].effects[0] = styles::tests::standard_shadow_fixture(depth); }
        if shadow_stack {
            doc.layers[0].effects.extend([styles::tests::standard_shadow_fixture(8), styles::tests::standard_shadow_fixture(16)]);
        }
        if inner_stack {
            doc.layers[0].effects = styles::tests::standard_inner_shadow_stack().into();
        }
        if stroke_stack {
            doc.layers[0].effects = styles::tests::standard_inside_stroke_stack().into();
        }
        if stroke {
            use styles::tests::{standard_shadow_fixture, standard_stroke_fixture};
            if depth == 8 { doc.layers[0].effects[0] = standard_stroke_fixture("outside"); }
            else { doc.layers[0].effects.extend([standard_shadow_fixture(8), standard_stroke_fixture("inside"), standard_shadow_fixture(16)]); }
        }
        if gradient {
            let mut overlay = styles::tests::standard_gradient_overlay_fixture(depth);
            if depth == 16 {
                std::sync::Arc::make_mut(&mut overlay.settings)["align_layer"] = serde_json::json!(true);
            }
            if multi { doc.layers[0].effects.insert(0, overlay); }
            else { doc.layers[0].effects[0] = overlay; }
        }
        if pattern {
            let mut overlay = styles::tests::standard_pattern_overlay_fixture(depth);
            if placed {
                let settings = std::sync::Arc::make_mut(&mut overlay.settings);
                settings["phase_x"] = serde_json::json!(3.);
                settings["phase_y"] = serde_json::json!(-2.);
                settings["bitmap_angle"] = serde_json::json!(if depth == 16 { -90. } else { 0. });
            }
            if multi {
                doc.layers[0].effects.insert(0, styles::tests::standard_gradient_overlay_fixture(depth));
                doc.layers[0].effects.insert(0, overlay);
            } else {
                doc.layers[0].effects[0] = overlay;
            }
        }
        if gradients {
            let stack = styles::tests::standard_gradient_stack_fixture(depth);
            if has_pattern {
                let pattern = styles::tests::standard_pattern_overlay_fixture(depth);
                drop(doc.layers[0].effects.splice(0..0, std::iter::once(pattern).chain(stack)));
            } else {
                doc.layers[0].effects = stack.into();
            }
        }
        if format == COLOR_OVERLAY_LAYER_FORMAT {
            doc.layers[0].opacity = 101. / 255.;
            if depth == 16 {
                doc.layers[0].blend = "multiply".into();
                doc.layers[0].psd_metadata.push(LayerMetadata { key: "infx".into(), data: vec![1, 0, 0, 0] });
            }
            let mut backdrop = Layer::new("Backdrop", "fill", 2, 1);
            backdrop.pixels = Raster::new_depth(2, 1, depth);
            backdrop.color = [17, 204, 71, 255];
            doc.layers.push(backdrop);
        }
        let report = crate::compatibility::report(&doc).unwrap();
        assert_eq!(report.layers[0].standard, if raster { Representation::Raster } else { Representation::SolidColor });
        if raster { assert_eq!(report.layers[0].standard_bounds, Some([-1, 0, 2, 1])); }
        assert!(report.losses.iter().all(|loss| loss.code != LossCode::EffectsBaked));
        for layout in [Layout::Psd, Layout::Psb] {
            let mut bytes = encode_layout(&doc, layout).unwrap();
            let index = structural::index_for_decode(&mut std::io::Cursor::new(&bytes), || Ok(())).unwrap();
            assert_eq!(index.global_tags.iter().filter(|tag| tag.key == *b"Patt").count(), usize::from(has_pattern));
            let record = index.layers.last().unwrap();
            assert_eq!(record.tags.iter().any(|tag| tag.key == *b"SoCo"), !raster);
            let style_key = if multi || gradients || inner_stack || stroke_stack { *b"lmfx" } else { *b"lfx2" };
            let style = record.tags.iter().find(|tag| tag.key == style_key).unwrap();
            assert_eq!(record.tags.iter().filter(|tag| matches!(&tag.key, b"lfx2" | b"lmfx")).count(), 1);
            let mask_mode = record.tags.iter().find(|tag| tag.key == *b"lmgm");
            assert_eq!(mask_mode.is_some(), linked == Some(false));
            assert_eq!(record.channels.iter().any(|channel| channel.id == -2), doc.layers[0].mask.is_some());
            let raw = record.channels.iter().find(|channel| channel.id == 0).unwrap().span.offset as usize;
            let expected = if raster { doc.layers[0].pixels.get16(0, 0)[0] } else { u16::from(base[0]) * 257 }.to_be_bytes();
            assert_eq!(&bytes[raw..raw + 2], &[0, 0]);
            assert_eq!(&bytes[raw + 2..raw + 2 + usize::from(depth / 8)], &expected[..usize::from(depth / 8)]);

            let mut file = Cursor::new(&bytes);
            file.bytes(26).unwrap();
            file.block().unwrap();
            let resource_start = file.pos;
            file.block().unwrap();
            let mut standard = bytes[..resource_start].to_vec();
            block(&mut standard, &[]);
            standard.extend_from_slice(&bytes[file.pos..]);
            let section_start = file.pos;
            let section = layout.block(&mut file).unwrap();
            assert!(metadata_sources_match(section, layout, &doc, format,
                None, &Default::default()).unwrap());
            if multi || shadow_stack || gradients || inner_stack || stroke_stack {
                let mut reordered = doc.clone();
                let start = usize::from(gradients && has_pattern);
                reordered.layers[0].effects.swap(start, start + 2);
                assert!(!metadata_sources_match(section, layout, &reordered, format,
                    None, &Default::default()).unwrap());
            }
            if format > COLOR_OVERLAY_FORMAT {
                assert!(!metadata_sources_match(section, layout, &doc, format - 1,
                    None, &Default::default()).unwrap());
            }
            for candidate in [&bytes, &standard] {
                for opened in [decode(candidate).unwrap(), decode_reader(
                    &mut std::io::Cursor::new(candidate), &Default::default(),
                ).unwrap()] {
                    assert!(!opened.read_only, "{:?}", opened.warnings);
                    if raster {
                        assert_eq!(opened.layers[0].x, -1);
                        for x in 0..3 {
                            assert_eq!(opened.layers[0].pixels.get16(x, 0), doc.layers[0].pixels.get16(x, 0));
                        }
                    } else {
                        assert_eq!(opened.layers[0].color, base);
                    }
                    assert_eq!(opened.layers[0].opacity, doc.layers[0].opacity);
                    assert_eq!(opened.layers[0].blend, doc.layers[0].blend);
                    assert_eq!(opened.layers[0].effects.len(), doc.layers[0].effects.len());
                    for (actual, expected) in opened.layers[0].effects.iter().zip(&doc.layers[0].effects) {
                        assert_eq!(actual.kind, expected.kind);
                        assert_eq!(actual.settings, expected.settings);
                        assert_eq!(actual.enabled, expected.enabled);
                    }
                    assert_eq!(opened.layers[0].mask.as_ref().map(|mask| mask.linked), linked);
                    assert_eq!(opened.export_png().unwrap(), doc.export_png().unwrap());
                }
            }
            let section_end = file.pos;
            if let Some(mode) = mask_mode {
                let offset = mode.data.offset as usize;
                assert_eq!(&bytes[offset..offset + 4], &[0; 4]);
                bytes[offset] = 1;
                let changed = layout.block(&mut Cursor::new(&bytes[section_start..section_end])).unwrap();
                assert!(!metadata_sources_match(changed, layout, &doc, format,
                    None, &Default::default()).unwrap());
                bytes[offset] = 0;
            }
            if let Some(grouped) = record.tags.iter().find(|tag| tag.key == *b"infx") {
                let offset = grouped.data.offset as usize;
                assert_eq!(&bytes[offset..offset + 4], &[1, 0, 0, 0]);
                bytes[offset] = 0;
                let changed = layout.block(&mut Cursor::new(&bytes[section_start..section_end])).unwrap();
                assert!(!metadata_sources_match(changed, layout, &doc, format,
                    None, &Default::default()).unwrap());
                bytes[offset] = 1;
            }
            bytes[(style.data.offset + style.data.bytes - 1) as usize] ^= 1;
            let changed = layout.block(&mut Cursor::new(&bytes[section_start..section_end])).unwrap();
            assert!(!metadata_sources_match(changed, layout, &doc, format,
                None, &Default::default()).unwrap());
            assert!(decode(&bytes).unwrap().read_only);
        }
        if stroke {
            if depth == 8 { doc.layers[0].effects.push(styles::tests::standard_shadow_fixture(16)); }
            else { std::sync::Arc::make_mut(&mut doc.layers[0].effects[4].settings)["position"] = serde_json::json!("outside"); }
        } else if shadow_stack {
            doc.layers[0].effects.swap(0, 2);
        } else if shadow {
            std::sync::Arc::make_mut(&mut doc.layers[0].effects[0].settings)["size"] = serde_json::json!(5.);
        } else if gradient {
            let settings = std::sync::Arc::make_mut(&mut doc.layers[0].effects[0].settings);
            settings["color_stops"][1]["midpoint"] = serde_json::json!(0.625);
        } else if pattern {
            std::sync::Arc::make_mut(&mut doc.layers[0].effects[0].settings)["phase_x"] = serde_json::json!(0.5);
        } else if gradients {
            doc.layers[0].effects[usize::from(has_pattern)].weight = 0.5;
        } else if format == COLOR_OVERLAY_LAYER_FORMAT {
            if let Some(grouped) = doc.layers[0].psd_metadata.iter_mut().find(|tag| tag.key == "infx") {
                grouped.data[0] = 0;
            } else {
                doc.layers[0].opacity = 0.5;
            }
        } else {
            doc.layers[0].effects[0].weight = 0.5;
        }
        let fallback = encode_layout(&doc, Layout::Psd).unwrap();
        let index = structural::index_for_decode(&mut std::io::Cursor::new(&fallback), || Ok(())).unwrap();
        assert!(index.global_tags.iter().all(|tag| tag.key != *b"Patt"));
        assert!(index.layers.last().unwrap().tags.iter().all(|tag| !matches!(&tag.key, b"lfx2" | b"lmfx" | b"SoCo")));
        assert!(!decode(&fallback).unwrap().read_only);
    }
}

#[test]
fn older_baked_gradient_stacks_do_not_require_new_style_assets() {
    let mut doc = Document::new(2, 1).unwrap();
    let mut effects = vec![styles::tests::standard_pattern_overlay_fixture(8)];
    effects.extend(styles::tests::standard_gradient_stack_fixture(8));
    for effect in &mut effects {
        effect.enabled = false;
    }
    doc.layers[0].effects = effects;
    let mut legacy = doc.clone();
    legacy.layers[0].effects[1].weight = 0.5;
    for layout in [Layout::Psd, Layout::Psb] {
        let bytes = encode_layout(&legacy, layout).unwrap();
        let old = adjustments::tests::replace_sources(&bytes, layout, &doc, PATTERN_OVERLAY_FORMAT);
        let opened = decode(&old).unwrap();
        assert!(!opened.read_only, "{:?}", opened.warnings);
        assert_eq!(opened.layers[0].effects.len(), 4);
        assert_eq!(opened.layers[0].effects[1].weight, 1.);
        let forged = adjustments::tests::replace_sources(&bytes, layout, &doc, MULTI_GRADIENT_OVERLAY_FORMAT);
        assert!(decode(&forged).unwrap().read_only);
    }
}

#[test]
fn native_style_and_working_profile_admission_exclude_encoded_file_size() {
    use crate::engine::native::worker::tests::Fixture;
    use std::io::{Seek, SeekFrom, Write};

    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let fragment = session.fresh_fragment().unwrap();
    let check = || session.check();
    let path = fixture.path().join("Styled.psb");
    let color = [80, 120, 200];
    let style = solid_color_overlay_style(true, color, "Nrml");
    let bytes = solid_color_fixture(16, Layout::Psb, 42., true, Some((&style, color)));
    let profile = crate::color_profile::srgb_profile();
    let mut resources = b"8BIM\x04\x0f\0\0".to_vec();
    block(&mut resources, profile);
    if profile.len() % 2 != 0 { resources.push(0); }
    let skipped = crate::file_safety::RESIDENT_FILE_BYTES as u32;
    resources.extend_from_slice(b"8BIM\x10\x04\0\0");
    u32b(&mut resources, skipped);
    {
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(&bytes[..30]).unwrap(); // Header and empty color-mode data.
        file.write_all(&(resources.len() as u32 + skipped).to_be_bytes()).unwrap();
        file.write_all(&resources).unwrap();
        file.seek(SeekFrom::Current(i64::from(skipped))).unwrap();
        file.write_all(&bytes[34..]).unwrap(); // Skip the original empty resource block.
    }
    let mut file = crate::file_safety::open_native_read(&path).unwrap();
    let version = crate::file_safety::native_source_version(&path, &mut file, &mut [0; 65536], &check).unwrap();
    assert!(version.bytes > crate::file_safety::RESIDENT_FILE_BYTES);
    let control = crate::loading::Control::default();
    let decoded = decode_backed(&mut file, &control, version, fragment.store().clone(), &check).unwrap();
    let native = crate::engine::scene::stage_decoded(&decoded.document, Some(&decoded), &fragment, &session, &check).unwrap();
    assert!(native.metadata().srgb_tagged());
    assert_eq!(native.metadata().layer(0).unwrap().effects().next().unwrap().kind(), "color_overlay");
    let engine = crate::engine::Engine::from_native(native, 8).unwrap();
    let scene = engine.capture_scene(None).unwrap().into_native().unwrap();
    assert!(decoded.composite.matches_native(&mut file, &scene, &session, &control, &check).unwrap());
    drop((engine, scene, decoded, file));
    session.abort_fragment(fragment, &mut || session.cleanup_check()).unwrap();
}

#[test]
fn photoshop_solid_color_keeps_editable_color_mask_and_original_depth() {
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            for (red, matches, editable) in [(42., true, true), (42., false, false), (42.5, true, false)] {
                let bytes = solid_color_fixture(depth, layout, red, matches, None);
                for doc in [decode(&bytes).unwrap(),
                    decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap()] {
                    assert_eq!(doc.bit_depth, depth);
                    assert_eq!(!doc.read_only, editable, "{:?}", doc.warnings);
                    if editable {
                        let layer = &doc.layers[0];
                        assert_eq!(layer.kind, "fill");
                        assert_eq!(layer.color, [42, 137, 231, 255]);
                        assert_eq!([layer.pixels.width, layer.pixels.height], [2, 1]);
                        assert_eq!(layer.mask.as_ref().unwrap().steps[1].pixels.get16(1, 0)[0], 0);
                        let report = crate::compatibility::report(&doc).unwrap();
                        assert_eq!(report.layers[0].standard, crate::compatibility::Representation::SolidColor);
                        assert!(!report.losses.iter().any(|loss| loss.message.contains("Solid Color")));
                        let mut output = encode_layout(&doc, layout).unwrap();
                        let reopened = decode(&output).unwrap();
                        assert_eq!(reopened.layers[0].kind, "fill");
                        assert_eq!(reopened.layers[0].color, layer.color);
                        let index = structural::index(
                            &mut std::io::Cursor::new(&output), structural::Limits::current(), || Ok(()),
                        ).unwrap();
                        let solid = index.layers[0].tags.iter().find(|tag| tag.key == *b"SoCo").unwrap();
                        let first_channel = solid.data.offset as usize + solid.data.bytes as usize - 48;
                        assert_eq!(&output[first_channel..first_channel + 8], &42f64.to_be_bytes());
                        let mut file = Cursor::new(&output);
                        file.bytes(26).unwrap();
                        file.block().unwrap();
                        let resource_length = file.pos;
                        file.block().unwrap();
                        let mut standard = output[..resource_length].to_vec();
                        u32b(&mut standard, 0);
                        standard.extend_from_slice(&output[file.pos..]);
                        let outside = decode(&standard).unwrap();
                        assert!(!outside.read_only, "{:?}", outside.warnings);
                        assert_eq!(outside.layers[0].kind, "fill");
                        assert_eq!(outside.layers[0].color, layer.color);
                        assert_eq!(outside.layers[0].pixels.get16(1, 0), [42 * 257, 137 * 257, 231 * 257, 65535]);
                        output[first_channel..first_channel + 8].copy_from_slice(&43f64.to_be_bytes());
                        let forged = super::adjustments::tests::replace_sources(&output, layout, &doc, 41);
                        assert!(decode(&forged).unwrap().read_only);

                        let mut old = doc.clone();
                        old.layers[0].kind = "paint".into();
                        for x in 0..2 {
                            old.layers[0].pixels.set16(x, 0, layer.color.map(|c| u16::from(c) * 257));
                        }
                        let old = super::adjustments::tests::replace_sources(
                            &encode_layout(&old, layout).unwrap(), layout, &doc, 29,
                        );
                        let old = decode(&old).unwrap();
                        assert!(!old.read_only, "{:?}", old.warnings);
                        assert_eq!(old.layers[0].kind, "fill");
                        assert_eq!(old.layers[0].color, layer.color);

                        let mut fallback = doc.clone();
                        fallback.layers[0].color[3] = 254;
                        let report = crate::compatibility::report(&fallback).unwrap();
                        assert_eq!(report.layers[0].standard, crate::compatibility::Representation::Raster);
                        assert!(report.losses.iter().any(|loss| loss.message.contains("Solid Color")));
                    } else {
                        assert!(doc.layers.iter().all(|layer| layer.kind != "fill"));
                    }
                }
            }
        }
    }
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
                    let streamed =
                        decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default())
                            .unwrap();
                    assert_eq!(streamed.read_only, doc.read_only);
                    assert_eq!(streamed.warnings, doc.warnings);
                    assert_eq!(streamed.layers[0].pixels, doc.layers[0].pixels);
                    let streamed_mask = streamed.layers[0].mask.as_ref().unwrap();
                    assert_eq!(
                        (streamed_mask.enabled, streamed_mask.linked),
                        (enabled, true)
                    );
                    assert_eq!(streamed_mask.steps[0].value, 127.);
                    assert_eq!(
                        streamed_mask.steps[1].pixels,
                        doc.layers[0].mask.as_ref().unwrap().steps[1].pixels
                    );
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
                    let full = mask.steps[1].pixels.retained.as_ref().unwrap();
                    assert_eq!(full.matrix, [1., 0., 0., 1., -249., -1.]);
                    let off_frame = words[513 + 246];
                    assert_eq!(full.pixels.get16(246, 1), [off_frame, off_frame, off_frame, 65535]);
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
fn linked_extended_mask_reopens_from_standard_rows_and_reads_older_private_layout() {
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            let (input, words) = fixture(depth, 0, layout, true, 0);
            let doc = decode(&input).unwrap();
            assert!(!doc.read_only, "{:?}", doc.warnings);
            let saved = encode_layout(&doc, layout).unwrap();
            let index = structural::index(
                &mut std::io::Cursor::new(&saved),
                structural::Limits::current(),
                || Ok(()),
            ).unwrap();
            let record = &index.layers[0];
            let at = record.mask.offset as usize;
            assert_eq!(&saved[at..at + 16], &[-1i32, -249, 3, 269].map(i32::to_be_bytes).concat());
            assert_eq!(&saved[at + 16..at + 20], &[127, 0, 0, 0]);
            assert!(!record.tags.iter().any(|tag| tag.key == *b"lmgm"));

            let mut cursor = Cursor::new(&saved);
            cursor.bytes(26).unwrap();
            cursor.block().unwrap();
            let start = cursor.pos;
            cursor.block().unwrap();
            let mut standard = saved[..start].to_vec();
            u32b(&mut standard, 0);
            standard.extend_from_slice(&saved[cursor.pos..]);
            let standard = decode(&standard).unwrap();
            assert!(!standard.read_only, "{:?}", standard.warnings);
            let mask = standard.layers[0].mask.as_ref().unwrap();
            assert!(mask.linked);
            let word = words[513 + 246];
            let original = mask.steps[1].pixels.retained.as_ref().unwrap();
            assert_eq!(original.pixels.get16(246, 1), [word, word, word, 65535]);
            assert_eq!(standard.export_png().unwrap(), doc.export_png().unwrap());

            let downgraded = adjustments::tests::replace_sources(&saved, layout, &doc, 42);
            assert!(decode(&downgraded).unwrap().read_only);
            let mut legacy = doc.clone();
            legacy.layers[0].mask.as_mut().unwrap().steps[1].pixels.retained = None;
            let legacy = encode_layout(&legacy, layout).unwrap();
            let legacy = adjustments::tests::replace_sources(&legacy, layout, &doc, 42);
            let legacy = decode(&legacy).unwrap();
            assert!(!legacy.read_only, "{:?}", legacy.warnings);
            let mask = legacy.layers[0].mask.as_ref().unwrap();
            assert!(mask.enabled && mask.linked);
            assert_eq!(mask.steps[1].pixels, doc.layers[0].mask.as_ref().unwrap().steps[1].pixels);
        }
    }
}

#[test]
fn standard_mask_link_round_trips_at_native_depth_and_protects_active_effects_mode() {
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            for enabled in [false, true] {
                let mut doc = Document::new_depth(12, 9, depth).unwrap();
                let layer = &mut doc.layers[0];
                layer.x = 3;
                layer.y = 2;
                layer.pixels = Raster::new_depth(5, 4, depth);
                for y in 0..4 {
                    for x in 0..5 {
                        layer.pixels.set16(x, y, [65535; 4]);
                    }
                }
                let word = if depth == 16 { 12347 } else { 12336 };
                let mut pixels = Raster::new_depth(5, 4, depth);
                pixels.set16(1, 2, [word, word, word, 65535]);
                layer.mask = Some(Mask {
                    enabled,
                    linked: false,
                    cache_key: id(),
                    steps: vec![MaskStep {
                        id: id(),
                        kind: "paint".into(),
                        enabled: true,
                        value: 0.,
                        weight: 1.,
                        settings: serde_json::Value::Null,
                        pixels,
                    }],
                });
                let saved = encode_layout(&doc, layout).unwrap();
                let reopened = decode(&saved).unwrap();
                assert!(!reopened.read_only);
                assert!(!reopened.layers[0].mask.as_ref().unwrap().linked);

                let mut cursor = Cursor::new(&saved);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let mut standard = saved[..start].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&saved[cursor.pos..]);
                let index = structural::index(
                    &mut std::io::Cursor::new(&standard),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let record = &index.layers[0];
                let at = record.mask.offset as usize;
                assert_eq!(
                    &standard[at..at + 16],
                    &[2i32, 3, 6, 8].map(i32::to_be_bytes).concat()
                );
                assert_eq!(standard[at + 17], standard_mask_flags(enabled, false));
                let mode = record.tags.iter().find(|t| &t.key == b"lmgm").unwrap();
                assert_eq!(mode.data.bytes, 4);
                let decoded =
                    decode_reader(&mut std::io::Cursor::new(&standard), &Default::default())
                        .unwrap();
                assert!(!decoded.read_only, "{:?}", decoded.warnings);
                assert_eq!([decoded.layers[0].x, decoded.layers[0].y], [3, 2]);
                let mask = decoded.layers[0].mask.as_ref().unwrap();
                assert_eq!((mask.enabled, mask.linked), (enabled, false));
                assert_eq!(mask.steps[1].pixels.get16(1, 2), [word, word, word, 65535]);

                let mut old_source = doc.clone();
                old_source.layers[0].mask.as_mut().unwrap().linked = true;
                let legacy = encode_layout(&old_source, layout).unwrap();
                let legacy = adjustments::tests::replace_sources(&legacy, layout, &doc, 29);
                let restored = decode(&legacy).unwrap();
                assert!(!restored.read_only);
                assert!(!restored.layers[0].mask.as_ref().unwrap().linked);

                let wrong = adjustments::tests::replace_sources(&saved, layout, &old_source, 38);
                assert!(decode(&wrong).unwrap().read_only);
                standard[mode.data.offset as usize] = 1;
                let protected = decode(&standard).unwrap();
                assert!(protected.read_only);
                assert!(protected.warnings.iter().any(|w| w.contains("lmgm")));
            }
        }
    }
}

#[test]
fn unlinked_mask_keeps_off_frame_samples_available_to_later_layer_moves() {
    for depth in [8, 16] {
        for layout in [Layout::Psd, Layout::Psb] {
            for compression in 0..4 {
                let (bytes, words) = fixture(depth, compression, layout, true, MASK_UNLINKED);
                let source = decode(&bytes).unwrap();
                let streamed =
                    decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap();
                assert!(!source.read_only, "{:?}", source.warnings);
                assert!(!streamed.read_only, "{:?}", streamed.warnings);
                let mask = source.layers[0].mask.as_ref().unwrap();
                let pixels = &mask.steps[1].pixels;
                assert!(!mask.linked);
                assert_eq!(
                    *pixels,
                    streamed.layers[0].mask.as_ref().unwrap().steps[1].pixels
                );
                let original = pixels.retained.as_ref().unwrap();
                assert_eq!((original.pixels.width, original.pixels.height), (513, 4));
                assert_eq!(original.matrix, [1., 0., 0., 1., -249., -1.]);
                let word = words[513 + 246];
                assert_eq!(original.pixels.get16(246, 1), [word, word, word, 65535]);
                assert_eq!(
                    (
                        source.layers[0].pixels.width,
                        source.layers[0].pixels.height
                    ),
                    (269, 3)
                );

                let saved = encode_layout(&source, layout).unwrap();
                let reopened = decode(&saved).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(
                    *pixels,
                    reopened.layers[0].mask.as_ref().unwrap().steps[1].pixels
                );
                let mut cursor = Cursor::new(&saved);
                cursor.bytes(26).unwrap();
                cursor.block().unwrap();
                let start = cursor.pos;
                cursor.block().unwrap();
                let mut standard = saved[..start].to_vec();
                u32b(&mut standard, 0);
                standard.extend_from_slice(&saved[cursor.pos..]);
                let indexed = structural::index(
                    &mut std::io::Cursor::new(&standard),
                    structural::Limits::current(),
                    || Ok(()),
                )
                .unwrap();
                let at = indexed.layers[0].mask.offset as usize;
                assert_eq!(
                    &standard[at..at + 16],
                    &[-1i32, -249, 3, 269].map(i32::to_be_bytes).concat()
                );
                assert_eq!(standard[at + 16], 127);
                let conventional = decode(&standard).unwrap();
                assert!(!conventional.read_only, "{:?}", conventional.warnings);
                let conventional_mask =
                    &conventional.layers[0].mask.as_ref().unwrap().steps[1].pixels;
                let external_original = conventional_mask.retained.as_ref().unwrap();
                assert_eq!(
                    external_original.pixels.get16(246, 1),
                    [word, word, word, 65535]
                );
                assert_eq!(
                    conventional.export_png().unwrap(),
                    reopened.export_png().unwrap()
                );
                let layer = reopened.layers[0].id.clone();
                let mut engine = crate::engine::Engine::new();
                engine.resident_state_mut().unwrap().document = reopened;
                engine.edit(
                    "human",
                    &[serde_json::json!({"op":"move","layer":layer,"dx":-3,"dy":0,"selection_only":false})],
                    None,
                    None,
                    "Move color across unlinked mask",
                ).unwrap();
                let moved = &engine.resident_state().unwrap().document.layers[0];
                assert_eq!(moved.x, -3);
                assert_eq!(
                    moved.mask.as_ref().unwrap().steps[1].pixels.get16(0, 0),
                    [word, word, word, 65535]
                );
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
fn indexed_native16_offcanvas_masks_beyond_retained_limits_preserve_the_saved_composite() {
    for layout in [Layout::Psd, Layout::Psb] {
        let (original, _) = fixture(16, 0, layout, false, 0);
        let index = structural::index(
            &mut std::io::Cursor::new(&original),
            structural::Limits::current(),
            || Ok(()),
        )
        .unwrap();
        let layer = &index.layers[0];
        let words = values(9000, 1, 16);
        let replacement = channel(&words, 9000, 1, 16, 0, layout);
        let mut record =
            original[layer.record.offset as usize..layer.record.end().unwrap() as usize].to_vec();
        let length_at = 18 + 4 * if layout.large() { 10 } else { 6 } + 2;
        let mut new_length = vec![];
        layout
            .write_length(&mut new_length, replacement.len() as u64)
            .unwrap();
        record[length_at..length_at + new_length.len()].copy_from_slice(&new_length);
        let mask_at = (layer.mask.offset - layer.record.offset) as usize;
        for (i, value) in [0i32, -8000, 1, 1000].into_iter().enumerate() {
            record[mask_at + i * 4..mask_at + i * 4 + 4].copy_from_slice(&value.to_be_bytes());
        }
        let mut info = (-1i16).to_be_bytes().to_vec();
        info.extend(record);
        for c in &layer.channels {
            if c.id == -2 {
                info.extend_from_slice(&replacement);
            } else {
                info.extend_from_slice(
                    &original[c.span.offset as usize..c.span.end().unwrap() as usize],
                );
            }
        }
        if info.len() % 2 != 0 {
            info.push(0);
        }
        let mut section = vec![];
        layout.write_length(&mut section, 0).unwrap();
        u32b(&mut section, 0);
        layout_tag(&mut section, b"Lr16", &info, layout);
        layout_tag(&mut section, b"Mt16", &[], layout);
        let mut bytes = original[..34].to_vec();
        layout
            .write_length(&mut bytes, section.len() as u64)
            .unwrap();
        bytes.extend(section);
        bytes.extend_from_slice(&original[index.composite.span.offset as usize..]);
        assert_eq!(crate::raster::MAX_EDGE, 8192);
        let index = structural::index(
            &mut std::io::Cursor::new(&bytes),
            structural::Limits::current(),
            || Ok(()),
        )
        .unwrap();
        let flags = index.layers[0].mask.offset as usize + 17;
        for linked in [true, false] {
            bytes[flags] = standard_mask_flags(false, linked);
            for doc in [
                decode(&bytes).unwrap(),
                decode_reader(&mut std::io::Cursor::new(&bytes), &Default::default()).unwrap(),
            ] {
                assert!(doc.read_only);
                assert!(doc.warnings.iter().any(|w| w.contains("retained-source limits")));
                assert_eq!(doc.layers[0].pixels.get16(0, 0), [65535; 4]);
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
