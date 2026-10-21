use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    psd,
    raster::Raster,
    selection, server,
    warp::Grid,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn edit(e: &mut Engine, commands: &[Value]) {
    e.edit("human", commands, Some(e.doc.revision), None, "Grid Warp")
        .unwrap();
}
fn state(e: &Engine) -> Value {
    let history = |items: &[peerbrush::engine::History]| {
        items.iter().map(|h|json!({"before":h.before,"after":h.after,"actor":h.actor,"label":h.label,"task":h.task,"scopes":h.scopes,"gesture":h.gesture,"reverted":h.reverted})).collect::<Vec<_>>()
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
    l.mask.as_mut().unwrap().linked = true;
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
    e.undo.clear();
    e
}
fn command(e: &Engine, fields: Value) -> Value {
    let mut c = json!({"op":"transform","layer":e.doc.layers[0].id,"selection_only":false,"warp":{"rect":[-2,-2,18,14],"preset":"arc","amount":0.2}});
    c.as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    c
}
fn arc(x: f64, rect: [f64; 4], amount: f64) -> f64 {
    let center = (rect[0] + rect[2]) * 0.5;
    amount * (rect[3] - rect[1]) * (1. - ((x - center) * 2. / (rect[2] - rect[0])).abs())
}
fn sample(r: &Raster, x: f32, y: f32) -> [u16; 4] {
    if x.fract() == 0. && y.fract() == 0. {
        r.get16(x as i32, y as i32)
    } else if r.depth == 16 {
        r.sample16(x, y)
    } else {
        r.sample(x, y).map(|v| u16::from(v) * 257)
    }
}
fn color(l: &Layer, x: i32, y: i32) -> [u16; 4] {
    l.pixels.get16(x - l.x, y - l.y)
}
fn paint(l: &Layer, x: i32, y: i32) -> [u16; 4] {
    l.mask.as_ref().unwrap().steps[1]
        .pixels
        .get16(x - l.x, y - l.y)
}
fn native_layers(a: &Document, b: &Document) {
    assert_eq!(a.layers.len(), b.layers.len());
    for (a, b) in a.layers.iter().zip(&b.layers) {
        assert_eq!(
            (&a.id, &a.kind, &a.parent, a.visible, a.x, a.y),
            (&b.id, &b.kind, &b.parent, b.visible, b.x, b.y)
        );
        assert_eq!(a.pixels, b.pixels);
        assert_eq!(a.source, b.source);
        assert_eq!(a.mask.is_some(), b.mask.is_some());
        if let (Some(a), Some(b)) = (&a.mask, &b.mask) {
            assert_eq!(
                (a.linked, a.enabled, a.steps.len()),
                (b.linked, b.enabled, b.steps.len())
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
    let at = bytes.windows(4).position(|v| v == b"PBR1").unwrap() + 4;
    let mut json = Vec::new();
    flate2::read::ZlibDecoder::new(&bytes[at..])
        .read_to_end(&mut json)
        .unwrap();
    serde_json::from_slice(&json).unwrap()
}
fn rewrite(bytes: &[u8], change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let size = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let r = &bytes[at + 4..at + 4 + size];
    let marker = r.windows(4).position(|v| v == b"PBR1").unwrap();
    let len = u32::from_be_bytes(r[marker - 4..marker].try_into().unwrap()) as usize;
    let mut value = embedded(bytes);
    change(&mut value);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    serde_json::to_writer(&mut z, &value).unwrap();
    let mut payload = b"PBR1".to_vec();
    payload.extend(z.finish().unwrap());
    let mut out = r[..marker - 4].to_vec();
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(&payload);
    if payload.len() % 2 != 0 {
        out.push(0);
    }
    out.extend_from_slice(&r[marker + len + len % 2..]);
    [
        bytes[..at].to_vec(),
        (out.len() as u32).to_be_bytes().to_vec(),
        out,
        bytes[at + 4 + size..].to_vec(),
    ]
    .concat()
}
fn composite(bytes: &[u8]) -> Vec<u16> {
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
    let raw = &bytes[start + size + 2..];
    let words = if depth == 16 {
        raw.chunks_exact(2)
            .map(|p| u16::from_be_bytes(p.try_into().unwrap()))
            .collect::<Vec<_>>()
    } else {
        raw.iter().map(|v| u16::from(*v) * 257).collect()
    };
    let n = 64 * 48;
    assert_eq!(words.len(), n * 4);
    (0..n)
        .flat_map(|i| (0..4).map(move |c| (i, c)))
        .map(|(i, c)| words[c * n + i])
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
fn protected(doc: &Document, standard: &[u16], depth: u16) {
    assert!(doc.read_only);
    assert_eq!((doc.width, doc.height, doc.bit_depth), (64, 48, depth));
    assert_eq!(doc.layers.len(), 1);
    assert!(!peerbrush::retained::has_originals(doc));
    assert!(doc
        .layers
        .iter()
        .all(|l| l.source.is_none() && l.mask.is_none()));
    let (max, d) = if depth == 16 {
        (65535i64, 1i64)
    } else {
        (255, 257)
    };
    let restored = standard
        .chunks_exact(4)
        .flat_map(|p| {
            let a = i64::from(p[3]) / d;
            (0..4).map(move |c| {
                let v = i64::from(p[c]) / d;
                let v = if c == 3 {
                    a
                } else if a == 0 {
                    0
                } else {
                    (((v + a - max) * max + a / 2) / a).clamp(0, max)
                };
                (v * d) as u16
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(doc.layers[0].pixels.depth, depth);
    assert_eq!(doc.layers[0].pixels.rgba16(), restored);
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
fn storage(doc: &Document) {
    for bytes in [psd::encode(doc).unwrap(), psd::encode_psb(doc).unwrap()] {
        assert_eq!(embedded(&bytes)["format"], 33);
        let loaded = psd::decode(&bytes).unwrap();
        assert!(!loaded.read_only);
        native_layers(&loaded, doc);
        assert_eq!(loaded.export_png().unwrap(), doc.export_png().unwrap());
        let plain = psd::decode(&standard_only(&bytes)).unwrap();
        assert!(!plain.read_only);
        assert_eq!(plain.bit_depth, doc.bit_depth);
        assert_eq!(
            plain.layers[0].pixels.rgba16(),
            doc.layers[0].pixels.rgba16()
        );
        for y in 0..48 {
            for x in 0..64 {
                let value = |d: &Document| {
                    let l = &d.layers[0];
                    if doc.bit_depth == 16 {
                        (peerbrush::depth16::mask_value_prepared(l, x - l.x, y - l.y, None, true)
                            * 65535.)
                            .round() as u16
                    } else {
                        (l.mask_value_raw(x - l.x, y - l.y) * 255.).round() as u16 * 257
                    }
                };
                assert_eq!(value(&plain), value(doc));
            }
        }
        let saved = composite(&bytes);
        assert_eq!(saved, matte(doc));
        for format in [32, u32::MAX] {
            protected(
                &psd::decode(&rewrite(&bytes, |v| v["format"] = json!(format))).unwrap(),
                &saved,
                doc.bit_depth,
            );
        }
        for damage in 0..3 {
            let rejected = psd::decode(&rewrite(&bytes, |v| {
                let stages = &mut v["document"]["layers"][0]["pixels"]["retained"]["warps"];
                match damage {
                    0 => stages[0]["grid"]["points"][4] = json!([5000, 5000]),
                    1 => {
                        let stage = stages[0].clone();
                        *stages = json!(vec![stage; 17]);
                    }
                    _ => stages[0]["before"] = json!(vec![0.0; 9]),
                }
            }))
            .unwrap();
            protected(&rejected, &saved, doc.bit_depth);
        }
    }
}

#[test]
fn whole_grid_arc_uses_independent_native_inverse_masks_originals_history_and_standard_sources() {
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
                let shift = arc(x as f64 + 0.5, [-2., -2., 18., 14.], 0.2);
                let (u, v) = (x as f32, (y as f64 - shift) as f32);
                assert_eq!(color(l, x, y), sample(&before.layers[0].pixels, u, v));
                assert_eq!(
                    paint(l, x, y),
                    sample(
                        &before.layers[0].mask.as_ref().unwrap().steps[1].pixels,
                        u,
                        v
                    )
                );
            }
        }
        let original = l.pixels.retained.as_ref().unwrap();
        assert_eq!(original.warps.len(), 1);
        assert_eq!(original.pixels.rgba16(), before.layers[0].pixels.rgba16());
        assert_eq!(
            original.pixels.get16(15, 11),
            before.layers[0].pixels.get16(15, 11)
        );
        assert_eq!(
            l.pixels.stored_bytes(),
            l.pixels.bytes()
                + original.pixels.bytes()
                + original.warps.capacity() * std::mem::size_of::<peerbrush::warp::Stage>()
        );
        if depth == 16 {
            assert!(l
                .pixels
                .rgba16()
                .iter()
                .enumerate()
                .any(|(i, v)| i % 4 < 3 && v % 257 != 0));
        }
        storage(&e.doc);
        let after = e.doc.clone();
        e.undo("human").unwrap();
        native_layers(&e.doc, &before);
        e.redo("human").unwrap();
        native_layers(&e.doc, &after);
    }
}

#[test]
fn successive_warps_then_affine_projective_preserve_one_original_and_reverse_exact_map_order() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let before = e.doc.clone();
        let c = command(
            &e,
            json!({"warp":{"rect":[-2,-2,18,14],"preset":"arc","amount":0.125}}),
        );
        edit(&mut e, &[c]);
        // The second source plane includes all first-stage interpolation support.
        let second = [-4., -4., 20., 20.];
        let c = command(
            &e,
            json!({"warp":{"rect":second,"preset":"arc","amount":-0.1}}),
        );
        edit(&mut e, &[c]);
        let id = e.doc.layers[0].id.clone();
        edit(
            &mut e,
            &[json!({"op":"transform","layer":id,"selection_only":false,"dx":3,"dy":2})],
        );
        let c = json!({"op":"transform","layer":id,"selection_only":false,"projective":{"rect":[0,0,64,48],"quad":[[0,0],[64./1.5,0],[64./1.5,48./1.5],[0,48]]}});
        edit(&mut e, &[c]);
        let l = &e.doc.layers[0];
        let original = l.pixels.retained.as_ref().unwrap();
        assert_eq!(original.warps.len(), 2);
        assert_eq!(original.pixels.rgba16(), before.layers[0].pixels.rgba16());
        for y in l.y..l.y + l.pixels.height as i32 {
            for x in l.x..l.x + l.pixels.width as i32 {
                let px = x as f64 + 0.5;
                let u = px / (1. - px / 128.);
                let v = (y as f64 + 0.5) * (1. + u / 128.);
                let (u, v) = (u - 3., v - 2.);
                let v = v - arc(u, second, -0.1) - arc(u, [-2., -2., 18., 14.], 0.125);
                assert_eq!(
                    color(l, x, y),
                    sample(&before.layers[0].pixels, (u - 0.5) as f32, (v - 0.5) as f32)
                );
                assert_eq!(
                    paint(l, x, y),
                    sample(
                        &before.layers[0].mask.as_ref().unwrap().steps[1].pixels,
                        (u - 0.5) as f32,
                        (v - 0.5) as f32
                    )
                );
            }
        }
        storage(&e.doc);
    }
}

#[test]
fn one_mixed_command_and_canonical_preset_share_native_affine_projective_grid_sampling() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let before = e.doc.clone();
        let saved = state(&e);
        let c = command(
            &e,
            json!({"pivot":[0,0],"scale_x":1.25,"scale_y":0.75,"dx":3,"dy":2,"projective":{"rect":[0,0,64,48],"quad":[[0,0],[64./1.5,0],[64./1.5,48./1.5],[0,48]]}}),
        );
        let mut canonical = c.clone();
        canonical["warp"] = json!(Grid::preset([-2., -2., 18., 14.], "arc", 0.2).unwrap());
        let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
        assert_eq!(
            serde_json::to_value(&preview).unwrap(),
            serde_json::to_value(Engine::preview_edits(before.clone(), &[canonical]).unwrap())
                .unwrap()
        );
        assert_eq!(state(&e), saved);
        edit(&mut e, &[c]);
        native_layers(&e.doc, &preview);
        let l = &e.doc.layers[0];
        assert_eq!(l.pixels.retained.as_ref().unwrap().warps.len(), 1);
        assert_eq!(
            l.pixels.retained.as_ref().unwrap().pixels.as_ref(),
            &before.layers[0].pixels
        );
        for y in l.y..l.y + l.pixels.height as i32 {
            for x in l.x..l.x + l.pixels.width as i32 {
                let px = (x as f64 + 0.5 - 3.) / 1.25;
                let u = px / (1. - px / 128.);
                let v = (y as f64 + 0.5 - 2.) / 0.75 * (1. + u / 128.)
                    - arc(u, [-2., -2., 18., 14.], 0.2);
                assert_eq!(
                    color(l, x, y),
                    sample(&before.layers[0].pixels, (u - 0.5) as f32, (v - 0.5) as f32)
                );
                assert_eq!(
                    paint(l, x, y),
                    sample(
                        &before.layers[0].mask.as_ref().unwrap().steps[1].pixels,
                        (u - 0.5) as f32,
                        (v - 0.5) as f32
                    )
                );
            }
        }
        storage(&e.doc);
    }
}

#[test]
fn independent_mask_warp_retains_off_frame_samples_and_reenters_without_an_eight_bit_copy() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        e.doc.layers[0].mask.as_mut().unwrap().linked = false;
        let before = e.doc.clone();
        let id = e.doc.layers[0].id.clone();
        let step = e.doc.layers[0].mask.as_ref().unwrap().steps[1].id.clone();
        edit(
            &mut e,
            &[json!({"op":"move","layer":id,"mask":true,"step":step,"dx":24})],
        );
        assert_eq!(
            e.doc.layers[0].mask.as_ref().unwrap().steps[1]
                .pixels
                .bytes(),
            0
        );
        let rect = [22., -2., 42., 14.];
        let c = command(
            &e,
            json!({"mask":true,"step":step,"warp":{"rect":rect,"preset":"arc","amount":0.2}}),
        );
        edit(&mut e, &[c]);
        assert_eq!(e.doc.layers[0].pixels, before.layers[0].pixels);
        for bytes in [
            psd::encode(&e.doc).unwrap(),
            psd::encode_psb(&e.doc).unwrap(),
        ] {
            assert_eq!(embedded(&bytes)["format"], 33);
            let loaded = psd::decode(&bytes).unwrap();
            assert!(!loaded.read_only);
            native_layers(&loaded, &e.doc);
            assert_eq!(loaded.export_png().unwrap(), e.doc.export_png().unwrap());
            let saved = composite(&bytes);
            assert_eq!(saved, matte(&e.doc));
            for format in [32, u32::MAX] {
                protected(
                    &psd::decode(&rewrite(&bytes, |v| v["format"] = json!(format))).unwrap(),
                    &saved,
                    depth,
                );
            }
            let damaged = rewrite(&bytes, |v| {
                v["document"]["layers"][0]["mask"]["steps"][1]["pixels"]["retained"]["warps"][0]
                    ["before"] = json!(vec![0.0; 9])
            });
            protected(&psd::decode(&damaged).unwrap(), &saved, depth);
        }
        edit(
            &mut e,
            &[json!({"op":"move","layer":id,"mask":true,"step":step,"dx":-24})],
        );
        let original = e.doc.layers[0].mask.as_ref().unwrap().steps[1]
            .pixels
            .retained
            .as_ref()
            .unwrap();
        assert_eq!(original.warps.len(), 1);
        assert_eq!(
            original.pixels.rgba16(),
            before.layers[0].mask.as_ref().unwrap().steps[1]
                .pixels
                .rgba16()
        );
        for y in 0..12 {
            for x in 0..16 {
                let v = y as f64 - arc(x as f64 + 24.5, rect, 0.2);
                assert_eq!(
                    paint(&e.doc.layers[0], x, y),
                    sample(
                        &before.layers[0].mask.as_ref().unwrap().steps[1].pixels,
                        x as f32,
                        v as f32
                    )
                );
            }
        }
    }
}

#[test]
fn grid_protocol_destination_selection_source_and_partial_batches_keep_existing_guards() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        e.doc.selection = Some([2, 2, 10, 9]);
        let before = e.doc.clone();
        let c = command(
            &e,
            json!({"selection_only":true,"dx":24,"dy":4,"document_id":before.id,"source_revision":before.revision}),
        );
        e.reserve(
            "other",
            "Warp destination",
            vec![Scope {
                target: Some(before.layers[0].id.clone()),
                rect: Some([28, 8, 29, 9]),
            }],
        )
        .unwrap();
        let shared = Arc::new(Mutex::new(e));
        let saved = state(&shared.lock().unwrap());
        let request = json!({"actor":"warp-agent","document_id":before.id,"expected_revision":before.revision,"commands":[c],"feedback":"always","max_edge":64});
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
        let mut folded = Grid::identity([-2., -2., 18., 14.]).unwrap();
        folded.points[4] = [30., 4.];
        let mut huge = Grid::identity([-2., -2., 18., 14.]).unwrap();
        for p in &mut huge.points {
            p[0] *= 200.;
            p[1] *= 200.;
        }
        for grid in [
            Value::Null,
            json!(folded),
            json!(huge),
            json!({"rect":[0,0,16,12],"preset":"arc","amount":0.2}),
            json!({"rect":[-2,-2,18,14],"preset":"arc","amount":0.251}),
            json!({"rect":[-2,-2,18,14],"preset":"unknown","amount":0.1}),
            json!({"rect":[-2,-2,18,14],"preset":"arc","amount":0.1,"unknown":true}),
        ] {
            let c = command(&e, json!({"warp":grid}));
            assert!(e
                .edit(
                    "human",
                    &[first.clone(), c.clone()],
                    None,
                    None,
                    "Invalid Warp"
                )
                .is_err());
            assert_eq!(state(&e), saved);
            assert!(Engine::preview_edits(e.doc.clone(), &[c]).is_err());
        }
        let c = command(&e, json!({"op":"move"}));
        assert!(peerbrush::transform::resolve_command(&c).is_err());
        e.doc.layers[0].locked = true;
        let saved = state(&e);
        let c = command(&e, json!({}));
        assert!(e.edit("human", &[c], None, None, "Locked").is_err());
        assert_eq!(state(&e), saved);
    }
}

#[test]
fn polygon_seam_vertices_multiple_roots_hidden_children_and_clipboard_preserve_native_state() {
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
        let projected = e.doc.selection_polygon.as_ref().unwrap();
        assert!(projected.len() > polygon.len());
        assert!(projected
            .iter()
            .any(|p| (p[0] - 8.).abs() < 1e-5 && (p[1] - 5.2).abs() < 1e-5));
        for p in projected {
            let q = [
                p[0] as f64,
                p[1] as f64 - arc(p[0] as f64, [-2., -2., 18., 14.], 0.2),
            ];
            assert!(polygon.iter().enumerate().any(|(i, a)| {
                let b = polygon[(i + 1) % polygon.len()];
                let (u, v) = (b[0] as f64 - a[0] as f64, b[1] as f64 - a[1] as f64);
                let (x, y) = (q[0] - a[0] as f64, q[1] - a[1] as f64);
                (u * y - v * x).abs() < 1e-4
                    && x * u + y * v >= -1e-4
                    && x * u + y * v <= u * u + v * v + 1e-4
            }));
        }
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
        let (a, b) = (
            l.mask.as_ref().unwrap(),
            e.doc.layers[0].mask.as_ref().unwrap(),
        );
        assert_eq!(
            (a.linked, a.enabled, a.steps.len()),
            (b.linked, b.enabled, b.steps.len())
        );
        for (a, b) in a.steps.iter().zip(&b.steps) {
            assert_ne!(a.id, b.id);
            assert_eq!(
                (&a.kind, a.enabled, a.weight, a.value, &a.settings),
                (&b.kind, b.enabled, b.weight, b.value, &b.settings)
            );
            assert_eq!(a.pixels, b.pixels);
        }
        let observed = peerbrush::retained::observe(&l.pixels);
        assert_eq!(observed["warp_stages"].as_array().unwrap().len(), 1);
        assert_eq!(
            observed["warp_stages"][0]["grid"],
            json!(Grid::preset([-2., -2., 18., 14.], "arc", 0.2).unwrap())
        );
        let mut malformed = copied.clone();
        malformed.layers[0].pixels.retained.as_mut().unwrap().warps[0].before = [0.; 9];
        assert!(peerbrush::layer_clipboard::validate(&malformed).is_err());

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
        let c = command(&e, json!({"layer":root}));
        let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
        edit(&mut e, &[c.clone()]);
        native_layers(&e.doc, &preview);
        assert!(!e.doc.layers[2].visible);
        assert_eq!(e.doc.layers[1].pixels, e.doc.layers[2].pixels);
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
            .edit("human", &[c], None, None, "Hidden source")
            .unwrap_err();
        assert!(error.to_lowercase().contains("rasterize"));
        assert_eq!(state(&e), saved);
        let c = command(&e, json!({"layer":source}));
        assert!(e
            .edit("human", &[c], None, None, "Whole source")
            .unwrap_err()
            .to_lowercase()
            .contains("rasterize"));
        assert_eq!(state(&e), saved);
        e.doc.layers[0].source = None;
        e.doc.layers[0].kind = "adjustment".into();
        let saved = state(&e);
        let c = command(&e, json!({"layer":root}));
        assert!(e
            .edit("human", &[c], None, None, "Hidden adjustment")
            .unwrap_err()
            .to_lowercase()
            .contains("rasterize"));
        assert_eq!(state(&e), saved);
    }
}

#[test]
fn exact_identity_grid_keeps_old_affine_projective_scopes_native_samples_and_serialization() {
    for depth in [8, 16] {
        for selected in [false, true] {
            for projected in [false, true] {
                let mut e = fixture(depth);
                if selected {
                    e.doc.selection = Some([2, 2, 10, 9]);
                }
                let mut c = json!({"op":"transform","layer":e.doc.layers[0].id,"selection_only":selected,"angle":17.,"scale_x":1.3,"scale_y":0.7,"skew_x":-15.,"mirror_y":true,"dx":7.,"dy":5.});
                if projected {
                    c["projective"] =
                        json!({"rect":[0,0,16,12],"quad":[[0,0],[14,0],[14,10],[0,12]]});
                }
                let mut identity = c.clone();
                identity["warp"] = json!(Grid::identity([-2., -2., 18., 14.]).unwrap());
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
                assert!(!serde_json::to_string(&a).unwrap().contains("\"warps\""));
            }
        }
    }
}

#[test]
fn sixteen_retained_stages_are_exact_metadata_and_the_seventeenth_is_an_atomic_actionable_refusal()
{
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let original = e.doc.layers[0].pixels.clone();
        for i in 0..16 {
            let c = command(
                &e,
                json!({"warp":{"rect":[-32,-32,48,44],"preset":"flag","amount":if i%2==0{0.005}else{-0.005}}}),
            );
            edit(&mut e, &[c]);
        }
        let l = &e.doc.layers[0];
        assert_eq!(l.pixels.retained.as_ref().unwrap().warps.len(), 16);
        assert_eq!(
            l.pixels.retained.as_ref().unwrap().pixels.as_ref(),
            &original
        );
        let c = json!({"op":"transform","layer":l.id,"selection_only":false,"dx":1});
        edit(&mut e, &[c]);
        assert_eq!(
            e.doc.layers[0]
                .pixels
                .retained
                .as_ref()
                .unwrap()
                .warps
                .len(),
            16
        );
        let saved = state(&e);
        let c = command(
            &e,
            json!({"warp":{"rect":[-64,-64,80,80],"preset":"arc","amount":0.005}}),
        );
        let error = e
            .edit("human", &[c.clone()], None, None, "Seventeenth Warp")
            .unwrap_err();
        assert!(error.contains("undo") && error.contains("cancel"));
        assert_eq!(state(&e), saved);
        assert!(Engine::preview_edits(e.doc.clone(), &[c]).is_err());
        assert_eq!(
            server::capabilities()["retained_transforms"]["warp"]["source_format"],
            33
        );
    }
}

#[test]
fn selected_grid_scalar_halo_color_center_clip_holes_and_every_original_remainder_stay_native() {
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
                        p[3] = (f64::from(p[3]) * f64::from(coverage.value(x, y))).round() as u16;
                        r.set16(x, y, p);
                    }
                }
            }
            let mut c = command(
                &e,
                json!({"selection_only":true,"dx":24,"dy":4,"mask":target=="mask"||target=="step"}),
            );
            if target == "step" {
                c["step"] = json!(before.layers[0].mask.as_ref().unwrap().steps[1].id);
            }
            let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
            edit(&mut e, &[c]);
            native_layers(&e.doc, &preview);
            assert_eq!(e.undo.len(), 1);
            let after = selection::current(&e.doc).unwrap();
            let mut halo = 0;
            let mut moved = 0;
            for y in after.bounds[1]..after.bounds[3] {
                for x in after.bounds[0]..after.bounds[2] {
                    let u = x as f64 + 0.5 - 24.;
                    let v = y as f64 + 0.5 - 4. - arc(u, [-2., -2., 18., 14.], 0.2);
                    let inside = u >= 2. && u <= 10. && v >= 2. && v <= 9.;
                    let (u, v) = ((u - 0.5) as f32, (v - 0.5) as f32);
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
                        halo += 1;
                    }
                    if inside && sample(&cut, u, v)[3] > 0 {
                        moved += 1;
                    }
                }
            }
            assert!(halo > 0 && moved > 0);
            for y in 0..12 {
                for x in 0..16 {
                    let remainder = |mut p: [u16; 4], changed: bool| {
                        let f = coverage.value(x, y);
                        if changed && f > 0. {
                            p[3] = (f64::from(p[3]) * (1. - f64::from(f))).round() as u16;
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
            e.undo("human").unwrap();
            native_layers(&e.doc, &before);
            assert_eq!(e.doc.selection_coverage, before.selection_coverage);
        }
    }
}
