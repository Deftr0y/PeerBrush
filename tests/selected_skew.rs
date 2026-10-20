use peerbrush::{
    engine::{Document, Engine, Layer, Scope},
    psd,
    raster::{self, Raster},
    selection::{self, Coverage},
    server,
    source::{Content, Source},
    transform,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn independent_coverage_eq(actual: Option<&Coverage>, expected: Option<&Coverage>) -> bool {
    let canonical = |contours: &[Vec<[f32; 2]>]| {
        let mut paths = contours
            .iter()
            .map(|points| {
                let mut path = points
                    .iter()
                    .map(|point| point.map(f32::to_bits))
                    .collect::<Vec<_>>();
                let start = (0..path.len())
                    .min_by(|&a, &b| {
                        path[a..]
                            .iter()
                            .chain(&path[..a])
                            .cmp(path[b..].iter().chain(&path[..b]))
                    })
                    .unwrap_or(0);
                path.rotate_left(start);
                path
            })
            .collect::<Vec<_>>();
        paths.sort();
        paths
    };
    match (actual, expected) {
        (Some(a), Some(b)) => {
            a.mask == b.mask
                && a.bounds == b.bounds
                && a.origin == b.origin
                && canonical(&a.contours) == canonical(&b.contours)
        }
        (None, None) => true,
        _ => false,
    }
}

#[test]
fn independent_coverage_comparison_changes_only_closed_loop_start_and_order() {
    let base = Coverage {
        mask: Raster::new(4, 4),
        bounds: [0, 0, 4, 4],
        origin: [0, 0],
        contours: vec![
            vec![[0., 0.], [4., 0.], [4., 4.], [0., 4.]],
            vec![[1., 1.], [1., 2.], [2., 2.], [2., 1.]],
            vec![[0., 0.], [1., 0.], [0., 0.], [0., 1.]],
        ],
    };
    let mut rotated = base.clone();
    for (index, path) in rotated.contours.iter_mut().enumerate() {
        path.rotate_left(index + 1);
    }
    rotated.contours.reverse();
    assert!(independent_coverage_eq(Some(&base), Some(&rotated)));
    assert!(independent_coverage_eq(None, None));
    assert!(!independent_coverage_eq(Some(&base), None));
    for change in 0..7 {
        let mut different = base.clone();
        match change {
            0 => different.mask.set(0, 0, [1; 4]),
            1 => different.bounds[2] -= 1,
            2 => different.origin[0] += 1,
            3 => different.contours[0][0][0] = -0.,
            4 => different.contours[0].reverse(),
            5 => {
                different.contours.pop();
            }
            6 => different.contours[0].swap(1, 2),
            _ => unreachable!(),
        }
        assert!(
            !independent_coverage_eq(Some(&base), Some(&different)),
            "Preserve difference {change}"
        );
    }
}

fn edit(engine: &mut Engine, commands: &[Value]) {
    engine
        .edit(
            "human",
            commands,
            Some(engine.doc.revision),
            None,
            "Selected skew",
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
fn fixture(depth: u16) -> Engine {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(64, 48, depth).unwrap();
    engine.doc.layers[0].pixels = Raster::new_depth(16, 14, depth);
    let id = engine.doc.layers[0].id.clone();
    edit(&mut engine, &[json!({"op":"mask.add","layer":id})]);
    let layer = &mut engine.doc.layers[0];
    for y in 0..14 {
        for x in 0..16 {
            let alpha = if (x, y) == (5, 5) || (x, y) == (15, 13) {
                0
            } else {
                65535
            };
            layer.pixels.set16(
                x,
                y,
                [12347 + x as u16 * 97, 23459 + y as u16 * 193, 34571, alpha],
            );
            let gray = 12001 + x as u16 * 151 + y as u16 * 137;
            layer.mask.as_mut().unwrap().steps[1]
                .pixels
                .set16(x, y, [gray, gray, gray, 65535]);
        }
    }
    let mut mask = Raster::new(16, 14);
    for y in 2..9 {
        for x in 2..10 {
            if (x, y) != (5, 5) {
                mask.set(
                    x,
                    y,
                    [if x == 2 || y == 2 {
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
    let coverage = selection::from_mask(mask).unwrap();
    engine.doc.selection = Some(coverage.bounds);
    engine.doc.selection_coverage = Some(coverage);
    engine.undo.clear();
    engine
}
fn command(engine: &Engine, fields: Value) -> Value {
    let mut command = json!({"op":"transform","layer":engine.doc.layers[0].id,
        "pivot":[0.,0.5],"selection_only":true});
    command
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    command
}
fn color(layer: &Layer, x: i32, y: i32) -> [u16; 4] {
    layer.pixels.get16(x - layer.x, y - layer.y)
}
fn paint(layer: &Layer, x: i32, y: i32) -> [u16; 4] {
    layer.mask.as_ref().unwrap().steps[1]
        .pixels
        .get16(x - layer.x, y - layer.y)
}
fn stored(depth: u16, p: [u16; 4]) -> [u16; 4] {
    if depth == 16 {
        p
    } else {
        p.map(|v| u16::from(raster::project16(v)) * 257)
    }
}
fn portion(depth: u16, mut p: [u16; 4], coverage: f32, selected: bool) -> [u16; 4] {
    if coverage == 0. {
        return if selected { [0; 4] } else { p };
    }
    p[3] = (f64::from(p[3]) * f64::from(if selected { coverage } else { 1. - coverage })).round()
        as u16;
    if p[3] == 0 {
        return [0; 4];
    }
    stored(depth, p)
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
fn saved_composite(bytes: &[u8], depth: u16) -> Vec<u16> {
    let bytes = standard_only(bytes);
    let (size, header) = if bytes[4..6] == 2u16.to_be_bytes() {
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
    assert_eq!(&bytes[header + size..header + size + 2], &[0, 0]);
    let planes = &bytes[header + size + 2..];
    let words = if depth == 16 {
        planes
            .chunks_exact(2)
            .map(|v| u16::from_be_bytes(v.try_into().unwrap()))
            .collect::<Vec<_>>()
    } else {
        planes
            .iter()
            .map(|v| u16::from(*v) * 257)
            .collect::<Vec<_>>()
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
            let divisor = if doc.bit_depth == 16 { 1u64 } else { 257 };
            let alpha = u64::from(p[3]) / divisor;
            (0..4).map(move |c| {
                if c == 3 {
                    p[3]
                } else {
                    (((u64::from(p[c]) / divisor * alpha + max * (max - alpha) + max / 2) / max)
                        * divisor) as u16
                }
            })
        })
        .collect()
}
fn storage(doc: &Document) {
    for bytes in [psd::encode(doc).unwrap(), psd::encode_psb(doc).unwrap()] {
        let reopened = psd::decode(&bytes).unwrap();
        assert!(!reopened.read_only);
        assert_eq!(reopened.bit_depth, doc.bit_depth);
        assert_eq!(reopened.layers[0].pixels, doc.layers[0].pixels);
        assert_eq!(reopened.selection_coverage, doc.selection_coverage);
        let (a, b) = (
            doc.layers[0].mask.as_ref().unwrap(),
            reopened.layers[0].mask.as_ref().unwrap(),
        );
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
        assert_eq!(reopened.export_png().unwrap(), doc.export_png().unwrap());
        let standard = psd::decode(&standard_only(&bytes)).unwrap();
        assert!(!standard.read_only);
        assert_eq!(standard.bit_depth, doc.bit_depth);
        assert_eq!(
            standard.layers[0].pixels.rgba16(),
            doc.layers[0].pixels.rgba16()
        );
        for y in 0..48 {
            for x in 0..64 {
                let a = &doc.layers[0];
                let b = &standard.layers[0];
                let value = |layer: &Layer| {
                    if doc.bit_depth == 16 {
                        (peerbrush::depth16::mask_value_prepared(
                            layer,
                            x - layer.x,
                            y - layer.y,
                            None,
                            true,
                        ) * 65535.)
                            .round() as u16
                    } else {
                        (layer.mask_value_raw(x - layer.x, y - layer.y) * 255.).round() as u16 * 257
                    }
                };
                assert_eq!(value(a), value(b));
            }
        }
        // Standard masks store the current combined gray value at native depth;
        // compare that channel above, and the actual merged bytes independently.
        assert_eq!(saved_composite(&bytes, doc.bit_depth), matte(doc));
    }
}

#[test]
fn selected_horizontal_skew_has_analytic_native_color_mask_soft_hole_remainder_and_storage() {
    for depth in [8, 16] {
        for target in ["linked", "color", "mask", "step"] {
            let mut engine = fixture(depth);
            if target != "linked" {
                let id = engine.doc.layers[0].id.clone();
                edit(
                    &mut engine,
                    &[json!({"op":"mask.link","layer":id,"linked":false})],
                );
                engine.undo.clear();
            }
            let before = engine.doc.clone();
            let before_state = state(&engine);
            let coverage = selection::current(&before).unwrap();
            let mut c = command(
                &engine,
                json!({"skew_x":45.,"dx":12.,"dy":3.,"mask":target=="mask"||target=="step"}),
            );
            if target == "step" {
                c["step"] = json!(before.layers[0].mask.as_ref().unwrap().steps[1].id);
            }
            let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
            assert_eq!(state(&engine), before_state);
            edit(&mut engine, &[c]);
            assert_eq!(engine.undo.len(), 1);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                preview.export_png().unwrap()
            );
            let layer = &engine.doc.layers[0];
            let moved_color = target == "linked" || target == "color";
            let moved_mask = target != "color";
            // About (0,.5), Hx(45deg) maps pixel centers to (x+y,y).
            for y in 2..9 {
                for x in 2..10 {
                    let factor = coverage.value(x, y);
                    let expected_color = if moved_color {
                        portion(depth, color(&before.layers[0], x, y), factor, true)
                    } else {
                        [0; 4]
                    };
                    let expected_mask = if moved_mask {
                        portion(depth, paint(&before.layers[0], x, y), factor, true)
                    } else {
                        [0; 4]
                    };
                    assert_eq!(
                        color(layer, x + y + 12, y + 3),
                        expected_color,
                        "{depth} {target} color {x},{y}"
                    );
                    assert_eq!(
                        paint(layer, x + y + 12, y + 3),
                        expected_mask,
                        "{depth} {target} mask {x},{y}"
                    );
                    assert_eq!(
                        selection::current(&engine.doc)
                            .unwrap()
                            .value(x + y + 12, y + 3),
                        factor
                    );
                }
            }
            for y in 0..14 {
                for x in 0..16 {
                    let factor = coverage.value(x, y);
                    let p = color(&before.layers[0], x, y);
                    let m = paint(&before.layers[0], x, y);
                    assert_eq!(
                        color(layer, x, y),
                        if moved_color {
                            portion(depth, p, factor, false)
                        } else {
                            p
                        }
                    );
                    assert_eq!(
                        paint(layer, x, y),
                        if moved_mask {
                            portion(depth, m, factor, false)
                        } else {
                            m
                        }
                    );
                }
            }
            // A hole and unselected transparent color keep their hidden native RGB.
            assert_eq!(color(layer, 5, 5), color(&before.layers[0], 5, 5));
            assert_eq!(color(layer, 15, 13), color(&before.layers[0], 15, 13));
            storage(&engine.doc);
            let after = engine.doc.clone();
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
            assert_eq!(engine.doc.selection_coverage, before.selection_coverage);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
            engine.redo("human").unwrap();
            assert_eq!(engine.doc.layers[0].pixels, after.layers[0].pixels);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                after.export_png().unwrap()
            );
        }
    }
}

fn scalar(coverage: &Coverage, x: f32, y: f32) -> u8 {
    let (ix, iy) = (x.floor() as i32, y.floor() as i32);
    let (fx, fy) = (x - ix as f32, y - iy as f32);
    let at = |x, y| {
        f32::from(
            coverage
                .mask
                .get(x - coverage.origin[0], y - coverage.origin[1])[0],
        )
    };
    (at(ix, iy) * (1. - fx) * (1. - fy)
        + at(ix + 1, iy) * fx * (1. - fy)
        + at(ix, iy + 1) * (1. - fx) * fy
        + at(ix + 1, iy + 1) * fx * fy)
        .round() as u8
}

#[test]
fn combined_shears_reflection_scale_rotation_and_translation_use_one_analytic_inverse() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let before = engine.doc.clone();
        let coverage = selection::current(&before).unwrap();
        let mut cut = Raster::new_depth(16, 14, depth);
        for y in 0..14 {
            for x in 0..16 {
                cut.set16(
                    x,
                    y,
                    portion(
                        depth,
                        color(&before.layers[0], x, y),
                        coverage.value(x, y),
                        true,
                    ),
                );
            }
        }
        let c = command(
            &engine,
            json!({"skew_x":45.,"skew_y":-45.,"mirror_x":true,
            "scale_x":2.,"scale_y":0.5,"angle":90.,"dx":40.,"dy":30.}),
        );
        let expected_polygon = [[41., 23.5], [45., 7.5], [45., -6.5], [41., 9.5]];
        let bounds = selection::bounds(&expected_polygon).unwrap();
        let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
        edit(&mut engine, &[c]);
        let layer = &engine.doc.layers[0];
        assert_eq!(
            engine.doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        for y in bounds[1]..bounds[3] {
            for x in bounds[0]..bounds[2] {
                // Independent matrix M=[.5,-2,0,-2] about (0,.5).
                // M^-1=[2,-2,0,-.5], then convert centers to source sample indices.
                let rx = x as f32 + 0.5 - 40.;
                let ry = y as f32 + 0.5 - 30.5;
                let (u, v) = (2. * rx - 0.5, -2. * rx - 0.5 * ry);
                let mut expected = if depth == 16 {
                    cut.sample16(u, v)
                } else {
                    cut.sample(u, v).map(|v| u16::from(v) * 257)
                };
                if !selection::contains(&expected_polygon, x as f32 + 0.5, y as f32 + 0.5)
                    || expected[3] == 0
                {
                    expected = [0; 4];
                }
                assert_eq!(color(layer, x, y), expected, "{depth} native {x},{y}");
                assert_eq!(
                    (selection::current(&engine.doc).unwrap().value(x, y) * 255.).round() as u8,
                    scalar(&coverage, u, v),
                    "{depth} scalar {x},{y}"
                );
            }
        }
        assert_eq!(color(layer, 15, 13), color(&before.layers[0], 15, 13));
        assert_eq!(engine.undo.len(), 1);
    }
}

#[test]
fn polygon_skew_has_analytic_vertices_and_omitted_or_zero_skew_preserves_legacy_selected_words() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        engine.doc.selection_coverage = None;
        let polygon = vec![[2., 2.], [10., 2.], [8., 9.], [3., 7.]];
        engine.doc.selection = Some(selection::bounds(&polygon).unwrap());
        engine.doc.selection_polygon = Some(polygon);
        let before = engine.doc.clone();
        let c = command(&engine, json!({"skew_x":45.,"skew_y":45.,"dx":12.,"dy":3.}));
        let preview = Engine::preview_edits(before.clone(), &[c.clone()]).unwrap();
        assert_eq!(
            preview.selection_polygon,
            Some(vec![[15.5, 8.5], [23.5, 16.5], [28.5, 28.5], [21.5, 19.5]])
        );
        assert_eq!(engine.doc.selection, before.selection);
        edit(&mut engine, &[c]);
        assert_eq!(engine.doc.layers[0].pixels, preview.layers[0].pixels);
        assert_eq!(engine.doc.selection_polygon, preview.selection_polygon);
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
        for covered in [false, true] {
            let mut engine = fixture(depth);
            if !covered {
                engine.doc.selection_coverage = None;
                engine.doc.selection_polygon =
                    Some(selection::rectangle(engine.doc.selection.unwrap()));
            }
            let c = command(
                &engine,
                json!({"angle":17.,"scale_x":1.3,"scale_y":0.7,"mirror_y":true,"dx":7.,"dy":5.}),
            );
            let mut zero = c.clone();
            zero["skew_x"] = json!(0.);
            zero["skew_y"] = json!(-0.);
            assert_eq!(engine.scopes(&c), engine.scopes(&zero));
            let a = Engine::preview_edits(engine.doc.clone(), &[c]).unwrap();
            let b = Engine::preview_edits(engine.doc.clone(), &[zero]).unwrap();
            assert_eq!(a.layers[0].pixels, b.layers[0].pixels);
            assert_eq!(a.selection, b.selection);
            assert_eq!(a.selection_polygon, b.selection_polygon);
            assert!(independent_coverage_eq(
                a.selection_coverage.as_ref(),
                b.selection_coverage.as_ref()
            ));
            for (a, b) in a.layers[0]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .iter()
                .zip(&b.layers[0].mask.as_ref().unwrap().steps)
            {
                assert_eq!(a.pixels, b.pixels);
            }
            assert_eq!(a.export_png().unwrap(), b.export_png().unwrap());
        }
    }
}

#[test]
fn multiple_roots_share_the_original_soft_selection_and_one_history_entry() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let mut second = engine.doc.layers[0].clone();
        second.id = peerbrush::engine::id();
        second.name = "Hidden second root".into();
        second.visible = false;
        engine.doc.layers.push(second);
        let before = engine.doc.clone();
        let commands = before
            .layers
            .iter()
            .map(|layer| {
                let mut c = command(
                    &engine,
                    json!({"skew_x":45.,"skew_y":-15.,"dx":12.,"dy":3.}),
                );
                c["layer"] = json!(layer.id);
                c
            })
            .collect::<Vec<_>>();
        let expected = commands
            .iter()
            .map(|c| Engine::preview_edits(before.clone(), &[c.clone()]).unwrap())
            .collect::<Vec<_>>();
        let preview = Engine::preview_edits(before.clone(), &commands).unwrap();
        edit(&mut engine, &commands);
        assert_eq!(engine.undo.len(), 1);
        for (index, expected) in expected.iter().enumerate() {
            assert_eq!(
                engine.doc.layers[index].pixels,
                expected.layers[index].pixels
            );
            assert_eq!(engine.doc.layers[index].parent, before.layers[index].parent);
            assert_eq!(
                engine.doc.layers[index].visible,
                before.layers[index].visible
            );
            for (a, b) in engine.doc.layers[index]
                .mask
                .as_ref()
                .unwrap()
                .steps
                .iter()
                .zip(&expected.layers[index].mask.as_ref().unwrap().steps)
            {
                assert_eq!(a.pixels, b.pixels);
            }
            assert!(independent_coverage_eq(
                engine.doc.selection_coverage.as_ref(),
                expected.selection_coverage.as_ref()
            ));
        }
        assert_eq!(
            engine.doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        engine.undo("human").unwrap();
        assert_eq!(engine.doc.selection_coverage, before.selection_coverage);
        for (a, b) in engine.doc.layers.iter().zip(&before.layers) {
            assert_eq!(a.pixels, b.pixels);
        }
    }
}

#[test]
fn selected_skew_destination_selection_source_and_partial_batch_guards_are_atomic_in_protocol() {
    use base64::Engine as _;
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let before = engine.doc.clone();
        let id = before.layers[0].id.clone();
        let mut c = command(&engine, json!({"skew_x":45.,"dx":12.,"dy":3.}));
        c["document_id"] = json!(before.id);
        c["source_revision"] = json!(before.revision);
        // This destination lies beyond the old rotation/scale-only scope.
        engine
            .reserve(
                "other",
                "Sheared destination",
                vec![Scope {
                    target: Some(id.clone()),
                    rect: Some([28, 10, 29, 11]),
                }],
            )
            .unwrap();
        let shared = Arc::new(Mutex::new(engine));
        let request = json!({"actor":"selected-skew-agent","document_id":before.id,"expected_revision":before.revision,
            "commands":[c],"feedback":"always","max_edge":64});
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
        let result = server::dispatch(&shared, "edit", &request).unwrap();
        assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 64, 48]));
        let png = base64::engine::general_purpose::STANDARD
            .decode(result["images"][0]["data"].as_str().unwrap())
            .unwrap();
        let after = shared.lock().unwrap().doc.clone();
        let (_, _, expected, _) = after.preview(None, 64, None, false).unwrap();
        assert_eq!(
            image::load_from_memory(&png).unwrap().to_rgba8().into_raw(),
            expected
        );
        assert_eq!(shared.lock().unwrap().undo.len(), 1);
        assert!(
            server::dispatch(&shared, "edit", &request).is_err(),
            "A consumed source revision must not silently rebase"
        );
        server::dispatch(
            &shared,
            "history",
            &json!({"actor":"selected-skew-agent","action":"undo",
            "document_id":after.id,"expected_revision":after.revision}),
        )
        .unwrap();
        assert_eq!(
            shared.lock().unwrap().doc.export_png().unwrap(),
            before.export_png().unwrap()
        );

        let mut engine = fixture(depth);
        let before = state(&engine);
        let first = json!({"op":"layer.update","layer":engine.doc.layers[0].id,"name":"Would change first"});
        for fields in [
            json!({"skew_x":"45"}),
            json!({"skew_y":true}),
            json!({"skew_x":null}),
            json!({"skew_x":80.01}),
            json!({"skew_y":-80.01}),
            json!({"skew_x":80.,"skew_y":80.,"scale_x":20.,"scale_y":20.,"pivot":[100000.,100000.]}),
        ] {
            let c = command(&engine, fields);
            assert!(engine
                .edit(
                    "human",
                    &[first.clone(), c.clone()],
                    None,
                    None,
                    "Atomic invalid skew"
                )
                .is_err());
            assert_eq!(state(&engine), before);
            assert!(Engine::preview_edits(engine.doc.clone(), &[c]).is_err());
        }
        engine.doc.layers[0].locked = true;
        let before = state(&engine);
        let c = command(&engine, json!({"skew_x":45.}));
        assert!(engine.edit("human", &[c], None, None, "Locked").is_err());
        assert_eq!(state(&engine), before);
        engine.doc.layers[0].locked = false;
        engine.doc.layers[0].mask.as_mut().unwrap().steps[1].kind = "invert".into();
        let before = state(&engine);
        let c = command(&engine, json!({"skew_x":45.}));
        assert!(engine
            .edit("human", &[first.clone(), c], None, None, "Unsupported mask")
            .is_err());
        assert_eq!(state(&engine), before);

        let mut engine = Engine::new();
        engine.doc = Document::new_depth(64, 48, depth).unwrap();
        let source = Source {
            width: 24,
            height: 16,
            matrix: [1., 0., 0., 1., 0., 0.],
            content: Content::Shape {
                shape: "rectangle".into(),
                bounds: [2., 2., 18., 13.],
                points: vec![],
                closed: true,
                fill: [12347, 23459, 34571, 65535],
                stroke: [0; 4],
                stroke_width: 0.,
            },
        };
        edit(&mut engine, &[json!({"op":"source.add","source":source})]);
        edit(&mut engine, &[json!({"op":"selection","rect":[2,2,10,9]})]);
        let before = state(&engine);
        let c = command(&engine, json!({"skew_x":45.}));
        let error = engine
            .edit("human", &[c], None, None, "Editable-source protection")
            .unwrap_err();
        assert!(error.contains("rasterize"));
        assert_eq!(state(&engine), before);
    }
    assert_eq!(
        server::capabilities()["retained_transforms"]["skew"]["selection_pixels"],
        true
    );
}

#[test]
fn thin_large_pivot_skew_scopes_enclose_actual_f32_polygon_without_large_allocations() {
    for (area, pivot, fields) in [
        (
            [94992, -84996, 94994, -84992],
            [95000., -85000.],
            json!({"skew_x":80.,"skew_y":-80.,"angle":89.999,"scale_x":0.05,"scale_y":0.05}),
        ),
        (
            [-89999, 79992, -89997, 79996],
            [-90000., 80000.],
            json!({"skew_x":-75.,"skew_y":65.,"angle":17.,"scale_x":0.2,"scale_y":0.3,"mirror_x":true}),
        ),
        // This nonzero f32 shear can disappear in rounded f64 composed coefficients.
        (
            [-89999, 79992, -89997, 79996],
            [-90000., 80000.],
            json!({"skew_x":1e-16,"skew_y":0.,"angle":45.,"scale_x":1.,"scale_y":1.}),
        ),
    ] {
        let mut engine = Engine::new();
        engine.doc = Document::new(8, 8).unwrap();
        engine.doc.selection = Some(area);
        let mut c = command(&engine, fields);
        c["pivot"] = json!(pivot);
        let (sin, cos) = c["angle"].as_f64().unwrap().to_radians().sin_cos();
        let sx = c["scale_x"].as_f64().unwrap() * if c["mirror_x"] == true { -1. } else { 1. };
        let sy = c["scale_y"].as_f64().unwrap();
        let kx = c["skew_x"].as_f64().unwrap().to_radians().tan();
        let ky = c["skew_y"].as_f64().unwrap().to_radians().tan();
        let polygon = selection::rectangle(area);
        let analytic = polygon
            .iter()
            .map(|p| {
                let u = f64::from(p[0]) - pivot[0] + kx * (f64::from(p[1]) - pivot[1]);
                let v = f64::from(p[1]) - pivot[1] + ky * u;
                [
                    (pivot[0] + u * sx * cos - v * sy * sin) as f32,
                    (pivot[1] + u * sx * sin + v * sy * cos) as f32,
                ]
            })
            .collect::<Vec<_>>();
        let analytic_bounds = selection::bounds(&analytic).unwrap();
        let mut layer = Layer::new("Tiny bounds fixture", "paint", 1, 1);
        layer.x = analytic_bounds[0];
        layer.y = analytic_bounds[1];
        let (actual, points) = transform::selection_with_polygon(&mut layer, &polygon, &c).unwrap();
        assert!(layer.pixels.width <= 512 && layer.pixels.height <= 512);
        let scope = engine
            .scopes(&c)
            .into_iter()
            .find(|s| s.target.as_deref() == Some(engine.doc.layers[0].id.as_str()))
            .unwrap()
            .rect
            .unwrap();
        assert!(
            scope[0] <= actual[0]
                && scope[1] <= actual[1]
                && scope[2] >= actual[2]
                && scope[3] >= actual[3],
            "Scope {scope:?} must enclose actual renderer bounds {actual:?}"
        );
        for (actual, expected) in points.iter().zip(&analytic) {
            assert!(
                (actual[0] - expected[0]).abs() <= 0.05 && (actual[1] - expected[1]).abs() <= 0.05
            );
        }
    }
}
