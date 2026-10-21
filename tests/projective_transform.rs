use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    psd,
    raster::Raster,
    selection, server, transform,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn edit(e: &mut Engine, commands: &[Value]) {
    e.edit(
        "human",
        commands,
        Some(e.doc.revision),
        None,
        "Projective transform",
    )
    .unwrap();
}
fn state(e: &Engine) -> Value {
    let history = |items: &[peerbrush::engine::History]| {
        items
            .iter()
            .map(|h| {
                json!({
        "before":h.before,"after":h.after,"actor":h.actor,"label":h.label,"task":h.task,
        "scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted})
            })
            .collect::<Vec<_>>()
    };
    json!({"doc":e.doc,"undo":history(&e.undo),"redo":history(&e.redo)})
}
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(64, 48, depth).unwrap();
    let id = e.doc.layers[0].id.clone();
    e.doc.layers[0].pixels = Raster::new_depth(16, 12, depth);
    edit(&mut e, &[json!({"op":"mask.add","layer":id})]);
    let l = &mut e.doc.layers[0];
    for y in 0..12 {
        for x in 0..16 {
            l.pixels.set16(
                x,
                y,
                [
                    12347 + x as u16 * 113,
                    23459 + y as u16 * 211,
                    34571,
                    if (x, y) == (15, 11) || (x, y) == (5, 5) {
                        0
                    } else {
                        50003
                    },
                ],
            );
            let v = 13001 + x as u16 * 193 + y as u16 * 97;
            l.mask.as_mut().unwrap().steps[1]
                .pixels
                .set16(x, y, [v, v, v, 65535]);
        }
    }
    l.mask.as_mut().unwrap().linked = true;
    e.undo.clear();
    e
}
fn command(e: &Engine, fields: Value) -> Value {
    // Independent analytic P(x,y)=((x+24)/(1+x/64),(y+3)/(1+x/64)).
    let mut c = json!({"op":"transform","layer":e.doc.layers[0].id,"selection_only":false,
        "projective":{"rect":[0.,0.,16.,12.],"quad":[[24.,3.],[32.,2.4],[32.,12.],[24.,15.]]}});
    c.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    c
}
fn inverse(x: f64, y: f64) -> [f64; 2] {
    let u = (x - 24.) / (1. - x / 64.);
    [u, y * (1. + u / 64.) - 3.]
}
fn color(l: &Layer, x: i32, y: i32) -> [u16; 4] {
    l.pixels.get16(x - l.x, y - l.y)
}
fn paint(l: &Layer, x: i32, y: i32) -> [u16; 4] {
    l.mask.as_ref().unwrap().steps[1]
        .pixels
        .get16(x - l.x, y - l.y)
}
fn sample(r: &Raster, x: f32, y: f32) -> [u16; 4] {
    if x.fract() == 0. && y.fract() == 0. {
        return r.get16(x as i32, y as i32);
    }
    if r.depth == 16 {
        r.sample16(x, y)
    } else {
        r.sample(x, y).map(|v| u16::from(v) * 257)
    }
}
fn native_layers(a: &Document, b: &Document) {
    assert_eq!(a.layers.len(), b.layers.len());
    for (a, b) in a.layers.iter().zip(&b.layers) {
        assert_eq!(
            (&a.id, &a.parent, a.visible, &a.kind, a.x, a.y),
            (&b.id, &b.parent, b.visible, &b.kind, b.x, b.y)
        );
        assert_eq!(a.pixels, b.pixels);
        assert_eq!(a.source, b.source);
        assert_eq!(a.mask.is_some(), b.mask.is_some());
        if let (Some(a), Some(b)) = (&a.mask, &b.mask) {
            assert_eq!(
                (a.enabled, a.linked, a.steps.len()),
                (b.enabled, b.linked, b.steps.len())
            );
            for (a, b) in a.steps.iter().zip(&b.steps) {
                assert_eq!(
                    (&a.id, &a.kind, a.enabled, a.weight, a.value, &a.settings),
                    (&b.id, &b.kind, b.enabled, b.weight, b.value, &b.settings)
                );
                assert_eq!(a.pixels, b.pixels);
            }
        }
    }
}
fn standard_only(bytes: &[u8]) -> Vec<u8> {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    [
        bytes[..at].to_vec(),
        vec![0; 4],
        bytes[at + 4 + size..].to_vec(),
    ]
    .concat()
}
fn embedded(bytes: &[u8]) -> Value {
    use std::io::Read;
    let marker = bytes.windows(4).position(|p| p == b"PBR1").unwrap() + 4;
    let mut decoded = Vec::new();
    flate2::read::ZlibDecoder::new(&bytes[marker..])
        .read_to_end(&mut decoded)
        .unwrap();
    serde_json::from_slice(&decoded).unwrap()
}
fn rewrite(bytes: &[u8], f: impl FnOnce(&mut Value)) -> Vec<u8> {
    use std::io::Write;
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let resources = &bytes[at + 4..at + 4 + size];
    let marker = resources.windows(4).position(|p| p == b"PBR1").unwrap();
    let len = u32::from_be_bytes(resources[marker - 4..marker].try_into().unwrap()) as usize;
    let mut value = embedded(bytes);
    f(&mut value);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    serde_json::to_writer(&mut z, &value).unwrap();
    let mut payload = b"PBR1".to_vec();
    payload.write_all(&z.finish().unwrap()).unwrap();
    let mut r = resources[..marker - 4].to_vec();
    r.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    r.extend_from_slice(&payload);
    if payload.len() % 2 != 0 {
        r.push(0);
    }
    r.extend_from_slice(&resources[marker + len + len % 2..]);
    [
        bytes[..at].to_vec(),
        (r.len() as u32).to_be_bytes().to_vec(),
        r,
        bytes[at + 4 + size..].to_vec(),
    ]
    .concat()
}
fn standard_composite(bytes: &[u8]) -> Vec<u16> {
    let bytes = standard_only(bytes);
    let (size, start) = if bytes[4..6] == 2u16.to_be_bytes() {
        (
            u64::from_be_bytes(bytes[34..42].try_into().unwrap()) as usize,
            42,
        )
    } else {
        (
            u32::from_be_bytes(bytes[34..38].try_into().unwrap()) as usize,
            38,
        )
    };
    assert_eq!(&bytes[start + size..start + size + 2], &[0, 0]);
    let depth = u16::from_be_bytes(bytes[22..24].try_into().unwrap());
    let p = &bytes[start + size + 2..];
    let words = if depth == 16 {
        p.chunks_exact(2)
            .map(|p| u16::from_be_bytes(p.try_into().unwrap()))
            .collect::<Vec<_>>()
    } else {
        p.iter().map(|v| u16::from(*v) * 257).collect()
    };
    let count = 64 * 48;
    assert_eq!(words.len(), count * 4);
    (0..count)
        .flat_map(|i| (0..4).map(move |c| (i, c)))
        .map(|(i, c)| words[c * count + i])
        .collect()
}
fn matte(doc: &Document) -> Vec<u16> {
    let words = if doc.bit_depth == 16 {
        peerbrush::depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, 64, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(|v| u16::from(v) * 257)
            .collect()
    };
    words
        .chunks_exact(4)
        .flat_map(|p| {
            let max = if doc.bit_depth == 16 { 65535u64 } else { 255 };
            let d = if doc.bit_depth == 16 { 1u64 } else { 257 };
            let a = u64::from(p[3]) / d;
            (0..4).map(move |c| {
                if c == 3 {
                    p[3]
                } else {
                    (((u64::from(p[c]) / d * a + max * (max - a) + max / 2) / max) * d) as u16
                }
            })
        })
        .collect()
}

fn protected_standard_readback(doc: &Document, standard: &[u16], depth: u16) {
    assert!(doc.read_only);
    assert_eq!((doc.width, doc.height, doc.bit_depth), (64, 48, depth));
    assert_eq!(doc.layers.len(), 1);
    assert!(!peerbrush::retained::has_originals(doc));
    assert!(doc
        .layers
        .iter()
        .all(|l| l.source.is_none() && l.mask.is_none()));
    // Standard merged RGB is white-matted at native depth. Its independently
    // decoded straight RGB need not equal the pre-matte editable RGB.
    let (max, divisor) = if depth == 16 {
        (65535i64, 1i64)
    } else {
        (255, 257)
    };
    let restored = standard
        .chunks_exact(4)
        .flat_map(|pixel| {
            let alpha = i64::from(pixel[3]) / divisor;
            (0..4).map(move |channel| {
                let value = i64::from(pixel[channel]) / divisor;
                let value = if channel == 3 {
                    alpha
                } else if alpha == 0 {
                    0
                } else {
                    (((value + alpha - max) * max + alpha / 2) / alpha).clamp(0, max)
                };
                (value * divisor) as u16
            })
        })
        .collect::<Vec<_>>();
    let layer = &doc.layers[0];
    assert_eq!(layer.pixels.depth, depth);
    assert_eq!(
        (layer.x, layer.y, layer.pixels.width, layer.pixels.height),
        (0, 0, 64, 48)
    );
    assert_eq!(layer.pixels.rgba16(), restored);
    assert_eq!(matte(doc), standard);
    let png = image::load_from_memory(&doc.export_png().unwrap()).unwrap();
    assert_eq!((png.width(), png.height()), (64, 48));
    assert_eq!(
        png.color(),
        if depth == 16 {
            image::ColorType::Rgba16
        } else {
            image::ColorType::Rgba8
        }
    );
    assert_eq!(png.to_rgba16().into_raw(), restored);
}

#[test]
fn whole_projective_native_color_masks_preview_history_and_format32_standard_channels() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let before = e.doc.clone();
        let saved = state(&e);
        let c = command(&e, json!({}));
        let preview = Engine::preview_edits(e.doc.clone(), &[c.clone()]).unwrap();
        assert_eq!(state(&e), saved);
        edit(&mut e, &[c]);
        native_layers(&e.doc, &preview);
        assert_eq!(e.undo.len(), 1);
        let l = &e.doc.layers[0];
        for y in l.y..l.y + l.pixels.height as i32 {
            for x in l.x..l.x + l.pixels.width as i32 {
                let [u, v] = inverse(x as f64 + 0.5, y as f64 + 0.5);
                assert_eq!(
                    color(l, x, y),
                    sample(&before.layers[0].pixels, (u - 0.5) as f32, (v - 0.5) as f32),
                    "{depth} Color {x},{y}"
                );
                assert_eq!(
                    paint(l, x, y),
                    sample(
                        &before.layers[0].mask.as_ref().unwrap().steps[1].pixels,
                        (u - 0.5) as f32,
                        (v - 0.5) as f32
                    ),
                    "{depth} mask {x},{y}"
                );
            }
        }
        assert_eq!(
            l.pixels.retained.as_ref().unwrap().pixels.rgba16(),
            before.layers[0].pixels.rgba16()
        );
        assert!(l.pixels.retained.as_ref().unwrap().projective.is_some());
        if depth == 16 {
            assert!(l
                .pixels
                .rgba16()
                .iter()
                .enumerate()
                .any(|(i, v)| i % 4 < 3 && v % 257 != 0));
        }
        for bytes in [
            psd::encode(&e.doc).unwrap(),
            psd::encode_psb(&e.doc).unwrap(),
        ] {
            assert_eq!(embedded(&bytes)["format"], 32);
            let loaded = psd::decode(&bytes).unwrap();
            assert!(!loaded.read_only);
            native_layers(&loaded, &e.doc);
            assert_eq!(loaded.export_png().unwrap(), e.doc.export_png().unwrap());
            let standard = psd::decode(&standard_only(&bytes)).unwrap();
            assert!(!standard.read_only);
            assert_eq!(standard.bit_depth, depth);
            assert_eq!(standard.layers[0].pixels.rgba16(), l.pixels.rgba16());
            for y in 0..48 {
                for x in 0..64 {
                    let value = |d: &Document| {
                        let l = &d.layers[0];
                        if depth == 16 {
                            (peerbrush::depth16::mask_value_prepared(
                                l,
                                x - l.x,
                                y - l.y,
                                None,
                                true,
                            ) * 65535.)
                                .round() as u16
                        } else {
                            (l.mask_value_raw(x - l.x, y - l.y) * 255.).round() as u16 * 257
                        }
                    };
                    assert_eq!(value(&standard), value(&e.doc));
                }
            }
            let composite = standard_composite(&bytes);
            assert_eq!(composite, matte(&e.doc));
            for format in [31, u32::MAX] {
                let rejected =
                    psd::decode(&rewrite(&bytes, |v| v["format"] = json!(format))).unwrap();
                assert!(rejected.read_only);
                assert!(!peerbrush::retained::has_originals(&rejected));
                protected_standard_readback(&rejected, &composite, depth);
            }
            let rejected = psd::decode(&rewrite(&bytes, |v| {
                v["document"]["layers"][0]["pixels"]["retained"]["projective"] =
                    json!([1., 0., 0., 0., 1., 0., 0., 0., 0.])
            }))
            .unwrap();
            assert!(rejected.read_only);
            protected_standard_readback(&rejected, &composite, depth);
        }
        let after = e.doc.clone();
        e.undo("human").unwrap();
        native_layers(&e.doc, &before);
        e.redo("human").unwrap();
        native_layers(&e.doc, &after);
    }
}

#[test]
fn affine_after_projective_and_reframing_reuse_original_once_and_masks_can_be_independent() {
    for depth in [8, 16] {
        for linked in [false, true] {
            let mut e = fixture(depth);
            e.doc.layers[0].mask.as_mut().unwrap().linked = linked;
            let before = e.doc.clone();
            let p = command(&e, json!({}));
            edit(&mut e, &[p.clone()]);
            let affine = json!({"op":"transform","layer":e.doc.layers[0].id,"selection_only":false,
                "pivot":[0.,0.],"scale_x":1.25,"scale_y":0.75,"angle":90.,"skew_x":20.,"dx":42.,"dy":1.});
            edit(&mut e, &[affine.clone()]);
            let mut combined = p;
            combined
                .as_object_mut()
                .unwrap()
                .extend(affine.as_object().unwrap().clone());
            let expected = Engine::preview_edits(before.clone(), &[combined]).unwrap();
            assert_eq!(e.doc.export_png().unwrap(), expected.export_png().unwrap());
            assert_eq!(
                e.doc.layers[0].pixels.rgba16(),
                expected.layers[0].pixels.rgba16()
            );
            assert_eq!(
                e.doc.layers[0]
                    .pixels
                    .retained
                    .as_ref()
                    .unwrap()
                    .pixels
                    .rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(
                e.doc.layers[0]
                    .pixels
                    .retained
                    .as_ref()
                    .unwrap()
                    .pixels
                    .get16(15, 11),
                color(&before.layers[0], 15, 11)
            );
            if !linked {
                let l = &e.doc.layers[0];
                let mask = l.mask.as_ref().unwrap().steps[1]
                    .pixels
                    .retained
                    .as_ref()
                    .unwrap();
                assert_eq!(
                    mask.pixels.as_ref(),
                    &before.layers[0].mask.as_ref().unwrap().steps[1].pixels
                );
                assert_eq!(mask.projective, None);
                assert_eq!(mask.matrix, [1., 0., 0., 1., -(l.x as f64), -(l.y as f64)]);
            }
        }
        let mut e = fixture(depth);
        e.doc.layers[0].mask.as_mut().unwrap().linked = false;
        let before = e.doc.clone();
        let step = before.layers[0].mask.as_ref().unwrap().steps[1].id.clone();
        let c = command(&e, json!({"mask":true,"step":step}));
        edit(&mut e, &[c]);
        assert_eq!(e.doc.layers[0].pixels, before.layers[0].pixels);
        let original = e.doc.layers[0].mask.as_ref().unwrap().steps[1]
            .pixels
            .retained
            .as_ref()
            .unwrap();
        assert!(original.projective.is_some());
        assert_eq!(
            original.pixels.as_ref(),
            &before.layers[0].mask.as_ref().unwrap().steps[1].pixels
        );
        // Off-frame projected mask samples survive the fixed-size mask projection.
        assert_eq!(embedded(&psd::encode(&e.doc).unwrap())["format"], 32);
        for bytes in [
            psd::encode(&e.doc).unwrap(),
            psd::encode_psb(&e.doc).unwrap(),
        ] {
            native_layers(&psd::decode(&bytes).unwrap(), &e.doc);
        }
    }
}

#[test]
fn selected_projective_soft_coverage_hole_native_masks_and_scalar_inverse_are_shared() {
    for depth in [8, 16] {
        for target in ["linked", "color", "mask", "step"] {
            let mut e = fixture(depth);
            e.doc.layers[0].mask.as_mut().unwrap().linked = target == "linked";
            let mut m = Raster::new(16, 12);
            for y in 2..9 {
                for x in 2..10 {
                    if (x, y) != (5, 5) {
                        m.set(
                            x,
                            y,
                            [if x == 2 {
                                64
                            } else if x == 3 {
                                128
                            } else {
                                255
                            }; 4],
                        );
                    }
                }
            }
            let coverage = selection::from_mask(m).unwrap();
            e.doc.selection = Some(coverage.bounds);
            e.doc.selection_coverage = Some(coverage.clone());
            let before = e.doc.clone();
            let mut cut = before.layers[0].pixels.clone();
            let mut masked = before.layers[0].mask.as_ref().unwrap().steps[1]
                .pixels
                .clone();
            for r in [&mut cut, &mut masked] {
                for y in 0..12 {
                    for x in 0..16 {
                        let mut p = r.get16(x, y);
                        p[3] = (p[3] as f64 * coverage.value(x, y) as f64).round() as u16;
                        r.set16(x, y, p);
                    }
                }
            }
            let mut c = command(
                &e,
                json!({"selection_only":true,"mask":target=="mask"||target=="step"}),
            );
            if target == "step" {
                c["step"] = json!(before.layers[0].mask.as_ref().unwrap().steps[1].id);
            }
            let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
            edit(&mut e, &[c]);
            native_layers(&e.doc, &preview);
            assert_eq!(e.undo.len(), 1);
            let after = selection::current(&e.doc).unwrap();
            let mut outside_bilinear_support = 0;
            let mut inside_sampled_pixels = 0;
            for y in after.bounds[1]..after.bounds[3] {
                for x in after.bounds[0]..after.bounds[2] {
                    let [u, v] = inverse(x as f64 + 0.5, y as f64 + 0.5);
                    // P is injective on the original rectangle: this analytic
                    // membership is independent of the engine's polygon fitter.
                    let inside = u >= f64::from(coverage.bounds[0])
                        && u <= f64::from(coverage.bounds[2])
                        && v >= f64::from(coverage.bounds[1])
                        && v <= f64::from(coverage.bounds[3]);
                    let [u, v] = [(u - 0.5) as f32, (v - 0.5) as f32];
                    let expected = |r: &Raster| {
                        let p = sample(r, u, v);
                        if !inside || p[3] == 0 {
                            [0; 4]
                        } else {
                            p
                        }
                    };
                    assert_eq!(
                        color(&e.doc.layers[0], x, y),
                        if matches!(target, "linked" | "color") {
                            expected(&cut)
                        } else {
                            [0; 4]
                        }
                    );
                    assert_eq!(
                        paint(&e.doc.layers[0], x, y),
                        if target != "color" {
                            expected(&masked)
                        } else {
                            [0; 4]
                        }
                    );
                    let (ix, iy) = (u.floor() as i32, v.floor() as i32);
                    let (fx, fy) = (u - ix as f32, v - iy as f32);
                    let at = |x, y| {
                        coverage
                            .mask
                            .get(x - coverage.origin[0], y - coverage.origin[1])[0]
                            as f32
                    };
                    let scalar = (at(ix, iy) * (1. - fx) * (1. - fy)
                        + at(ix + 1, iy) * fx * (1. - fy)
                        + at(ix, iy + 1) * (1. - fx) * fy
                        + at(ix + 1, iy + 1) * fx * fy)
                        .round() as u8;
                    assert_eq!((after.value(x, y) * 255.).round() as u8, scalar);
                    if !inside && scalar > 0 {
                        outside_bilinear_support += 1;
                    }
                    if inside && sample(&cut, u, v)[3] > 0 {
                        inside_sampled_pixels += 1;
                    }
                }
            }
            assert!(
                outside_bilinear_support > 0,
                "Exercise scalar interpolation beyond the center-clipped Color/mask polygon"
            );
            assert!(
                inside_sampled_pixels > 0,
                "Exercise actual moved native pixels"
            );
            for y in 0..12 {
                for x in 0..16 {
                    let remainder = |mut p: [u16; 4], moved: bool| {
                        let factor = coverage.value(x, y);
                        if moved && factor > 0. {
                            p[3] = (f64::from(p[3]) * (1. - f64::from(factor))).round() as u16;
                            if p[3] == 0 {
                                return [0; 4];
                            }
                            if depth == 8 {
                                p = p.map(|v| u16::from(peerbrush::raster::project16(v)) * 257);
                            }
                        }
                        p
                    };
                    assert_eq!(
                        color(&e.doc.layers[0], x, y),
                        remainder(
                            color(&before.layers[0], x, y),
                            matches!(target, "linked" | "color")
                        )
                    );
                    assert_eq!(
                        paint(&e.doc.layers[0], x, y),
                        remainder(paint(&before.layers[0], x, y), target != "color")
                    );
                }
            }
            assert_eq!(
                color(&e.doc.layers[0], 15, 11),
                color(&before.layers[0], 15, 11)
            );
            assert_eq!(
                color(&e.doc.layers[0], 5, 5),
                color(&before.layers[0], 5, 5)
            );
            e.undo("human").unwrap();
            native_layers(&e.doc, &before);
            assert_eq!(e.doc.selection_coverage, before.selection_coverage);
        }
    }
}

#[test]
fn projective_protocol_destination_selection_source_and_batch_protections_are_atomic() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        e.doc.selection = Some([2, 2, 10, 9]);
        let before = e.doc.clone();
        let mut c = command(
            &e,
            json!({"selection_only":true,"document_id":before.id,"source_revision":before.revision}),
        );
        let id = e.doc.layers[0].id.clone();
        e.reserve(
            "other",
            "Projected destination",
            vec![Scope {
                target: Some(id),
                rect: Some([28, 8, 29, 9]),
            }],
        )
        .unwrap();
        let shared = Arc::new(Mutex::new(e));
        let request = json!({"actor":"projective-agent","document_id":before.id,"expected_revision":before.revision,"commands":[c],"feedback":"always","max_edge":64});
        let saved = state(&shared.lock().unwrap());
        assert!(server::dispatch(&shared, "edit", &request).is_err());
        assert_eq!(state(&shared.lock().unwrap()), saved);
        {
            let mut e = shared.lock().unwrap();
            e.leases.clear();
            e.reserve("other", "Selection", vec![Scope::layer("@selection")])
                .unwrap();
        }
        assert!(server::dispatch(&shared, "edit", &request).is_err());
        assert_eq!(state(&shared.lock().unwrap()), saved);
        shared.lock().unwrap().leases.clear();
        let reply = server::dispatch(&shared, "edit", &request).unwrap();
        assert_eq!(reply["images"][0]["document_rect"], json!([0, 0, 64, 48]));
        assert_eq!(shared.lock().unwrap().undo.len(), 1);
        assert!(server::dispatch(&shared, "edit", &request).is_err());

        let mut e = fixture(depth);
        let saved = state(&e);
        let first = json!({"op":"layer.update","layer":e.doc.layers[0].id,"name":"Must roll back"});
        for value in [
            Value::Null,
            json!({"rect":[0,0,16,12],"quad":[[0,0],[16,12],[16,0],[0,12]]}),
            json!({"rect":[0,0,16,12],"quad":[[0,0],[16,0],[16,12],[0,12]],"unknown":true}),
            json!({"rect":[0,0,0,12],"quad":[[0,0],[16,0],[16,12],[0,12]]}),
            json!({"rect":[0,0,16,12],"quad":[[0,0],[90000,0],[90000,90000],[0,90000]]}),
        ] {
            c = command(&e, json!({"projective":value}));
            assert!(e
                .edit(
                    "human",
                    &[first.clone(), c.clone()],
                    None,
                    None,
                    "Invalid projective"
                )
                .is_err());
            assert_eq!(state(&e), saved);
            assert!(Engine::preview_edits(e.doc.clone(), &[c]).is_err());
        }
        let c = command(&e, json!({"op":"move"}));
        assert!(transform::resolve_command(&c).is_err());
        e.doc.layers[0].locked = true;
        let saved = state(&e);
        let c = command(&e, json!({}));
        assert!(e.edit("human", &[c], None, None, "Locked").is_err());
        assert_eq!(state(&e), saved);
    }
}

#[test]
fn hidden_folder_sources_and_full_retained_frame_horizon_reject_before_mutation() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let mut group = Layer::new("Folder", "group", 64, 48);
        let root = group.id.clone();
        group.pixels = Raster::new_depth(64, 48, depth);
        e.doc.layers[0].parent = Some(root.clone());
        let mut hidden = e.doc.layers[0].clone();
        hidden.id = peerbrush::engine::id();
        hidden.visible = false;
        e.doc.layers.push(hidden);
        e.doc.layers.insert(0, group);
        let before = e.doc.clone();
        let mut c = command(&e, json!({"layer":root}));
        let preview = Engine::preview_edits(e.doc.clone(), &[c.clone()]).unwrap();
        edit(&mut e, &[c.clone()]);
        native_layers(&e.doc, &preview);
        assert!(!e.doc.layers[2].visible);
        assert_eq!(
            e.doc.layers[2].pixels.rgba16(),
            e.doc.layers[1].pixels.rgba16()
        );
        assert_eq!(e.undo.len(), 1);
        e.undo("human").unwrap();
        native_layers(&e.doc, &before);
        edit(
            &mut e,
            &[
                json!({"op":"source.add","source":{"width":16,"height":12,"content":{"kind":"shape","shape":"rectangle","bounds":[1,1,10,10],"points":[],"closed":true,"fill":[12347,23459,34571,65535],"stroke":[0,0,0,0],"stroke_width":0}}}),
            ],
        );
        let source = e.doc.layers[0].id.clone();
        e.doc.layers[0].parent = Some(root.clone());
        e.doc.layers[0].visible = false;
        let saved = state(&e);
        let error = e
            .edit("human", &[c.clone()], None, None, "Hidden source")
            .unwrap_err();
        assert!(error.to_lowercase().contains("rasterize"));
        assert_eq!(state(&e), saved);
        c["layer"] = json!(source);
        let error = e
            .edit("human", &[c], None, None, "Whole source")
            .unwrap_err();
        assert!(error.to_lowercase().contains("rasterize"));
        assert_eq!(state(&e), saved);
        let mut e = fixture(depth);
        let saved = state(&e);
        // P is well behaved on [0,1] but its horizon x=8 crosses the complete
        // original frame, including transparent off-rect original samples.
        let c = command(
            &e,
            json!({"projective":{"rect":[0,0,1,1],"quad":[[0.,0.],[8./7.,0.],[8./7.,8./7.],[0.,1.]]}}),
        );
        assert!(e
            .edit("human", &[c], None, None, "Full-frame horizon")
            .is_err());
        assert_eq!(state(&e), saved);
    }
}

#[test]
fn absent_or_exact_identity_projective_keeps_legacy_affine_samples_scopes_and_json() {
    for depth in [8, 16] {
        for selected in [false, true] {
            let mut e = fixture(depth);
            if selected {
                e.doc.selection = Some([2, 2, 10, 9]);
            }
            let c = json!({"op":"transform","layer":e.doc.layers[0].id,"selection_only":selected,"angle":17.,"scale_x":1.3,"scale_y":0.7,"skew_x":-15.,"mirror_y":true,"dx":7.,"dy":5.});
            let mut identity = c.clone();
            identity["projective"] =
                json!({"rect":[0,0,16,12],"quad":[[0,0],[16,0],[16,12],[0,12]]});
            assert_eq!(e.scopes(&c), e.scopes(&identity));
            let a = Engine::preview_edits(e.doc.clone(), &[c]).unwrap();
            let b = Engine::preview_edits(e.doc.clone(), &[identity]).unwrap();
            native_layers(&a, &b);
            assert_eq!(a.selection_polygon, b.selection_polygon);
            assert_eq!(a.export_png().unwrap(), b.export_png().unwrap());
            assert_eq!(
                serde_json::to_value(&a).unwrap(),
                serde_json::to_value(&b).unwrap()
            );
            assert!(!serde_json::to_string(&a).unwrap().contains("projective"));
        }
    }
}

#[test]
fn polygon_multiple_roots_and_clipboard_keep_shared_map_selection_and_native_originals() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let mut second = e.doc.layers[0].clone();
        second.id = peerbrush::engine::id();
        second.visible = false;
        e.doc.layers.push(second);
        let polygon = vec![[2., 2.], [10., 2.], [8., 9.], [3., 7.]];
        e.doc.selection = Some(selection::bounds(&polygon).unwrap());
        e.doc.selection_polygon = Some(polygon.clone());
        let before = e.doc.clone();
        let commands = before
            .layers
            .iter()
            .map(|l| command(&e, json!({"layer":l.id,"selection_only":true})))
            .collect::<Vec<_>>();
        let singles = commands
            .iter()
            .map(|c| Engine::preview_edits(before.clone(), &[c.clone()]).unwrap())
            .collect::<Vec<_>>();
        let preview = Engine::preview_edits(before.clone(), &commands).unwrap();
        edit(&mut e, &commands);
        native_layers(&e.doc, &preview);
        assert_eq!(e.undo.len(), 1);
        let projected = polygon
            .iter()
            .map(|p| {
                let d = 1. + p[0] as f64 / 64.;
                [
                    ((p[0] as f64 + 24.) / d) as f32,
                    ((p[1] as f64 + 3.) / d) as f32,
                ]
            })
            .collect::<Vec<_>>();
        assert_eq!(e.doc.selection_polygon, Some(projected));
        for (i, single) in singles.iter().enumerate() {
            assert_eq!(e.doc.layers[i].pixels, single.layers[i].pixels);
        }
        e.undo("human").unwrap();
        native_layers(&e.doc, &before);
        assert_eq!(e.doc.selection_polygon, before.selection_polygon);

        e.doc.selection = None;
        e.doc.selection_polygon = None;
        let c = command(&e, json!({}));
        edit(&mut e, &[c]);
        let copied =
            peerbrush::layer_clipboard::copy(&e.doc, &[e.doc.layers[0].id.clone()]).unwrap();
        peerbrush::layer_clipboard::validate(&copied).unwrap();
        let mut pasted = Document::new_depth(64, 48, depth).unwrap();
        peerbrush::layer_clipboard::paste(&mut pasted, &copied, "").unwrap();
        let l = pasted
            .layers
            .iter()
            .find(|l| l.pixels.retained.is_some())
            .unwrap();
        assert_eq!(l.pixels.retained, e.doc.layers[0].pixels.retained);
        let observed = peerbrush::retained::observe(&l.pixels);
        assert_eq!(observed["projective"].as_array().unwrap().len(), 9);
        assert_eq!(
            observed["matrix"],
            json!(l.pixels.retained.as_ref().unwrap().matrix)
        );
        let mut malformed = copied.clone();
        malformed.layers[0]
            .pixels
            .retained
            .as_mut()
            .unwrap()
            .projective = Some([0.; 9]);
        assert!(peerbrush::layer_clipboard::validate(&malformed).is_err());
        assert_eq!(
            server::capabilities()["retained_transforms"]["projective"]["source_format"],
            32
        );
    }
}
