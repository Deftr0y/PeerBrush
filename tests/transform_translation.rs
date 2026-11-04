use peerbrush::{
    engine::{Document, Engine, Scope},
    psd,
    raster::Raster,
    selection,
    source::{Content, Source},
    transform,
};
use serde_json::{json, Value};

fn fixture(depth: u16, selected: bool) -> Engine {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(64, 48, depth).unwrap();
    engine.doc.layers[0].pixels = Raster::new_depth(16, 14, depth);
    let id = engine.doc.layers[0].id.clone();
    engine
        .edit(
            "human",
            &[json!({"op":"mask.add","layer":id})],
            None,
            None,
            "Mask",
        )
        .unwrap();
    let layer = &mut engine.doc.layers[0];
    for y in 0..14 {
        for x in 0..16 {
            layer.pixels.set16(
                x,
                y,
                [
                    12347 + x as u16 * 97,
                    23459 + y as u16 * 193,
                    34571,
                    if (x + y) % 13 == 0 { 0 } else { 65535 },
                ],
            );
            let gray = 12001 + x as u16 * 151 + y as u16 * 137;
            layer.mask.as_mut().unwrap().steps[1]
                .pixels
                .set16(x, y, [gray, gray, gray, 65535]);
        }
    }
    if selected {
        engine
            .edit(
                "human",
                &[json!({"op":"selection","rect":[2,2,10,10],"feather":1.})],
                None,
                None,
                "Feathered selection",
            )
            .unwrap();
    }
    engine.undo.clear();
    engine
}
fn command(engine: &Engine, fields: Value) -> Value {
    let mut command = json!({"op":"transform","layer":engine.doc.layers[0].id,"pivot":[24.,16.]});
    command
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    command
}
fn edit(engine: &mut Engine, command: Value) {
    engine
        .edit(
            "human",
            &[command],
            Some(engine.doc.revision),
            None,
            "Affine placement",
        )
        .unwrap();
}
fn state(engine: &Engine) -> Value {
    let history = |entries: &[peerbrush::engine::History]| {
        entries.iter().map(|entry|json!({
        "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
        "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted
    })).collect::<Vec<_>>()
    };
    json!({"document":engine.doc,"undo":history(&engine.undo),"redo":history(&engine.redo)})
}
fn color(engine: &Engine, x: i32, y: i32) -> [u16; 4] {
    let layer = &engine.doc.layers[0];
    layer.pixels.get16(x - layer.x, y - layer.y)
}
fn mask(engine: &Engine, x: i32, y: i32) -> [u16; 4] {
    let layer = &engine.doc.layers[0];
    layer.mask.as_ref().unwrap().steps[1]
        .pixels
        .get16(x - layer.x, y - layer.y)
}

#[test]
fn combined_selected_translation_uses_one_original_soft_coverage_sample_and_preserves_remainders() {
    for depth in [8, 16] {
        for mask_only in [false, true] {
            let mut translated = fixture(depth, true);
            if mask_only {
                let id = translated.doc.layers[0].id.clone();
                edit(
                    &mut translated,
                    json!({"op":"mask.link","layer":id,"linked":false}),
                );
                translated.undo.clear();
            }
            let before = translated.doc.clone();
            let mut rotation = translated.clone();
            let rotate = command(&rotation, json!({"angle":90.,"mask":mask_only}));
            edit(&mut rotation, rotate);
            let combined = command(
                &translated,
                json!({"angle":90.,"dx":8.,"dy":12.,"mask":mask_only}),
            );
            let preview = Engine::preview_edits(before.clone(), &[combined.clone()]).unwrap();
            assert_eq!(
                translated.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
            edit(&mut translated, combined);
            assert_eq!(translated.undo.len(), 1);
            assert_eq!(
                translated.doc.export_png().unwrap(),
                preview.export_png().unwrap()
            );
            // Rotation moves the selected tile away from the original area.
            // Compare its exact native destination with the established rotation,
            // translated as coordinates rather than cut/filtering that result again.
            let bounds = rotation.doc.selection.unwrap();
            for y in bounds[1]..bounds[3] {
                for x in bounds[0]..bounds[2] {
                    assert_eq!(color(&translated, x + 8, y + 12), color(&rotation, x, y));
                    assert_eq!(mask(&translated, x + 8, y + 12), mask(&rotation, x, y));
                }
            }
            for y in 0..14 {
                for x in 0..16 {
                    assert_eq!(color(&translated, x, y), color(&rotation, x, y));
                    assert_eq!(mask(&translated, x, y), mask(&rotation, x, y));
                }
            }
            let expected_coverage = selection::current(&rotation.doc).unwrap();
            let actual_coverage = selection::current(&translated.doc).unwrap();
            for y in bounds[1]..bounds[3] {
                for x in bounds[0]..bounds[2] {
                    assert_eq!(
                        actual_coverage.value(x + 8, y + 12),
                        expected_coverage.value(x, y)
                    );
                }
            }
            translated.undo("human").unwrap();
            assert_eq!(
                translated.doc.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(
                translated.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
            translated.redo("human").unwrap();
            assert_eq!(
                translated.doc.export_png().unwrap(),
                preview.export_png().unwrap()
            );
        }
        let mut legacy = fixture(depth, true);
        let mut combined = legacy.clone();
        let legacy_command =
            json!({"op":"move","layer":legacy.doc.layers[0].id,"dx":5.6,"dy":-2.4});
        let combined_command = command(&combined, json!({"dx":5.6,"dy":-2.4}));
        edit(&mut legacy, legacy_command);
        edit(&mut combined, combined_command);
        assert_eq!(combined.doc.layers[0].pixels, legacy.doc.layers[0].pixels);
        assert_eq!(
            combined.doc.export_png().unwrap(),
            legacy.doc.export_png().unwrap()
        );
        assert_eq!(combined.doc.selection, legacy.doc.selection);
        let actual = combined.doc.selection_coverage.as_ref().unwrap();
        let expected = legacy.doc.selection_coverage.as_ref().unwrap();
        assert_eq!(actual.bounds, expected.bounds);
        assert_eq!(actual.origin, expected.origin);
        assert_eq!(actual.mask, expected.mask);
        let canonical = |contours: &[Vec<[f32; 2]>]| {
            let mut paths = contours
                .iter()
                .map(|points| {
                    let mut path = points
                        .iter()
                        .map(|point| point.map(f32::to_bits))
                        .collect::<Vec<_>>();
                    let start = path
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, point)| **point)
                        .map_or(0, |(index, _)| index);
                    path.rotate_left(start);
                    path
                })
                .collect::<Vec<_>>();
            paths.sort();
            paths
        };
        // Closed contour start/order is derived; preserve every vertex and winding.
        assert_eq!(canonical(&actual.contours), canonical(&expected.contours));
    }
}

#[test]
fn whole_affine_translation_retains_native_hidden_rgb_offcanvas_masks_sources_and_inverse_after_storage(
) {
    for depth in [8, 16] {
        let mut engine = fixture(depth, false);
        let before = engine.doc.clone();
        let transform = command(
            &engine,
            json!({"angle":90.,"dx":-40.,"dy":-20.,"pivot":[0.,0.],"selection_only":false}),
        );
        edit(&mut engine, transform);
        assert!(engine.doc.layers[0].x < 0 && engine.doc.layers[0].y < 0);
        for y in 0..14 {
            for x in 0..16 {
                assert_eq!(
                    color(&engine, -y - 41, x - 20),
                    before.layers[0].pixels.get16(x, y)
                );
                assert_eq!(
                    mask(&engine, -y - 41, x - 20),
                    before.layers[0].mask.as_ref().unwrap().steps[1]
                        .pixels
                        .get16(x, y)
                );
            }
        }
        for bytes in [
            psd::encode(&engine.doc).unwrap(),
            psd::encode_psb(&engine.doc).unwrap(),
        ] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(
                reopened.layers[0].pixels.retained,
                engine.doc.layers[0].pixels.retained
            );
            let mut inverse = engine.clone();
            inverse.doc = reopened;
            let inverse_command = command(
                &inverse,
                json!({"angle":-90.,"dx":20.,"dy":-40.,"pivot":[0.,0.],"selection_only":false}),
            );
            edit(&mut inverse, inverse_command);
            assert_eq!(
                inverse.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
            assert_eq!(
                inverse.doc.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(
                inverse.doc.layers[0].mask.as_ref().unwrap().steps[1]
                    .pixels
                    .rgba16(),
                before.layers[0].mask.as_ref().unwrap().steps[1]
                    .pixels
                    .rgba16()
            );
        }
        let source = Source {
            width: 24,
            height: 16,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Shape {
                shape: "rectangle".into(),
                bounds: [2., 3., 12., 13.],
                points: vec![],
                closed: true,
                fill: [12347, 23459, 34571, 65535],
                stroke: [0; 4],
                stroke_width: 0.,
            },
        };
        let mut native = Engine::new();
        native.doc = Document::new_depth(64, 48, depth).unwrap();
        edit(&mut native, json!({"op":"source.add","source":source}));
        let before = native.doc.export_png().unwrap();
        let transform = command(
            &native,
            json!({"angle":90.,"dx":-40.,"dy":-20.,"pivot":[0.,0.],"selection_only":false}),
        );
        edit(&mut native, transform);
        native.doc = psd::decode(&psd::encode_psb(&native.doc).unwrap()).unwrap();
        let inverse = command(
            &native,
            json!({"angle":-90.,"dx":20.,"dy":-40.,"pivot":[0.,0.],"selection_only":false}),
        );
        edit(&mut native, inverse);
        assert_eq!(
            native.doc.layers[0].source.as_ref().unwrap().content,
            source.content
        );
        assert_eq!(native.doc.export_png().unwrap(), before);
    }
}

#[test]
fn combined_destination_reservations_selection_guards_and_invalid_raw_translation_are_atomic() {
    for depth in [8, 16] {
        let mut engine = fixture(depth, true);
        let id = engine.doc.layers[0].id.clone();
        let command = command(
            &engine,
            json!({"angle":90.,"dx":50.,"dy":10.,"pivot":[0.,0.]}),
        );
        let scopes = engine.scopes(&command);
        assert!(scopes
            .iter()
            .any(|scope| scope.target.as_deref() == Some("@selection")));
        engine
            .reserve(
                "other",
                "Translated destination",
                vec![Scope {
                    target: Some(id.clone()),
                    rect: Some([40, 12, 48, 20]),
                }],
            )
            .unwrap();
        let before = state(&engine);
        assert!(engine
            .edit(
                "human",
                &[command.clone()],
                None,
                None,
                "Blocked destination"
            )
            .is_err());
        assert_eq!(state(&engine), before);
        engine.leases.clear();
        engine
            .reserve(
                "other",
                "Selection geometry",
                vec![Scope::layer("@selection")],
            )
            .unwrap();
        assert!(engine
            .edit("human", &[command.clone()], None, None, "Blocked selection")
            .is_err());
        assert_eq!(state(&engine), before);
        engine.leases.clear();
        for invalid in [
            json!("2"),
            json!(true),
            json!(null),
            json!([]),
            json!(100000.01),
            json!(-100000.01),
        ] {
            let mut command = command.clone();
            command["dx"] = invalid;
            let before = state(&engine);
            assert!(Engine::preview_edits(engine.doc.clone(), &[command.clone()]).is_err());
            assert!(engine
                .edit("human", &[command.clone()], None, None, "Invalid")
                .is_err());
            let mut layer = engine.doc.layers[0].clone();
            let pixels = layer.pixels.clone();
            let coverage = selection::current(&engine.doc).unwrap();
            assert!(transform::selection_with_coverage(&mut layer, &coverage, &command).is_err());
            assert_eq!(layer.pixels, pixels);
            assert_eq!(state(&engine), before);
        }
    }
    let legacy =
        transform::resolve_command(&json!({"op":"transform","angle":17.,"snap":true})).unwrap();
    assert!(legacy.get("dx").is_none() && legacy.get("dy").is_none());
    let snapped = transform::resolve_command(
        &json!({"op":"transform","angle":17.,"dx":11.9,"dy":-12.1,"snap":true}),
    )
    .unwrap();
    assert_eq!(
        (snapped["dx"].as_f64(), snapped["dy"].as_f64()),
        (Some(8.), Some(-16.))
    );
}

#[test]
fn explicit_zero_translation_preserves_legacy_retained_words_frames_and_metadata() {
    for depth in [8, 16] {
        for mask_only in [false, true] {
            let mut legacy = fixture(depth, false);
            if mask_only {
                let id = legacy.doc.layers[0].id.clone();
                edit(
                    &mut legacy,
                    json!({"op":"mask.link","layer":id,"linked":false}),
                );
            }
            let mut explicit = legacy.clone();
            let command = command(
                &legacy,
                json!({"angle":17.,"scale_x":1.2,"scale_y":0.9,"mask":mask_only,"selection_only":false}),
            );
            let mut zero = command.clone();
            zero["dx"] = json!(0.);
            zero["dy"] = json!(-0.);
            edit(&mut legacy, command);
            edit(&mut explicit, zero);
            let (a, b) = (&legacy.doc.layers[0], &explicit.doc.layers[0]);
            assert_eq!(
                (a.x, a.y, a.pixels.width, a.pixels.height),
                (b.x, b.y, b.pixels.width, b.pixels.height)
            );
            assert_eq!(a.pixels, b.pixels);
            assert_eq!(a.source, b.source);
            for (a, b) in a
                .mask
                .as_ref()
                .unwrap()
                .steps
                .iter()
                .zip(&b.mask.as_ref().unwrap().steps)
            {
                assert_eq!(a.pixels, b.pixels);
            }
            assert_eq!(
                legacy.doc.export_png().unwrap(),
                explicit.doc.export_png().unwrap()
            );
        }
    }
}

#[test]
fn fractional_opt_in_false_is_exactly_legacy_for_whole_selected_and_independent_masks() {
    for depth in [8, 16] {
        for selected in [false, true] {
            for mask_only in [false, true] {
                let mut legacy = fixture(depth, selected);
                if mask_only {
                    let id = legacy.doc.layers[0].id.clone();
                    edit(
                        &mut legacy,
                        json!({"op":"mask.link","layer":id,"linked":false}),
                    );
                }
                let mut explicit = legacy.clone();
                let original = command(
                    &legacy,
                    json!({"angle":17.,"scale_x":1.2,"scale_y":0.9,"skew_x":7.,"dx":0.25,"dy":-1.5,"mask":mask_only}),
                );
                let mut disabled = original.clone();
                disabled["fractional_translation"] = json!(false);
                edit(&mut legacy, original);
                edit(&mut explicit, disabled);
                let (a, b) = (&legacy.doc.layers[0], &explicit.doc.layers[0]);
                assert_eq!(
                    (a.x, a.y, a.pixels.width, a.pixels.height),
                    (b.x, b.y, b.pixels.width, b.pixels.height)
                );
                assert_eq!(a.pixels, b.pixels);
                assert_eq!(a.source, b.source);
                for (a, b) in a
                    .mask
                    .as_ref()
                    .unwrap()
                    .steps
                    .iter()
                    .zip(&b.mask.as_ref().unwrap().steps)
                {
                    assert_eq!(a.pixels, b.pixels);
                }
                assert_eq!(
                    legacy.doc.export_png().unwrap(),
                    explicit.doc.export_png().unwrap()
                );
                assert_eq!(legacy.doc.selection, explicit.doc.selection);
                if let (Some(a), Some(b)) = (
                    &legacy.doc.selection_coverage,
                    &explicit.doc.selection_coverage,
                ) {
                    assert_eq!((&a.mask, a.bounds, a.origin), (&b.mask, b.bounds, b.origin));
                }
            }
        }
    }
}

#[test]
fn opt_in_quarter_pixel_placement_retains_native_masks_scalar_coverage_history_and_standard_saves()
{
    for depth in [8, 16] {
        for selected in [false, true] {
            for mask_only in [false, true] {
                let mut engine = fixture(depth, selected);
                if mask_only {
                    let id = engine.doc.layers[0].id.clone();
                    edit(
                        &mut engine,
                        json!({"op":"mask.link","layer":id,"linked":false}),
                    );
                    engine.undo.clear();
                }
                let before = engine.doc.clone();
                let c = command(
                    &engine,
                    json!({"fractional_translation":true,"dx":0.25,"dy":0.5,"mask":mask_only}),
                );
                let expected = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
                edit(&mut engine, c);
                assert_eq!(engine.undo.len(), 1);
                assert_eq!(engine.doc.bit_depth, depth);
                assert_eq!(engine.doc.layers[0].pixels, expected.layers[0].pixels);
                assert_eq!(
                    engine.doc.export_png().unwrap(),
                    expected.export_png().unwrap()
                );
                if !selected {
                    let sample = |r: &Raster| {
                        if depth == 16 {
                            r.sample16(3.75, 4.5)
                        } else {
                            r.sample(3.75, 4.5).map(|v| u16::from(v) * 257)
                        }
                    };
                    if mask_only {
                        assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
                    } else {
                        assert_eq!(color(&engine, 4, 5), sample(&before.layers[0].pixels));
                    }
                    assert_eq!(
                        mask(&engine, 4, 5),
                        sample(&before.layers[0].mask.as_ref().unwrap().steps[1].pixels)
                    );
                } else {
                    let old = before.selection_coverage.as_ref().unwrap();
                    let actual = engine.doc.selection_coverage.as_ref().unwrap();
                    let at = |x, y| old.mask.get(x - old.origin[0], y - old.origin[1])[0] as f64;
                    for y in 3..10 {
                        for x in 3..10 {
                            let value = (at(x - 1, y - 1) * 0.125
                                + at(x, y - 1) * 0.375
                                + at(x - 1, y) * 0.125
                                + at(x, y) * 0.375)
                                .round() as u8;
                            assert_eq!(actual.value(x, y), f32::from(value) / 255.);
                        }
                    }
                    assert_eq!(
                        color(&engine, 15, 13),
                        before.layers[0].pixels.get16(15, 13)
                    );
                    assert_eq!(
                        mask(&engine, 15, 13),
                        before.layers[0].mask.as_ref().unwrap().steps[1]
                            .pixels
                            .get16(15, 13)
                    );
                }
                for bytes in [
                    psd::encode(&engine.doc).unwrap(),
                    psd::encode_psb(&engine.doc).unwrap(),
                ] {
                    let reopened = psd::decode(&bytes).unwrap();
                    assert!(!reopened.read_only);
                    assert_eq!(reopened.bit_depth, depth);
                    assert_eq!(reopened.layers[0].pixels, engine.doc.layers[0].pixels);
                    for (a, b) in reopened.layers[0]
                        .mask
                        .as_ref()
                        .unwrap()
                        .steps
                        .iter()
                        .zip(&engine.doc.layers[0].mask.as_ref().unwrap().steps)
                    {
                        assert_eq!(a.pixels, b.pixels);
                    }
                    assert_eq!(
                        reopened.export_png().unwrap(),
                        engine.doc.export_png().unwrap()
                    );
                }
                let applied = engine.doc.clone();
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
                assert_eq!(engine.doc.selection_coverage, before.selection_coverage);
                for (a, b) in engine.doc.layers[0]
                    .mask
                    .as_ref()
                    .unwrap()
                    .steps
                    .iter()
                    .zip(&before.layers[0].mask.as_ref().unwrap().steps)
                {
                    assert_eq!(a.pixels, b.pixels);
                }
                engine.redo("human").unwrap();
                assert_eq!(engine.doc.layers[0].pixels, applied.layers[0].pixels);
                assert_eq!(engine.doc.selection_coverage, applied.selection_coverage);
            }
        }
    }
}

#[test]
fn fractional_scope_encloses_actual_f32_vertices_and_new_destination_reservations_are_atomic() {
    let mut analytic = fixture(16, false);
    analytic.doc.selection = Some([7000, 4000, 7001, 4020]);
    let c = command(
        &analytic,
        json!({"fractional_translation":true,"angle":31.2,"scale_x":1.8,"scale_y":0.7,"dx":0.125,"dy":-0.375,"pivot":[100000.,-100000.]}),
    );
    let scope = analytic
        .scopes(&c)
        .into_iter()
        .find(|s| s.target.as_deref() == Some(analytic.doc.layers[0].id.as_str()))
        .unwrap()
        .rect
        .unwrap();
    let (sin, cos) = 31.2f32.to_radians().sin_cos();
    for [x, y] in selection::rectangle(analytic.doc.selection.unwrap()) {
        let px = 100000f32 + (x - 100000.) * 1.8 * cos - (y + 100000.) * 0.7 * sin + 0.125;
        let py = -100000f32 + (x - 100000.) * 1.8 * sin + (y + 100000.) * 0.7 * cos - 0.375;
        assert!(
            scope[0] <= px.floor() as i32
                && scope[1] <= py.floor() as i32
                && scope[2] >= px.ceil() as i32
                && scope[3] >= py.ceil() as i32
        );
    }
    for depth in [8, 16] {
        let mut engine = fixture(depth, true);
        let id = engine.doc.layers[0].id.clone();
        let b = engine.doc.selection.unwrap();
        let c = command(&engine, json!({"fractional_translation":true,"dx":0.25}));
        engine
            .reserve(
                "other",
                "Fractional destination",
                vec![Scope {
                    target: Some(id),
                    rect: Some([b[2], b[1], b[2] + 1, b[3]]),
                }],
            )
            .unwrap();
        let before = state(&engine);
        assert!(engine
            .edit("human", &[c.clone()], None, None, "Reserved")
            .is_err());
        assert_eq!(state(&engine), before);
        engine.leases.clear();
        for invalid in [json!(null), json!(1), json!("true"), json!([])] {
            let mut invalid_command = c.clone();
            invalid_command["fractional_translation"] = invalid;
            assert!(Engine::preview_edits(engine.doc.clone(), &[invalid_command.clone()]).is_err());
            assert!(engine
                .edit("human", &[invalid_command], None, None, "Invalid flag")
                .is_err());
            assert_eq!(state(&engine), before);
        }
        let moved = json!({"op":"move","layer":engine.doc.layers[0].id,"dx":0.25,"fractional_translation":true});
        assert!(Engine::preview_edits(engine.doc.clone(), &[moved.clone()]).is_err());
        assert!(engine
            .edit("human", &[moved], None, None, "Fractional move")
            .is_err());
        assert_eq!(state(&engine), before);
    }
}

#[test]
fn fractional_editable_source_uses_existing_matrix_format_and_protocol_native_preview() {
    for depth in [8, 16] {
        let mut engine = Engine::new();
        engine.doc = Document::new_depth(64, 48, depth).unwrap();
        let source = Source {
            width: 24,
            height: 16,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Shape {
                shape: "rectangle".into(),
                bounds: [2., 3., 12., 13.],
                points: vec![],
                closed: true,
                fill: [12347, 23459, 34571, 65535],
                stroke: [0; 4],
                stroke_width: 0.,
            },
        };
        edit(&mut engine, json!({"op":"source.add","source":source}));
        engine.undo.clear();
        let before = engine.doc.clone();
        let c = command(
            &engine,
            json!({"fractional_translation":true,"dx":0.25,"dy":0.5,"pivot":[0.,0.],"selection_only":false}),
        );
        let expected = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
        let shared = std::sync::Arc::new(std::sync::Mutex::new(engine));
        let response = peerbrush::server::mcp(
            &shared,
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"peerbrush_edit","arguments":{"actor":"human","commands":[c],"expected_revision":before.revision,"feedback":"always"}}}),
        );
        assert_eq!(response["result"]["isError"], false);
        assert!(response["result"]["content"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "image" && v["mimeType"] == "image/png"));
        let engine = shared.lock().unwrap();
        assert_eq!(engine.undo.len(), 1);
        assert_eq!(
            engine.doc.layers[0].source.as_ref().unwrap().matrix,
            [1., 0., 0., 1., 0.25, 0.5]
        );
        assert_eq!(
            engine.doc.export_png().unwrap(),
            expected.export_png().unwrap()
        );
        for bytes in [
            psd::encode(&engine.doc).unwrap(),
            psd::encode_psb(&engine.doc).unwrap(),
        ] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.layers[0].source, engine.doc.layers[0].source);
            assert_eq!(
                reopened.export_png().unwrap(),
                engine.doc.export_png().unwrap()
            );
        }
    }
}
