use peerbrush::{
    depth16::{self, Image16},
    effects,
    engine::{Document, Engine, Scope},
    psd,
    raster::Raster,
    server,
};
use serde_json::json;
use std::sync::{Arc, Mutex};
fn native(doc: &Document) -> Vec<u16> {
    if doc.bit_depth == 16 {
        depth16::render(doc).unwrap().words
    } else {
        doc.preview(None, 64, None, false)
            .unwrap()
            .2
            .into_iter()
            .map(|b| u16::from(b) * 257)
            .collect()
    }
}
fn fixture(depth: u16) -> Engine {
    let mut e = Engine::new();
    e.doc = Document::new_depth(64, 64, depth).unwrap();
    let l = &mut e.doc.layers[0];
    l.x = 20;
    l.y = 18;
    l.pixels = Raster::new_depth(24, 24, depth);
    for y in 0..24 {
        for x in 0..24 {
            let a = if (8..12).contains(&x) && (9..13).contains(&y) {
                0
            } else {
                65535
            };
            l.pixels.set16(
                x,
                y,
                [12345 + x as u16 * 911, 23457 + y as u16 * 719, 45679, a],
            );
        }
    }
    e
}
fn strip(bytes: &[u8]) -> Vec<u8> {
    let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    [
        bytes[..at].to_vec(),
        vec![0; 4],
        bytes[at + 4 + len..].to_vec(),
    ]
    .concat()
}

#[test]
fn gradient_pixel_grid_matches_calibrated_knots_at_native_precision() {
    // Independently read knot samples from Photoshop's saved geometry BMP:
    // Patchy bb6b9426080842af6f4f65acb122516e07328323, gradient-overlay-geometry.
    // This verifies placement only, without assuming its cubic interpolation.
    let cases = [
        (
            "reflected",
            90.,
            1.46,
            [10, 10, 34, 70],
            vec![(22, 40, [83, 91, 94])],
        ),
        (
            "reflected",
            0.,
            1.46,
            [44, 10, 69, 51],
            vec![(56, 30, [83, 91, 94])],
        ),
        (
            "radial",
            90.,
            1.46,
            [80, 10, 110, 66],
            vec![(95, 38, [83, 91, 94])],
        ),
        (
            "diamond",
            90.,
            1.46,
            [160, 10, 190, 66],
            vec![(175, 38, [83, 91, 94])],
        ),
        (
            "angle",
            90.,
            1.46,
            [10, 80, 54, 130],
            vec![(32, 105, [255, 0, 0])],
        ),
        (
            "linear",
            90.,
            1.46,
            [64, 80, 92, 130],
            vec![
                (78, 105, [74, 81, 84]),
                (78, 87, [253, 253, 253]),
                (78, 123, [254, 254, 254]),
            ],
        ),
    ];
    for depth in [8, 16] {
        for (style, angle, scale, bounds, knots) in &cases {
            let mut engine = Engine::new();
            engine.doc = Document::new_depth(200, 140, depth).unwrap();
            let layer = engine.doc.layers[0].id.clone();
            for y in 0..140 {
                for x in 0..200 {
                    engine.doc.layers[0].pixels.set16(
                        x,
                        y,
                        [
                            12347,
                            23459,
                            34571,
                            if (bounds[0]..bounds[2]).contains(&x)
                                && (bounds[1]..bounds[3]).contains(&y)
                            {
                                65535
                            } else {
                                0
                            },
                        ],
                    );
                }
            }
            let source = engine.doc.layers[0].pixels.rgba16();
            let quarter = if *style == "angle" {
                [255., 0., 0.]
            } else {
                [254.; 3]
            };
            let colors = [
                [83., 91., 94.],
                quarter,
                [74., 81., 84.],
                [253.; 3],
                [83., 91., 94.],
            ];
            let stops=colors.into_iter().enumerate().map(|(i,color)|json!({"position":i as f64/4.,"midpoint":0.5,"color":color.map(|v|v/255.)})).collect::<Vec<_>>();
            let command = json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":{"style":style,"angle":angle,"scale":scale,"opacity":1.,"pixel_grid":true,"align_layer":true,"color_stops":stops}});
            let draft = Engine::preview_edits(engine.doc.clone(), &[command.clone()]).unwrap();
            assert!(engine.undo.is_empty());
            engine
                .edit(
                    "human",
                    &[command],
                    None,
                    None,
                    "Calibrated native gradient grid",
                )
                .unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                draft.export_png().unwrap()
            );
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
            let words = if depth == 16 {
                depth16::render(&engine.doc).unwrap().words
            } else {
                engine
                    .doc
                    .preview(None, 200, None, false)
                    .unwrap()
                    .2
                    .into_iter()
                    .map(|v| u16::from(v) * 257)
                    .collect()
            };
            for &(x, y, color) in knots {
                let at = ((y * 200 + x) * 4) as usize;
                assert_eq!(
                    words[at..at + 3],
                    color.map(|v| v * 257),
                    "{depth} {style} {x} {y}"
                );
            }
        }
    }
}

#[test]
fn gradient_pixel_grid_retains_legacy_sources_history_and_current_standard_saves() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(4, 2, depth).unwrap();
        for y in 0..2 {
            for x in 0..4 {
                engine.doc.layers[0].pixels.set16(
                    x,
                    y,
                    [
                        12347 + x as u16 * 101,
                        23459 + y as u16 * 107,
                        34571,
                        if x == 2 {
                            0
                        } else if x == 1 {
                            32769
                        } else {
                            65535
                        },
                    ],
                );
            }
        }
        let layer = engine.doc.layers[0].id.clone();
        engine.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"gradient_overlay","settings":{"angle":0.,"color":[0.,0.,0.],"color2":[1.,1.,1.],"opacity":1.}})],None,None,"Legacy gradient").unwrap();
        engine.undo.clear();
        let source = engine.doc.layers[0].pixels.rgba16();
        let original = engine.doc.export_png().unwrap();
        let effect = engine.doc.layers[0].effects[0].id.clone();
        let mut settings = engine.doc.layers[0].effects[0].settings.clone();
        settings["pixel_grid"] = json!(true);
        settings["align_layer"] = json!(false);
        let command =
            json!({"op":"effect.update","layer":layer,"effect":effect,"settings":settings});
        let draft = Engine::preview_edits(engine.doc.clone(), &[command.clone()]).unwrap();
        assert_ne!(draft.export_png().unwrap(), original);
        assert_eq!(engine.doc.export_png().unwrap(), original);
        assert!(engine.undo.is_empty());
        engine
            .edit("human", &[command], None, None, "Native grid placement")
            .unwrap();
        assert_eq!(engine.undo.len(), 1);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            draft.export_png().unwrap()
        );
        let current = engine.doc.export_png().unwrap();
        let words = native(&engine.doc);
        for enabled in [false, true] {
            for grid in [false, true] {
                let mut settings = engine.doc.layers[0].effects[0].settings.clone();
                settings["pixel_grid"] = json!(grid);
                let doc = Engine::preview_edits(engine.doc.clone(), &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":enabled,"settings":settings})]).unwrap();
                let current = doc.export_png().unwrap();
                for bytes in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    use std::io::Read;
                    let at = bytes.windows(4).position(|v| v == b"PBR1").unwrap() + 4;
                    let mut raw = vec![];
                    flate2::read::ZlibDecoder::new(&bytes[at..])
                        .read_to_end(&mut raw)
                        .unwrap();
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&raw).unwrap()["format"],
                        27
                    );
                    let reopened = psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                    assert_eq!(
                        reopened.layers[0].effects[0].settings,
                        doc.layers[0].effects[0].settings
                    );
                    assert_eq!(reopened.export_png().unwrap(), current);
                    let standard = psd::decode(&strip(&bytes)).unwrap();
                    assert!(!standard.read_only);
                    assert_eq!(standard.export_png().unwrap(), current);
                    assert_eq!(native(&standard), native(&doc));
                }
            }
        }
        for x in [0usize, 1, 3] {
            let expected = if depth == 16 {
                (x as f64 / 4. * 65535.).round() as u16
            } else {
                u16::from(peerbrush::raster::project16(
                    (x as f64 / 4. * 65535.).round() as u16,
                )) * 257
            };
            assert_eq!(words[x * 4], expected);
        }
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.export_png().unwrap(), original);
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
        engine.redo("human").unwrap();
        assert_eq!(engine.doc.export_png().unwrap(), current);
        let before = serde_json::to_value(&engine.doc).unwrap();
        let history = engine.undo.len();
        for invalid in [
            json!({"pixel_grid":1}),
            json!({"align_layer":true}),
            json!({"pixel_grid":false,"align_layer":true}),
            json!({"pixel_grid":true,"align_layer":1}),
        ] {
            assert!(engine.edit("human",&[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":invalid})],None,None,"Invalid grid").is_err());
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
            assert_eq!(engine.undo.len(), history);
        }
        for kind in ["color_overlay", "pattern_overlay", "drop_shadow"] {
            assert!(effects::validate(kind, &json!({"pixel_grid":true})).is_err());
        }
        let legacy = engine.doc.layers[0].effects[0].settings.clone();
        let mut disabled = legacy.clone();
        disabled["pixel_grid"] = json!(false);
        let mut removed = disabled.clone();
        removed.as_object_mut().unwrap().remove("pixel_grid");
        removed.as_object_mut().unwrap().remove("align_layer");
        let mut a = source.clone();
        let mut b = source.clone();
        effects::styles::apply(&mut a, 4, 2, "gradient_overlay", &disabled).unwrap();
        effects::styles::apply(&mut b, 4, 2, "gradient_overlay", &removed).unwrap();
        assert_eq!(a, b);
    }
}

#[test]
fn drop_shadow_source_knockout_preserves_native_sources_previews_undo_and_standard_saves() {
    for depth in [8, 16] {
        for (angle, distance, dx, dy) in
            [(0., 1f64, 1i32, 0i32), (45., 2., 1, -1), (-135., 2., -1, 1)]
        {
            let mut engine = Engine::new();
            engine.doc = Document::new_depth(4, 4, depth).unwrap();
            engine.doc.layers[0].psd_metadata.push(psd::LayerMetadata {
                key: "lyvr".into(),
                data: 160u32.to_be_bytes().to_vec(),
            });
            let alpha = [
                0u16, 1, 12347, 65535, 32768, 65534, 257, 23459, 54321, 0, 10001, 45679, 34571,
                51347, 22371, 65535,
            ];
            for i in 0..16 {
                engine.doc.layers[0].pixels.set16(
                    i % 4,
                    i / 4,
                    [
                        12347 + i as u16 * 107,
                        23459 + i as u16 * 113,
                        34571 + i as u16 * 127,
                        alpha[i as usize],
                    ],
                );
            }
            let source = engine.doc.layers[0].pixels.rgba16();
            let layer = engine.doc.layers[0].id.clone();
            let tint = [11111u16, 22223, 55555];
            engine.edit("human", &[json!({"op":"effect.add","layer":layer,"kind":"drop_shadow","settings":{"size":0.,"distance":distance,"angle":angle,"opacity":0.37,"color":tint.map(|v|f64::from(v)/65535.),"pixel_snap":true,"source_knockout":false}})], None, None, "Legacy cast").unwrap();
            let original = engine.doc.export_png().unwrap();
            let effect = engine.doc.layers[0].effects[0].id.clone();
            let mut settings = engine.doc.layers[0].effects[0].settings.clone();
            settings["source_knockout"] = json!(true);
            let command =
                json!({"op":"effect.update","layer":layer,"effect":effect,"settings":settings});
            let draft = Engine::preview_edits(engine.doc.clone(), &[command.clone()]).unwrap();
            assert_eq!(engine.undo.len(), 1);
            assert_eq!(engine.doc.export_png().unwrap(), original);
            let reference = |knockout: bool| {
                let mut output = vec![];
                for i in 0..16 {
                    let p: [u16; 4] = source[i * 4..i * 4 + 4].try_into().unwrap();
                    let (x, y) = (i as i32 % 4 - dx, i as i32 / 4 - dy);
                    let shifted = if (0..4).contains(&x) && (0..4).contains(&y) {
                        source[((y * 4 + x) * 4 + 3) as usize]
                    } else {
                        0
                    };
                    let a = f64::from(p[3]) / 65535.;
                    let shade = (f64::from(shifted) * if knockout { 1. - a } else { 1. } * 0.37)
                        .round()
                        / 65535.;
                    let total = a + shade * (1. - a);
                    let mut pixel = if total == 0. {
                        [0; 4]
                    } else {
                        let mut pixel = [0; 4];
                        for c in 0..3 {
                            pixel[c] = ((f64::from(p[c]) * a
                                + f64::from(tint[c]) * shade * (1. - a))
                                / total)
                                .round() as u16;
                        }
                        pixel[3] = (total * 65535.).round() as u16;
                        pixel
                    };
                    if depth == 8 {
                        pixel = pixel.map(|v| ((u32::from(v) + 128) / 257) as u16 * 257);
                    }
                    if pixel[3] == 0 {
                        pixel = [0; 4];
                    }
                    output.extend(pixel);
                }
                output
            };
            assert_eq!(native(&engine.doc), reference(false));
            assert_eq!(native(&draft), reference(true));
            assert_ne!(draft.export_png().unwrap(), original);
            engine
                .edit("human", &[command], None, None, "Source knockout")
                .unwrap();
            assert_eq!(
                engine.doc.export_png().unwrap(),
                draft.export_png().unwrap()
            );
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
            for enabled in [false, true] {
                let mut doc = engine.doc.clone();
                doc.layers[0].effects[0].enabled = enabled;
                let current = doc.export_png().unwrap();
                for bytes in [psd::encode(&doc).unwrap(), psd::encode_psb(&doc).unwrap()] {
                    use std::io::Read;
                    let at = bytes.windows(4).position(|v| v == b"PBR1").unwrap() + 4;
                    let mut decoded = vec![];
                    flate2::read::ZlibDecoder::new(&bytes[at..])
                        .read_to_end(&mut decoded)
                        .unwrap();
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&decoded).unwrap()["format"],
                        26
                    );
                    let reopened = psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                    assert_eq!(reopened.layers[0].effects[0].settings, settings);
                    assert_eq!(reopened.export_png().unwrap(), current);
                    assert_eq!(
                        psd::decode(&strip(&bytes)).unwrap().export_png().unwrap(),
                        current
                    );
                }
            }
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.export_png().unwrap(), original);
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), source);
            let mut legacy = engine.doc.clone();
            legacy.layers[0].effects[0]
                .settings
                .as_object_mut()
                .unwrap()
                .remove("source_knockout");
            assert_eq!(legacy.export_png().unwrap(), original);
            let bytes = psd::encode(&legacy).unwrap();
            assert_eq!(psd::decode(&bytes).unwrap().export_png().unwrap(), original);
        }
    }
}

#[test]
fn drop_shadow_source_knockout_validation_reservations_and_protocol_are_atomic() {
    let shared = Arc::new(Mutex::new(fixture(16)));
    let before = shared.lock().unwrap().doc.clone();
    let layer = before.layers[0].id.clone();
    for (kind, value) in [
        ("drop_shadow", json!(null)),
        ("drop_shadow", json!(1)),
        ("drop_shadow", json!("yes")),
        ("inner_shadow", json!(true)),
        ("outer_glow", json!(false)),
    ] {
        assert!(server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"layer.update","layer":layer,"opacity":0.3},{"op":"effect.add","layer":layer,"kind":kind,"settings":{"source_knockout":value}}]})).is_err());
        let engine = shared.lock().unwrap();
        assert_eq!(
            engine.doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
        assert_eq!(engine.doc.revision, before.revision);
        assert!(engine.undo.is_empty());
    }
    {
        let mut engine = shared.lock().unwrap();
        engine
            .reserve(
                "other",
                "Synthetic knockout lease",
                vec![Scope {
                    target: Some(layer.clone()),
                    rect: None,
                }],
            )
            .unwrap();
        assert!(engine.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"drop_shadow","settings":{"source_knockout":true}})],None,None,"Reserved knockout").is_err());
        engine.leases.clear();
        engine.doc.layers[0].locked = true;
        assert!(engine.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"drop_shadow","settings":{"source_knockout":true}})],None,None,"Locked knockout").is_err());
        engine.doc.layers[0].locked = false;
    }
    let reply=server::dispatch(&shared,"edit",&json!({"actor":"human","document_id":before.id,"expected_revision":before.revision,"commands":[{"op":"effect.add","layer":layer,"kind":"drop_shadow","settings":{"source_knockout":true,"size":0.,"distance":1.}}],"max_edge":64})).unwrap();
    assert!(!reply["images"][0]["data"].as_str().unwrap().is_empty());
    assert_eq!(reply["images"][0]["document_rect"], json!([0, 0, 64, 64]));
    let engine = shared.lock().unwrap();
    assert_eq!(
        engine.doc.layers[0].effects[0].settings["source_knockout"],
        true
    );
    assert_eq!(
        engine.doc.layers[0].pixels.rgba16(),
        before.layers[0].pixels.rgba16()
    );
    assert_eq!(engine.undo.len(), 1);
}

#[test]
fn pixel_aligned_shadows_preserve_native_samples_previews_undo_and_standard_saves() {
    for depth in [8, 16] {
        for kind in ["drop_shadow", "inner_shadow"] {
            for (angle, dx, dy) in [(-135., -1, 1), (-45., 1, 1), (45., 1, -1), (135., -1, -1)] {
                let mut e = Engine::new();
                e.doc = Document::new_depth(7, 7, depth).unwrap();
                e.doc.layers[0].psd_metadata.push(psd::LayerMetadata {
                    key: "lyvr".into(),
                    data: 160u32.to_be_bytes().to_vec(),
                });
                for y in 0..7 {
                    for x in 0..7 {
                        let alpha =
                            if (2..5).contains(&x) && (2..5).contains(&y) && (x, y) != (3, 3) {
                                65535
                            } else {
                                0
                            };
                        e.doc.layers[0].pixels.set16(
                            x,
                            y,
                            [12345 + x as u16 * 137, 23457 + y as u16 * 193, 45679, alpha],
                        );
                    }
                }
                let source = e.doc.layers[0].pixels.rgba16();
                let layer = e.doc.layers[0].id.clone();
                let base = e.doc.export_png().unwrap();
                let mut settings = json!({"size":0.,"distance":2.,"angle":angle,"opacity":0.37,"color":[11111f64/65535.,22223f64/65535.,55555f64/65535.],"pixel_snap":false});
                if kind == "inner_shadow" {
                    settings["blend"] = json!("normal");
                }
                e.edit(
                    "human",
                    &[json!({"op":"effect.add","layer":layer,"kind":kind,"settings":settings})],
                    None,
                    None,
                    "Fractional shadow",
                )
                .unwrap();
                let fractional = e.doc.export_png().unwrap();
                let effect = e.doc.layers[0].effects[0].id.clone();
                let mut settings = e.doc.layers[0].effects[0].settings.clone();
                settings["pixel_snap"] = json!(true);
                let command =
                    json!({"op":"effect.update","layer":layer,"effect":effect,"settings":settings});
                let preview = Engine::preview_edits(e.doc.clone(), &[command.clone()]).unwrap();
                assert_eq!(e.undo.len(), 1);
                assert_eq!(e.doc.export_png().unwrap(), fractional);
                e.edit("human", &[command], None, None, "Pixel aligned shadow")
                    .unwrap();
                assert_eq!(e.doc.export_png().unwrap(), preview.export_png().unwrap());
                assert_ne!(e.doc.export_png().unwrap(), fractional);
                assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
                let mut expected = vec![];
                let weight = (0.37f64 * 65535.).round() / 65535.;
                for y in 0..7i32 {
                    for x in 0..7i32 {
                        let at = ((y * 7 + x) * 4) as usize;
                        let original: [u16; 4] = source[at..at + 4].try_into().unwrap();
                        let sx = x - dx;
                        let sy = y - dy;
                        let cast = if (0..7).contains(&sx) && (0..7).contains(&sy) {
                            source[((sy * 7 + sx) * 4 + 3) as usize]
                        } else {
                            0
                        };
                        let mut pixel = original;
                        if kind == "drop_shadow" && original[3] == 0 && cast != 0 {
                            pixel = [11111, 22223, 55555, (weight * 65535.).round() as u16];
                        } else if kind == "inner_shadow" && original[3] != 0 && cast == 0 {
                            for c in 0..3 {
                                pixel[c] = ((f64::from(original[c]) / 65535. * (1. - weight)
                                    + f64::from([11111, 22223, 55555][c]) / 65535. * weight)
                                    * 65535.)
                                    .round() as u16;
                            }
                        }
                        if depth == 8 {
                            pixel = pixel.map(|v| ((u32::from(v) + 128) / 257) as u16 * 257);
                        }
                        if pixel[3] == 0 {
                            pixel = [0; 4];
                        }
                        expected.extend(pixel);
                    }
                }
                assert_eq!(native(&e.doc), expected, "{depth} {kind} {angle}");
                let current = e.doc.export_png().unwrap();
                for enabled in [false, true] {
                    let mut document = e.doc.clone();
                    document.layers[0].effects[0].enabled = enabled;
                    let appearance = document.export_png().unwrap();
                    for bytes in [
                        psd::encode(&document).unwrap(),
                        psd::encode_psb(&document).unwrap(),
                    ] {
                        use std::io::Read;
                        let at = bytes.windows(4).position(|v| v == b"PBR1").unwrap() + 4;
                        let mut decoded = vec![];
                        flate2::read::ZlibDecoder::new(&bytes[at..])
                            .read_to_end(&mut decoded)
                            .unwrap();
                        assert_eq!(
                            serde_json::from_slice::<serde_json::Value>(&decoded).unwrap()
                                ["format"],
                            if kind == "drop_shadow" { 26 } else { 25 }
                        );
                        let reopened = psd::decode(&bytes).unwrap();
                        assert!(!reopened.read_only);
                        assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                        assert_eq!(
                            reopened.layers[0].effects[0].settings,
                            e.doc.layers[0].effects[0].settings
                        );
                        assert_eq!(
                            reopened.layers[0].psd_metadata,
                            e.doc.layers[0].psd_metadata
                        );
                        assert_eq!(reopened.export_png().unwrap(), appearance);
                        let standard = psd::decode(&strip(&bytes)).unwrap();
                        assert!(!standard.read_only);
                        assert_eq!(standard.export_png().unwrap(), appearance);
                    }
                }
                assert_eq!(e.doc.export_png().unwrap(), current);
                assert_eq!(e.undo.len(), 2);
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), fractional);
                e.undo("human").unwrap();
                assert_eq!(e.doc.export_png().unwrap(), base);
                assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
            }
        }
    }
}

#[test]
fn pixel_aligned_shadow_validation_reservations_and_protocol_remain_atomic() {
    let shared = Arc::new(Mutex::new(fixture(16)));
    let source = shared.lock().unwrap().doc.clone();
    let layer = source.layers[0].id.clone();
    for (kind, value) in [
        ("inner_shadow", json!(null)),
        ("drop_shadow", json!(1)),
        ("drop_shadow", json!("yes")),
        ("color_overlay", json!(true)),
        ("satin", json!(false)),
    ] {
        let result = server::dispatch(
            &shared,
            "edit",
            &json!({"actor":"human","commands":[{"op":"layer.update","layer":layer,"opacity":0.3},{"op":"effect.add","layer":layer,"kind":kind,"settings":{"pixel_snap":value}}]}),
        );
        assert!(result.is_err());
        let e = shared.lock().unwrap();
        assert_eq!(e.doc.revision, source.revision);
        assert_eq!(e.doc.export_png().unwrap(), source.export_png().unwrap());
        assert!(e.undo.is_empty());
    }
    {
        let mut e = shared.lock().unwrap();
        e.reserve(
            "other",
            "Synthetic shadow lease",
            vec![Scope {
                target: Some(layer.clone()),
                rect: Some([0, 0, 64, 64]),
            }],
        )
        .unwrap();
        assert!(e.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"drop_shadow","settings":{"pixel_snap":true}})],None,None,"Reserved pixel shadow").is_err());
        assert_eq!(e.doc.revision, source.revision);
        e.leases.clear();
        e.doc.layers[0].locked = true;
        assert!(e.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"inner_shadow","settings":{"pixel_snap":true}})],None,None,"Locked pixel shadow").is_err());
        e.doc.layers[0].locked = false;
    }
    let reply=server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"effect.add","layer":layer,"kind":"drop_shadow","settings":{"pixel_snap":true,"size":0.,"angle":45.,"distance":2.}}],"max_edge":64})).unwrap();
    assert!(!reply["images"][0]["data"].as_str().unwrap().is_empty());
    assert_eq!(reply["images"][0]["document_rect"], json!([0, 0, 64, 64]));
    assert_eq!(
        shared.lock().unwrap().doc.layers[0].pixels.rgba16(),
        source.layers[0].pixels.rgba16()
    );
    assert_eq!(shared.lock().unwrap().undo.len(), 1);
}
#[test]
fn every_style_renders_native_sources_preserves_source_and_roundtrips_actual_standard_pixels() {
    for depth in [8, 16] {
        for kind in effects::styles::KINDS {
            let mut e = fixture(depth);
            let source = e.doc.layers[0].pixels.rgba16();
            let before = native(&e.doc);
            let layer = e.doc.layers[0].id.clone();
            e.edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":kind})],
                None,
                None,
                "Layer style",
            )
            .unwrap();
            let after = native(&e.doc);
            assert_ne!(after, before, "{kind} depth {depth}");
            assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
            assert_eq!(e.undo.len(), 1);
            let saved = psd::encode(&e.doc).unwrap();
            let large = psd::encode_psb(&e.doc).unwrap();
            let large_loaded = psd::decode(&large).unwrap();
            assert!(!large_loaded.read_only);
            assert_eq!(native(&large_loaded), after, "{kind} PSB reopen");
            let large_standard = psd::decode(&strip(&large)).unwrap();
            assert!(!large_standard.read_only);
            assert_eq!(native(&large_standard), after, "{kind} standard PSB pixels");
            let loaded = psd::decode(&saved).unwrap();
            assert!(!loaded.read_only);
            assert_eq!(loaded.layers[0].pixels.rgba16(), source);
            assert_eq!(native(&loaded), after, "{kind} editable reopen");
            let standard = psd::decode(&strip(&saved)).unwrap();
            assert!(!standard.read_only, "{kind} standard reopen");
            assert_eq!(
                native(&standard),
                after,
                "{kind} standard rendered channels"
            );
            let id = e.doc.layers[0].effects[0].id.clone();
            e.edit(
                "human",
                &[json!({"op":"effect.update","layer":layer,"effect":id,"enabled":false})],
                None,
                None,
                "Bypass style",
            )
            .unwrap();
            assert_eq!(native(&e.doc), before, "{kind} bypass");
            e.undo("human").unwrap();
            assert_eq!(native(&e.doc), after);
            e.undo("human").unwrap();
            assert_eq!(native(&e.doc), before);
            e.redo("human").unwrap();
            assert_eq!(native(&e.doc), after);
        }
    }
}
#[test]
fn overlay_and_circular_outline_have_analytic_native_color_alpha_and_extent() {
    let mut image = Image16 {
        width: 2,
        height: 1,
        words: vec![12345, 23457, 34569, 27123, 11111, 22223, 33337, 0],
    };
    depth16::color::apply(
        &mut image,
        "color_overlay",
        &json!({"color":[1.,0.,0.],"opacity":1.}),
    )
    .unwrap();
    assert_eq!(image.words, [65535, 0, 0, 27123, 11111, 22223, 33337, 0]);
    let before = image.words.clone();
    depth16::color::apply(&mut image, "drop_shadow", &json!({"opacity":0.})).unwrap();
    assert_eq!(image.words, before);
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    e.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"stroke","settings":{"size":3.,"opacity":1.,"color":[1.,0.,0.]}})],None,None,"Outline").unwrap();
    let render = depth16::render(&e.doc).unwrap();
    assert_eq!(render.get(18, 24), [65535, 0, 0, 65535]);
    assert_eq!(render.get(16, 24)[3], 0);
    assert_eq!(
        render.get(17, 15)[3],
        0,
        "Circular distance must not produce square corners"
    );
    assert_eq!(
        e.doc.layers[0].pixels.width, 24,
        "Derived expansion must not replace source pixels"
    );
}
#[test]
fn style_stack_preview_order_masks_weight_locks_and_reservations_share_the_engine() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        e.edit("human",&[json!({"op":"mask.add","layer":layer}),json!({"op":"effect.add","layer":layer,"kind":"color_overlay","settings":{"color":[1.,0.,0.],"opacity":1.}}),json!({"op":"effect.add","layer":layer,"kind":"color_overlay","settings":{"color":[0.,0.,1.],"opacity":1.}})],None,None,"Stack").unwrap();
        let blue = native(&e.doc);
        let at = (25 * 64 + 25) * 4;
        assert_eq!(&blue[at..at + 3], &[0, 0, 65535]);
        let first = e.doc.layers[0].effects[0].id.clone();
        let second = e.doc.layers[0].effects[1].id.clone();
        let commands = [json!({"op":"effect.reorder","layer":layer,"effect":first,"index":1})];
        let rendered = Engine::preview_edits(e.doc.clone(), &commands).unwrap();
        assert_eq!(&native(&rendered)[at..at + 3], &[65535, 0, 0]);
        assert_eq!(native(&e.doc), blue, "Preview must not commit");
        e.edit("human", &commands, None, None, "Reorder").unwrap();
        assert_eq!(native(&e.doc), native(&rendered));
        e.edit(
            "human",
            &[json!({"op":"effect.update","layer":layer,"effect":first,"weight":0.})],
            None,
            None,
            "Weight",
        )
        .unwrap();
        assert_eq!(native(&e.doc), blue);
        let before = psd::encode(&e.doc).unwrap();
        e.reserve(
            "other",
            "Protected exterior",
            vec![Scope {
                target: Some(layer.clone()),
                rect: Some([0, 0, 18, 18]),
            }],
        )
        .unwrap();
        assert!(e
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":"drop_shadow"})],
                None,
                None,
                "Reserved"
            )
            .is_err());
        assert_eq!(psd::encode(&e.doc).unwrap(), before);
        e.leases.clear();
        e.doc.layers[0].locked = true;
        assert!(e
            .edit(
                "human",
                &[json!({"op":"effect.delete","layer":layer,"effect":second})],
                None,
                None,
                "Locked"
            )
            .is_err());
    }
}
#[test]
fn invalid_style_parameters_masks_and_work_budgets_reject_atomically_and_protocol_returns_pixels() {
    let shared = Arc::new(Mutex::new(fixture(16)));
    let source = shared.lock().unwrap().doc.clone();
    let layer = source.layers[0].id.clone();
    for (kind, settings) in [
        ("stroke", json!({"size":65})),
        ("color_overlay", json!({"color":[1.,0.]})),
        ("pattern_overlay", json!({"pattern":"unsupported"})),
        ("drop_shadow", json!({"distance":129})),
        ("texture", json!({"unused":1})),
    ] {
        let result = server::dispatch(
            &shared,
            "edit",
            &json!({"actor":"human","commands":[{"op":"effect.add","layer":layer,"kind":kind,"settings":settings}]}),
        );
        assert!(result.is_err(), "{kind}");
        assert_eq!(shared.lock().unwrap().doc.revision, source.revision);
    }
    assert!(effects::styles::working_bytes(4096, 4096) > effects::WORKING_BUDGET as u64);
    let mut e = shared.lock().unwrap();
    e.edit(
        "human",
        &[json!({"op":"mask.add","layer":layer})],
        None,
        None,
        "Mask",
    )
    .unwrap();
    let rev = e.doc.revision;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"mask.step.add","layer":layer,"kind":"stroke"})],
            None,
            None,
            "Unsupported mask style"
        )
        .is_err());
    assert_eq!(e.doc.revision, rev);
    drop(e);
    let result=server::dispatch(&shared,"edit",&json!({"actor":"human","commands":[{"op":"effect.add","layer":layer,"kind":"drop_shadow"}],"max_edge":64})).unwrap();
    assert!(!result["images"][0]["data"].as_str().unwrap().is_empty());
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 64, 64]));
}

#[test]
fn styled_masks_copy_merge_and_zero_opacity_prefix_keep_actual_canvas_pixels() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        e.edit("human",&[json!({"op":"mask.add","layer":layer}),json!({"op":"mask.step.add","layer":layer,"kind":"fill","value":127}),json!({"op":"effect.add","layer":layer,"kind":"drop_shadow","settings":{"size":4,"distance":8}})],None,None,"Masked shadow").unwrap();
        let expected = native(&e.doc);
        let copied = peerbrush::clipboard::Image::copy(&e.doc, &layer, false, false).unwrap();
        let words = if depth == 16 {
            copied.samples16.unwrap()
        } else {
            copied
                .bytes
                .into_iter()
                .map(|v| u16::from(v) * 257)
                .collect()
        };
        for (a, b) in words.chunks_exact(4).zip(expected.chunks_exact(4)) {
            assert_eq!(a[3], b[3], "Masked exterior copy alpha");
            if a[3] != 0 {
                assert_eq!(a, b, "Masked exterior copy samples");
            }
        }
        let standard = psd::decode(&strip(&psd::encode(&e.doc).unwrap())).unwrap();
        assert_eq!(native(&standard), expected, "Standard expanded mask frame");
        let mut blank = peerbrush::engine::Layer::new("Empty lower layer", "paint", 64, 64);
        blank.pixels = Raster::new_depth(64, 64, depth);
        e.doc.layers.push(blank);
        e.edit(
            "human",
            &[json!({"op":"layer.merge","layers":[layer]})],
            None,
            None,
            "Merge styled layer",
        )
        .unwrap();
        assert_eq!(
            native(&e.doc),
            expected,
            "Merge must retain exterior effect pixels"
        );
        e.undo("human").unwrap();
        assert_eq!(native(&e.doc), expected);
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[json!({"op":"effect.add","layer":layer,"kind":"blur","settings":{"radius":3}})],
            None,
            None,
            "Prefix",
        )
        .unwrap();
        let before = native(&e.doc);
        e.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"color_overlay","settings":{"opacity":0}})],None,None,"Zero opacity style").unwrap();
        assert_eq!(
            native(&e.doc),
            before,
            "Expanding before the first style must not rerun preceding effects on another frame"
        );
    }
}

#[test]
fn oversized_style_document_frame_rejects_before_mutating_cropped_sources() {
    let mut e = fixture(16);
    e.doc.width = 4096;
    e.doc.height = 4096;
    let layer = e.doc.layers[0].id.clone();
    let source = e.doc.layers[0].pixels.rgba16();
    let revision = e.doc.revision;
    assert!(e
        .edit(
            "human",
            &[json!({"op":"effect.add","layer":layer,"kind":"stroke"})],
            None,
            None,
            "Oversized style"
        )
        .unwrap_err()
        .to_lowercase()
        .contains("budget"));
    assert!(e.doc.layers[0].effects.is_empty());
    assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
    assert_eq!(e.doc.revision, revision);
    assert!(e.undo.is_empty());
}

#[test]
fn isolated_folder_styles_render_children_and_adjustments_reject_unsupported_alpha_sources() {
    for depth in [8, 16] {
        let mut e = fixture(depth);
        let layer = e.doc.layers[0].id.clone();
        e.edit(
            "human",
            &[json!({"op":"group.create_selected","layers":[layer]})],
            None,
            None,
            "Folder",
        )
        .unwrap();
        let group = e
            .doc
            .layers
            .iter()
            .find(|l| l.kind == "group")
            .unwrap()
            .id
            .clone();
        let before = native(&e.doc);
        e.edit(
            "human",
            &[json!({"op":"effect.add","layer":group,"kind":"drop_shadow"})],
            None,
            None,
            "Folder shadow",
        )
        .unwrap();
        let expected = native(&e.doc);
        assert_ne!(expected, before);
        let saved = psd::encode(&e.doc).unwrap();
        assert_eq!(native(&psd::decode(&saved).unwrap()), expected);
        assert_eq!(native(&psd::decode(&strip(&saved)).unwrap()), expected);
        e.undo("human").unwrap();
        assert_eq!(native(&e.doc), before);
        let mut adjustment = peerbrush::engine::Layer::new("Adjustment", "adjustment", 64, 64);
        adjustment.pixels = Raster::new_depth(64, 64, depth);
        let id = adjustment.id.clone();
        e.doc.layers.insert(0, adjustment);
        let revision = e.doc.revision;
        assert!(e
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":id,"kind":"outer_glow"})],
                None,
                None,
                "Unsupported adjustment style"
            )
            .is_err());
        assert_eq!(e.doc.revision, revision);
        assert!(e.doc.layers[0].effects.is_empty());
    }
}

#[test]
fn inapplicable_style_settings_reject_instead_of_exposing_no_op_controls() {
    for (kind, key, value) in [
        ("pattern_overlay", "depth", json!(0.4)),
        ("pattern_overlay", "angle", json!(45.)),
        ("outer_glow", "source", json!("center")),
    ] {
        let mut e = fixture(16);
        let source = e.doc.layers[0].pixels.rgba16();
        let revision = e.doc.revision;
        let layer = e.doc.layers[0].id.clone();
        let mut settings = json!({});
        settings[key] = value;
        assert!(e
            .edit(
                "human",
                &[json!({"op":"effect.add","layer":layer,"kind":kind,"settings":settings})],
                None,
                None,
                "Invalid control"
            )
            .is_err());
        assert_eq!(e.doc.revision, revision);
        assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
        assert!(e.doc.layers[0].effects.is_empty());
        assert!(e.undo.is_empty());
    }
}

#[test]
fn bevel_highlight_and_shadow_channels_have_independent_native_blends_and_coverage() {
    for depth in [8, 16] {
        let adapt = |n: u16| {
            if depth == 16 {
                n
            } else {
                u16::from(peerbrush::raster::project16(n)) * 257
            }
        };
        let source = [
            [12347, 23459, 34571, 0],
            [12347, 23459, 34571, 65535],
            [23459, 34571, 45683, 65535],
            [54321, 32123, 11223, 0],
        ]
        .map(|p| p.map(adapt));
        let mut words = source.into_iter().flatten().collect::<Vec<_>>();
        let settings = json!({"style":"inner","size":0.,"soften":0.,"angle":0.,"altitude":0.,"depth":1.,"opacity":0.8,"color":[0.2,0.4,0.7],"color2":[0.9,0.3,0.1],"highlight_opacity":0.25,"shadow_opacity":0.75,"highlight_blend":"screen","shadow_blend":"multiply"});
        effects::styles::apply(&mut words, 4, 1, "bevel_emboss", &settings).unwrap();
        for (index, color, opacity, screen) in [
            (1, [0.2, 0.4, 0.7], 0.2, true),
            (2, [0.9, 0.3, 0.1], 0.6, false),
        ] {
            let alpha = (opacity * 65535f64).round() / 65535.;
            for c in 0..3 {
                let value = f64::from(source[index][c]) / 65535.;
                let tint = (color[c] * 65535f64).round() / 65535.;
                let blended = if screen {
                    1. - (1. - value) * (1. - tint)
                } else {
                    value * tint
                };
                let expected = ((value * (1. - alpha) + blended * alpha) * 65535.).round() as u16;
                // The byte adapter projects only after native style processing.
                let actual = if depth == 16 {
                    words[index * 4 + c]
                } else {
                    adapt(words[index * 4 + c])
                };
                assert_eq!(
                    actual,
                    adapt(expected),
                    "depth {depth}, pixel {index}, channel {c}"
                );
            }
            assert_eq!(words[index * 4 + 3], 65535);
        }
        assert_eq!(&words[..4], &source[0]);
        assert_eq!(&words[12..], &source[3]);
    }
}
#[test]
fn legacy_bevel_settings_inherit_common_blend_and_opacity_without_changed_sources() {
    for depth in [8, 16] {
        for style in ["inner", "outer", "emboss", "pillow"] {
            for blend in ["normal", "multiply"] {
                let mut e = fixture(depth);
                let layer = e.doc.layers[0].id.clone();
                let legacy = json!({"style":style,"blend":blend,"opacity":0.35});
                e.edit("human",&[json!({"op":"effect.add","layer":layer,"kind":"bevel_emboss","settings":legacy})],None,None,"Legacy relief").unwrap();
                let effect = e.doc.layers[0].effects[0].id.clone();
                // Old editable sources did not contain independent channel properties.
                for key in [
                    "highlight_opacity",
                    "shadow_opacity",
                    "highlight_blend",
                    "shadow_blend",
                ] {
                    e.doc.layers[0].effects[0]
                        .settings
                        .as_object_mut()
                        .unwrap()
                        .remove(key);
                }
                let before = native(&e.doc);
                let source = e.doc.layers[0].pixels.rgba16();
                let mut explicit = legacy.clone();
                explicit["highlight_opacity"] = json!(1.);
                explicit["shadow_opacity"] = json!(1.);
                explicit["highlight_blend"] = json!("inherit");
                explicit["shadow_blend"] = json!("inherit");
                let preview=Engine::preview_edits(e.doc.clone(),&[json!({"op":"effect.update","layer":layer,"effect":effect,"settings":explicit})]).unwrap();
                assert_eq!(native(&preview), before);
                assert_eq!(preview.layers[0].pixels.rgba16(), source);
                assert!(e.doc.layers[0].effects[0]
                    .settings
                    .get("highlight_opacity")
                    .is_none());
            }
        }
    }
}
#[test]
fn independent_bevel_settings_preview_commit_once_and_roundtrip_current_native_sources() {
    for depth in [8, 16] {
        for disabled in [false, true] {
            let mut e = fixture(depth);
            let layer = e.doc.layers[0].id.clone();
            let source = e.doc.layers[0].pixels.rgba16();
            let before = e.doc.export_png().unwrap();
            let settings = json!({"highlight_opacity":0.35,"shadow_opacity":0.7,"highlight_blend":"screen","shadow_blend":"multiply"});
            let command = json!({"op":"effect.add","layer":layer,"kind":"bevel_emboss","settings":settings,"enabled":!disabled});
            // Add uses the normal enabled default; bypass is a separate definition property.
            e.edit("human", &[command], None, None, "Bevel channels")
                .unwrap();
            let effect = e.doc.layers[0].effects[0].id.clone();
            if disabled {
                e.edit(
                    "human",
                    &[json!({"op":"effect.update","layer":layer,"effect":effect,"enabled":false})],
                    None,
                    None,
                    "Bevel bypass",
                )
                .unwrap();
            }
            e.undo.clear();
            let baseline = e.doc.clone();
            let command = json!({"op":"effect.update","layer":layer,"effect":effect,"settings":{"highlight_opacity":0.61,"shadow_opacity":0.42,"highlight_blend":"overlay","shadow_blend":"darken"}});
            let preview =
                Engine::preview_edits(baseline.clone(), std::slice::from_ref(&command)).unwrap();
            e.edit("human", &[command], None, None, "Independent relief")
                .unwrap();
            assert_eq!(e.undo.len(), 1);
            assert_eq!(native(&e.doc), native(&preview));
            assert_eq!(e.doc.layers[0].pixels.rgba16(), source);
            let current = e.doc.export_png().unwrap();
            if !disabled {
                assert_ne!(current, before);
            }
            for bytes in [
                psd::encode(&e.doc).unwrap(),
                psd::encode_psb(&e.doc).unwrap(),
            ] {
                let at = 30 + u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
                let resources = &bytes[at + 4..];
                assert_eq!(&resources[..6], b"8BIM\x0f\xa0");
                let count = 6 + (usize::from(resources[6]) + 1).div_ceil(2) * 2;
                let len =
                    u32::from_be_bytes(resources[count..count + 4].try_into().unwrap()) as usize;
                let payload = &resources[count + 4..count + 4 + len];
                assert_eq!(&payload[..4], b"PBR1");
                use std::io::Read;
                let mut decoded = Vec::new();
                flate2::read::ZlibDecoder::new(&payload[4..])
                    .read_to_end(&mut decoded)
                    .unwrap();
                let source_json: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
                assert_eq!(source_json["format"], 22);
                let reopened = psd::decode(&bytes).unwrap();
                assert!(!reopened.read_only);
                assert_eq!(reopened.export_png().unwrap(), current);
                assert_eq!(reopened.layers[0].pixels.rgba16(), source);
                assert_eq!(
                    reopened.layers[0].effects[0].settings,
                    e.doc.layers[0].effects[0].settings
                );
                assert_eq!(reopened.layers[0].effects[0].enabled, !disabled);
                let standard = psd::decode(&strip(&bytes)).unwrap();
                assert!(!standard.read_only);
                assert_eq!(standard.export_png().unwrap(), current);
            }
            e.undo("human").unwrap();
            assert_eq!(native(&e.doc), native(&baseline));
            e.redo("human").unwrap();
            assert_eq!(e.doc.export_png().unwrap(), current);
        }
    }
}
#[test]
fn invalid_or_inapplicable_bevel_channel_settings_rollback_native_work_and_history() {
    let mut e = fixture(16);
    let layer = e.doc.layers[0].id.clone();
    let before = serde_json::to_value(&e.doc).unwrap();
    for settings in [
        json!({"highlight_opacity":-0.1}),
        json!({"shadow_opacity":1.1}),
        json!({"highlight_opacity":"bad"}),
        json!({"highlight_blend":"unsupported"}),
        json!({"shadow_blend":17}),
    ] {
        assert!(e.edit("human",&[json!({"op":"paint.fill","layer":layer,"rect":[0,0,1,1],"color":[255,0,0,255]}),json!({"op":"effect.add","layer":layer,"kind":"bevel_emboss","settings":settings})],None,None,"Invalid bevel channel").is_err());
        assert_eq!(serde_json::to_value(&e.doc).unwrap(), before);
        assert!(e.undo.is_empty());
    }
    for kind in ["levels", "drop_shadow", "contour", "stroke"] {
        assert!(effects::validate(kind, &json!({"highlight_opacity":0.5})).is_err());
    }
}
