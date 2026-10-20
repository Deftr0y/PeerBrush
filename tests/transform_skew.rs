use peerbrush::{
    engine::{Document, Engine, History, Layer, Scope},
    layer_clipboard, psd,
    raster::Raster,
    server,
    source::{Content, Source},
    transform,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn edit(engine: &mut Engine, command: Value) {
    engine
        .edit("human", &[command], Some(engine.doc.revision), None, "Skew")
        .unwrap();
}
fn fixture(depth: u16) -> Engine {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(48, 40, depth).unwrap();
    engine.doc.layers[0].pixels = Raster::new_depth(12, 10, depth);
    let id = engine.doc.layers[0].id.clone();
    edit(&mut engine, json!({"op":"mask.add","layer":id}));
    let layer = &mut engine.doc.layers[0];
    for y in 0..10 {
        for x in 0..12 {
            layer.pixels.set16(
                x,
                y,
                [
                    10001 + x as u16 * 237,
                    20003 + y as u16 * 151,
                    40007,
                    if (x + y) % 11 == 0 { 0 } else { 65535 },
                ],
            );
            let gray = 12347 + x as u16 * 97 + y as u16 * 193;
            layer.mask.as_mut().unwrap().steps[1]
                .pixels
                .set16(x, y, [gray, gray, gray, 65535]);
        }
    }
    engine.undo.clear();
    engine
}
fn command(engine: &Engine, fields: Value) -> Value {
    let mut command = json!({
        "op":"transform","layer":engine.doc.layers[0].id,
        "pivot":[0.,0.5],"selection_only":false
    });
    command
        .as_object_mut()
        .unwrap()
        .extend(fields.as_object().unwrap().clone());
    command
}
fn state(engine: &Engine) -> Value {
    let history = |entries: &[History]| {
        entries.iter().map(|entry| json!({
        "before":entry.before,"after":entry.after,"actor":entry.actor,"label":entry.label,
        "task":entry.task,"scopes":entry.scopes,"gesture":entry.gesture,"reverted":entry.reverted
    })).collect::<Vec<_>>()
    };
    json!({"document":engine.doc,"undo":history(&engine.undo),"redo":history(&engine.redo)})
}
fn mask_at(layer: &Layer, x: i32, y: i32) -> [u16; 4] {
    layer.mask.as_ref().unwrap().steps[1]
        .pixels
        .get16(x - layer.x, y - layer.y)
}
fn standard_only(bytes: &[u8]) -> Vec<u8> {
    let color = u32::from_be_bytes(bytes[26..30].try_into().unwrap()) as usize;
    let at = 30 + color;
    let resources = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
    let mut standard = bytes[..at].to_vec();
    standard.extend_from_slice(&0u32.to_be_bytes());
    standard.extend_from_slice(&bytes[at + 4 + resources..]);
    standard
}

#[test]
fn native_skew_has_analytic_pixels_masks_hidden_rgb_preview_undo_and_storage_inverse() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let before = engine.doc.clone();
        let unchanged = state(&engine);
        let transform = command(&engine, json!({"skew_x":45.}));
        let preview = Engine::preview_edits(before.clone(), &[transform.clone()]).unwrap();
        assert_eq!(state(&engine), unchanged);
        edit(&mut engine, transform);
        assert_eq!(engine.undo.len(), 1);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        let layer = &engine.doc.layers[0];
        assert_eq!(layer.pixels.depth, depth);
        assert_eq!(
            layer.pixels.retained.as_ref().unwrap().pixels.as_ref(),
            &before.layers[0].pixels
        );
        for y in 0..10 {
            for x in 0..12 {
                assert_eq!(
                    layer.pixels.get16(x + y - layer.x, y - layer.y),
                    before.layers[0].pixels.get16(x, y)
                );
                assert_eq!(mask_at(layer, x + y, y), mask_at(&before.layers[0], x, y));
            }
        }
        let transformed = engine.doc.clone();
        engine.undo("human").unwrap();
        assert_eq!(
            engine.doc.export_png().unwrap(),
            before.export_png().unwrap()
        );
        assert_eq!(engine.doc.layers[0].pixels, before.layers[0].pixels);
        engine.redo("human").unwrap();
        assert_eq!(engine.doc.layers[0].pixels, transformed.layers[0].pixels);
        for bytes in [
            psd::encode(&transformed).unwrap(),
            psd::encode_psb(&transformed).unwrap(),
        ] {
            let reopened = psd::decode(&bytes).unwrap();
            assert!(!reopened.read_only);
            assert_eq!(reopened.bit_depth, depth);
            assert_eq!(
                reopened.layers[0].pixels.rgba16(),
                transformed.layers[0].pixels.rgba16()
            );
            assert_eq!(
                reopened.layers[0].pixels.retained,
                transformed.layers[0].pixels.retained
            );
            assert_eq!(
                reopened.export_png().unwrap(),
                transformed.export_png().unwrap()
            );
            let standard = psd::decode(&standard_only(&bytes)).unwrap();
            assert_eq!(
                standard.layers[0].pixels.rgba16(),
                transformed.layers[0].pixels.rgba16()
            );
            assert_eq!(
                standard.export_png().unwrap(),
                transformed.export_png().unwrap()
            );
            engine.doc = reopened;
            let inverse = command(&engine, json!({"skew_x":-45.}));
            edit(&mut engine, inverse);
            assert_eq!((engine.doc.layers[0].x, engine.doc.layers[0].y), (0, 0));
            assert_eq!(
                engine.doc.layers[0].pixels.rgba16(),
                before.layers[0].pixels.rgba16()
            );
            assert_eq!(
                engine.doc.layers[0].mask.as_ref().unwrap().steps[1]
                    .pixels
                    .rgba16(),
                before.layers[0].mask.as_ref().unwrap().steps[1]
                    .pixels
                    .rgba16()
            );
            assert_eq!(
                engine.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
        }
        let copy =
            layer_clipboard::copy(&transformed, &[transformed.layers[0].id.clone()]).unwrap();
        let mut destination = Document::new_depth(48, 40, depth).unwrap();
        let ids = layer_clipboard::paste(&mut destination, &copy, "").unwrap();
        let pasted = destination
            .layers
            .iter()
            .find(|layer| layer.id == ids[0])
            .unwrap();
        assert_eq!(pasted.pixels, transformed.layers[0].pixels);
        let old_mask = transformed.layers[0].mask.as_ref().unwrap();
        let pasted_mask = pasted.mask.as_ref().unwrap();
        assert_eq!(pasted_mask.linked, old_mask.linked);
        for (actual, expected) in pasted_mask.steps.iter().zip(&old_mask.steps) {
            assert_eq!(actual.kind, expected.kind);
            assert_eq!(actual.pixels, expected.pixels);
        }
    }
}

#[test]
fn combined_skew_scale_reflection_rotation_matches_order_and_explicit_zero_is_legacy() {
    for depth in [8, 16] {
        let mut combined = fixture(depth);
        let mut ordered = combined.clone();
        let transform = command(
            &combined,
            json!({
                "skew_x":45.,"skew_y":-45.,"scale_x":2.,"scale_y":0.5,"mirror_x":true,"angle":90.
            }),
        );
        let preview = Engine::preview_edits(combined.doc.clone(), &[transform.clone()]).unwrap();
        edit(&mut combined, transform);
        for fields in [
            json!({"skew_x":45.}),
            json!({"skew_y":-45.}),
            json!({"scale_x":2.,"scale_y":0.5,"mirror_x":true,"angle":90.}),
        ] {
            let transform = command(&ordered, fields);
            edit(&mut ordered, transform);
        }
        assert_eq!(combined.undo.len(), 1);
        let actual = &combined.doc.layers[0];
        let expected = &ordered.doc.layers[0];
        assert_eq!(
            (
                actual.x,
                actual.y,
                actual.pixels.width,
                actual.pixels.height
            ),
            (
                expected.x,
                expected.y,
                expected.pixels.width,
                expected.pixels.height
            )
        );
        assert_eq!(
            (
                actual.x,
                actual.y,
                actual.pixels.width,
                actual.pixels.height,
                actual.pixels.depth
            ),
            (0, -43, 6, 45, depth)
        );
        assert_eq!(actual.pixels.rgba16(), expected.pixels.rgba16());
        let mut actual_projection = actual.pixels.clone();
        let mut expected_projection = expected.pixels.clone();
        actual_projection.retained = None;
        expected_projection.retained = None;
        assert!(
            actual_projection == expected_projection,
            "Native projection metadata or samples differ"
        );
        let actual_source = actual.pixels.retained.as_ref().unwrap();
        let expected_source = expected.pixels.retained.as_ref().unwrap();
        assert_eq!(
            actual_source.pixels.rgba16(),
            expected_source.pixels.rgba16()
        );
        assert!(
            actual_source.pixels == expected_source.pixels,
            "Retained original metadata or samples differ"
        );
        // tan(45deg) can compose to an adjacent f64 in one combined delta.
        // Projections and original native samples above must still be exact.
        for (actual, expected) in actual_source.matrix.iter().zip(expected_source.matrix) {
            assert!(
                actual.is_finite() && expected.is_finite() && (actual - expected).abs() <= 1e-12,
                "Combined/ordered affine differs: {} versus {}",
                actual,
                expected
            );
        }
        // R90 * diag(-2, .5) * Hy(-45) * Hx(45) is
        // [.5,-2,0,-2]; pivot [0,.5] gives world translation [0,1.5].
        // The 12x10 original bounds become [0,-43,6,2], hence local ty=44.5.
        let analytic = [0.5, -2., 0., -2., 0., 44.5];
        for matrix in [&actual_source.matrix, &expected_source.matrix] {
            for (actual, expected) in matrix.iter().zip(analytic) {
                assert!(
                    actual.is_finite() && (actual - expected).abs() <= 1e-12,
                    "Affine coefficient differs from analytic placement: {} versus {}",
                    actual,
                    expected
                );
            }
        }
        assert_eq!(
            combined.doc.export_png().unwrap(),
            ordered.doc.export_png().unwrap()
        );
        assert_eq!(
            combined.doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        let mut legacy = fixture(depth);
        let mut zero = legacy.clone();
        let old = command(
            &legacy,
            json!({"angle":37.,"scale_x":1.3,"scale_y":0.8,"mirror_y":true}),
        );
        let mut explicit = old.clone();
        explicit["skew_x"] = json!(0.);
        explicit["skew_y"] = json!(-0.);
        edit(&mut legacy, old);
        edit(&mut zero, explicit);
        assert_eq!(zero.doc.layers[0].pixels, legacy.doc.layers[0].pixels);
        assert_eq!(
            zero.doc.export_png().unwrap(),
            legacy.doc.export_png().unwrap()
        );
    }
}

#[test]
fn skew_preserves_linking_and_independent_mask_or_color_targets() {
    for depth in [8, 16] {
        for mask in [false, true] {
            let mut engine = fixture(depth);
            let id = engine.doc.layers[0].id.clone();
            edit(
                &mut engine,
                json!({"op":"mask.link","layer":id,"linked":false}),
            );
            engine.undo.clear();
            let before = engine.doc.clone();
            let transform = command(&engine, json!({"skew_x":45.,"mask":mask}));
            let preview = Engine::preview_edits(before.clone(), &[transform.clone()]).unwrap();
            edit(&mut engine, transform);
            assert_eq!(engine.undo.len(), 1);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                preview.export_png().unwrap()
            );
            let layer = &engine.doc.layers[0];
            if mask {
                assert_eq!(layer.pixels, before.layers[0].pixels);
                assert_eq!((layer.x, layer.y), (before.layers[0].x, before.layers[0].y));
                assert!(layer.mask.as_ref().unwrap().steps[1]
                    .pixels
                    .retained
                    .is_some());
                let inverse = command(&engine, json!({"skew_x":-45.,"mask":true}));
                edit(&mut engine, inverse);
                assert_eq!(
                    engine.doc.layers[0].mask.as_ref().unwrap().steps[1]
                        .pixels
                        .rgba16(),
                    before.layers[0].mask.as_ref().unwrap().steps[1]
                        .pixels
                        .rgba16()
                );
            } else {
                for y in 0..10 {
                    for x in 0..12 {
                        assert_eq!(mask_at(layer, x, y), mask_at(&before.layers[0], x, y));
                    }
                }
            }
            engine.undo("human").unwrap();
            if mask {
                engine.undo("human").unwrap();
            }
            assert_eq!(
                engine.doc.export_png().unwrap(),
                before.export_png().unwrap()
            );
        }
        let mut color = fixture(depth);
        let mut mask = color.clone();
        let color_command = command(&color, json!({"skew_y":30.}));
        let mask_command = command(&mask, json!({"skew_y":30.,"mask":true}));
        edit(&mut color, color_command);
        edit(&mut mask, mask_command);
        assert_eq!(mask.doc.layers[0].pixels, color.doc.layers[0].pixels);
        assert_eq!(
            mask.doc.export_png().unwrap(),
            color.doc.export_png().unwrap()
        );
    }
}

#[test]
fn invalid_selected_or_guarded_skew_and_partial_batches_preserve_source_and_history() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        for fields in [
            json!({"skew_x":"15"}),
            json!({"skew_x":true}),
            json!({"skew_y":null}),
            json!({"skew_y":[]}),
            json!({"skew_x":80.01}),
            json!({"skew_y":-80.01}),
            json!({"skew_x":80.,"scale_x":20.,"scale_y":20.,"pivot":[100000,100000]}),
        ] {
            let before = state(&engine);
            let transform = command(&engine, fields);
            assert!(Engine::preview_edits(engine.doc.clone(), &[transform.clone()]).is_err());
            assert!(engine
                .edit("human", &[transform], None, None, "Invalid skew")
                .is_err());
            assert_eq!(state(&engine), before);
        }
        for guard in ["locked", "reserved", "stale", "document", "read_only"] {
            let mut engine = fixture(depth);
            let mut transform = command(&engine, json!({"skew_y":15.}));
            match guard {
                "locked" => engine.doc.layers[0].locked = true,
                "reserved" => {
                    let id = engine.doc.layers[0].id.clone();
                    engine
                        .reserve("other", "Reserved target", vec![Scope::layer(&id)])
                        .unwrap();
                }
                "stale" => transform["source_revision"] = json!(engine.doc.revision + 1),
                "document" => transform["document_id"] = json!("another-document"),
                _ => engine.doc.read_only = true,
            }
            let before = state(&engine);
            assert!(
                engine
                    .edit("human", &[transform], None, None, "Guarded skew")
                    .is_err(),
                "{guard}"
            );
            assert_eq!(state(&engine), before, "{guard}");
        }
        edit(
            &mut engine,
            json!({"op":"selection","rect":[2,2,9,8],"feather":1.}),
        );
        let before = state(&engine);
        let mut selected = command(&engine, json!({"skew_x":81.,"selection_only":true}));
        let error = Engine::preview_edits(engine.doc.clone(), &[selected.clone()]).unwrap_err();
        assert!(error.contains("between -80 and 80"));
        let move_first =
            json!({"op":"move","layer":engine.doc.layers[0].id,"dx":1,"selection_only":false});
        assert!(engine
            .edit(
                "human",
                &[move_first, selected.clone()],
                None,
                None,
                "Atomic selected skew"
            )
            .is_err());
        assert_eq!(state(&engine), before);
        let mut layer = engine.doc.layers[0].clone();
        let layer_before = serde_json::to_value(&layer).unwrap();
        assert!(transform::selection_with_polygon(
            &mut layer,
            &peerbrush::selection::rectangle([2, 2, 9, 8]),
            &selected
        )
        .is_err());
        assert_eq!(serde_json::to_value(&layer).unwrap(), layer_before);
        let coverage = peerbrush::selection::current(&engine.doc).unwrap();
        assert!(transform::selection_with_coverage(&mut layer, &coverage, &selected).is_err());
        assert_eq!(serde_json::to_value(&layer).unwrap(), layer_before);
        selected["skew_x"] = json!(15.);
        selected["selection_only"] = json!(false);
        edit(&mut engine, selected);
        let old = serde_json::from_value::<Document>(before["document"].clone()).unwrap();
        assert_eq!(engine.doc.selection, old.selection);
        assert_eq!(engine.doc.selection_polygon, old.selection_polygon);
        assert_eq!(
            serde_json::to_value(&engine.doc.selection_coverage).unwrap(),
            serde_json::to_value(&old.selection_coverage).unwrap()
        );
        assert_eq!(
            serde_json::to_value(&engine.doc.selection_previous).unwrap(),
            serde_json::to_value(&old.selection_previous).unwrap()
        );
    }
    let snapped = transform::resolve_command(
        &json!({"op":"transform","skew_x":22.4,"skew_y":-80.,"snap":true}),
    )
    .unwrap();
    assert_eq!(snapped["skew_x"], 15.);
    assert_eq!(snapped["skew_y"], -75.);
    assert!(
        transform::resolve_command(&json!({"op":"transform","skew_x":80.01,"snap":true})).is_err()
    );
}

#[test]
fn folders_sources_and_protocol_use_shared_skew_without_rasterizing_definitions() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let mut root = Layer::new("Folder", "group", 48, 40);
        root.pixels = Raster::new_depth(48, 40, depth);
        let id = root.id.clone();
        engine.doc.layers[0].parent = Some(id.clone());
        let mut hidden = engine.doc.layers[0].clone();
        hidden.id = peerbrush::engine::id();
        hidden.visible = false;
        engine.doc.layers.push(hidden);
        engine.doc.layers.insert(0, root);
        let before = engine.doc.clone();
        let transform = command(&engine, json!({"skew_x":45.}));
        let preview = Engine::preview_edits(before.clone(), &[transform.clone()]).unwrap();
        edit(&mut engine, transform);
        assert_eq!(engine.undo.len(), 1);
        assert_eq!(
            engine.doc.export_png().unwrap(),
            preview.export_png().unwrap()
        );
        for old in before.layers.iter().filter(|layer| layer.kind == "paint") {
            let layer = engine
                .doc
                .layers
                .iter()
                .find(|layer| layer.id == old.id)
                .unwrap();
            assert_eq!((&layer.parent, layer.visible), (&old.parent, old.visible));
            for y in 0..10 {
                for x in 0..12 {
                    assert_eq!(
                        layer.pixels.get16(x + y - layer.x, y - layer.y),
                        old.pixels.get16(x, y)
                    );
                }
            }
        }
        engine.undo("human").unwrap();
        engine.doc.layers[2].locked = true;
        let before = state(&engine);
        let transform = command(&engine, json!({"skew_x":15.}));
        assert!(engine
            .edit("human", &[transform], None, None, "Locked hidden child")
            .is_err());
        assert_eq!(state(&engine), before);

        for content in [
            Content::Shape {
                shape: "rectangle".into(),
                bounds: [4., 3., 18., 16.],
                points: vec![],
                closed: true,
                fill: [10001, 20003, 30007, 65535],
                stroke: [0; 4],
                stroke_width: 0.,
            },
            Content::Text {
                text: "AV".into(),
                font: "regular".into(),
                size: 12.,
                line_height: 1.2,
                letter_spacing: 0.,
                align: "left".into(),
                color: [10001, 20003, 30007, 65535],
            },
        ] {
            let source = Source {
                width: 32,
                height: 24,
                matrix: [1., 0., 0., 1., 0., 0.],
                content,
            };
            let mut engine = Engine::new();
            engine.doc = Document::new_depth(48, 40, depth).unwrap();
            edit(&mut engine, json!({"op":"source.add","source":source}));
            engine.undo.clear();
            let before = engine.doc.export_png().unwrap();
            let transform = command(&engine, json!({"skew_x":45.}));
            let preview = Engine::preview_edits(engine.doc.clone(), &[transform.clone()]).unwrap();
            edit(&mut engine, transform);
            assert_eq!(engine.undo.len(), 1);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                preview.export_png().unwrap()
            );
            assert_eq!(
                engine.doc.layers[0].source.as_ref().unwrap().content,
                source.content
            );
            engine.doc = psd::decode(&psd::encode_psb(&engine.doc).unwrap()).unwrap();
            let inverse = command(&engine, json!({"skew_x":-45.}));
            edit(&mut engine, inverse);
            assert_eq!(
                engine.doc.layers[0].source.as_ref().unwrap().content,
                source.content
            );
            assert_eq!(engine.doc.export_png().unwrap(), before);
        }
    }
    let engine = fixture(16);
    let transform = command(&engine, json!({"skew_x":15.,"skew_y":-10.}));
    let request = json!({"actor":"skew-agent","document_id":engine.doc.id,
        "expected_revision":engine.doc.revision,"commands":[transform],"feedback":"always","max_edge":48});
    let shared = Arc::new(Mutex::new(engine));
    let result = server::dispatch(&shared, "edit", &request).unwrap();
    assert!(result["images"][0]["data"]
        .as_str()
        .is_some_and(|data| !data.is_empty()));
    assert_eq!(result["images"][0]["document_rect"], json!([0, 0, 48, 40]));
    assert_eq!(shared.lock().unwrap().undo.len(), 1);
    let discovery = server::capabilities();
    assert_eq!(
        discovery["retained_transforms"]["skew"]["selection_pixels"],
        true
    );
    assert_eq!(
        discovery["retained_transforms"]["skew"]["degrees"],
        json!([-80, 80])
    );
}
