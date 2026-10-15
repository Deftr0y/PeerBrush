use peerbrush::{
    compatibility::{self, LossCode},
    engine::{Document, Engine, Layer, Scope},
    psd,
    raster::Raster,
    server,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

fn edit(engine: &mut Engine, command: Value) {
    engine
        .edit(
            "human",
            &[command],
            Some(engine.doc.revision),
            None,
            "Mask transform",
        )
        .unwrap();
}
fn fixture(depth: u16) -> Engine {
    let mut engine = Engine::new();
    engine.doc = Document::new_depth(12, 10, depth).unwrap();
    let id = engine.doc.layers[0].id.clone();
    edit(&mut engine, json!({"op":"mask.add","layer":id}));
    let layer = &mut engine.doc.layers[0];
    for y in 0..10 {
        for x in 0..12 {
            layer.pixels.set16(
                x,
                y,
                [10001 + x as u16 * 237, 20003 + y as u16 * 151, 40007, 65535],
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
fn command(engine: &Engine, op: &str, mask: bool) -> Value {
    json!({"op":op,"layer":engine.doc.layers[0].id,"mask":mask,"selection_only":false})
}
fn move_by(engine: &mut Engine, mask: bool, dx: i32, dy: i32) {
    let mut cmd = command(engine, "move", mask);
    cmd["dx"] = json!(dx);
    cmd["dy"] = json!(dy);
    edit(engine, cmd);
}
fn rotate(engine: &mut Engine, mask: bool, angle: f64) {
    let mut cmd = command(engine, "transform", mask);
    cmd["angle"] = json!(angle);
    cmd["pivot"] = json!([6, 5]);
    edit(engine, cmd);
}
fn unlink(engine: &mut Engine) {
    edit(
        engine,
        json!({"op":"mask.link","layer":engine.doc.layers[0].id,"linked":false}),
    );
}

#[test]
fn linking_is_metadata_with_shared_preview_atomic_guards_and_undo() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let id = engine.doc.layers[0].id.clone();
        let before = engine.doc.export_png().unwrap();
        assert!(engine.doc.layers[0].mask.as_ref().unwrap().linked);
        let mut legacy = serde_json::to_value(&engine.doc).unwrap();
        legacy["layers"][0]["mask"]
            .as_object_mut()
            .unwrap()
            .remove("linked");
        let legacy: Document = serde_json::from_value(legacy).unwrap();
        assert!(legacy.layers[0].mask.as_ref().unwrap().linked);
        let cmd = json!({"op":"mask.link","layer":id,"linked":false});
        let preview = Engine::preview_edits(engine.doc.clone(), &[cmd.clone()]).unwrap();
        assert!(!preview.layers[0].mask.as_ref().unwrap().linked);
        assert!(engine.doc.layers[0].mask.as_ref().unwrap().linked);
        assert_eq!(preview.export_png().unwrap(), before);
        for invalid in [json!(null), json!("false"), json!(0)] {
            let snapshot = serde_json::to_value(&engine.doc).unwrap();
            assert!(engine
                .edit(
                    "human",
                    &[json!({"op":"mask.link","layer":id,"linked":invalid})],
                    None,
                    None,
                    "Invalid"
                )
                .is_err());
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), snapshot);
            assert!(engine.undo.is_empty());
        }
        engine
            .reserve("other", "Mask work", vec![Scope::layer(&id)])
            .unwrap();
        assert!(engine
            .edit("human", &[cmd.clone()], None, None, "Reserved")
            .is_err());
        engine.leases.clear();
        edit(&mut engine, cmd);
        assert_eq!(engine.undo.len(), 1);
        engine.undo("human").unwrap();
        assert!(engine.doc.layers[0].mask.as_ref().unwrap().linked);
        engine.redo("human").unwrap();
        assert!(!engine.doc.layers[0].mask.as_ref().unwrap().linked);
        assert_eq!(engine.doc.export_png().unwrap(), before);
    }
}

#[test]
fn linked_whole_moves_and_rotations_follow_either_selected_channel() {
    for depth in [8, 16] {
        for mask in [false, true] {
            let mut engine = fixture(depth);
            let original = engine.doc.layers[0].clone();
            move_by(&mut engine, mask, 3, 2);
            let layer = &engine.doc.layers[0];
            assert_eq!((layer.x, layer.y), (3, 2));
            assert_eq!(layer.pixels.rgba16(), original.pixels.rgba16());
            assert_eq!(
                layer.mask.as_ref().unwrap().steps[1].pixels.rgba16(),
                original.mask.as_ref().unwrap().steps[1].pixels.rgba16()
            );
            engine.undo("human").unwrap();
            rotate(&mut engine, mask, 90.);
            let layer = &engine.doc.layers[0];
            for y in 0..10 {
                for x in 0..12 {
                    let (nx, ny) = (10 - y, x - 1);
                    assert_eq!(
                        layer.pixels.get16(nx - layer.x, ny - layer.y),
                        original.pixels.get16(x, y)
                    );
                    assert_eq!(
                        layer.mask.as_ref().unwrap().steps[1]
                            .pixels
                            .get16(nx - layer.x, ny - layer.y),
                        original.mask.as_ref().unwrap().steps[1].pixels.get16(x, y)
                    );
                }
            }
        }
    }
}

#[test]
fn unlinked_color_preserves_document_mask_positions_and_off_frame_originals() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        unlink(&mut engine);
        let before = engine.doc.layers[0].clone();
        move_by(&mut engine, false, 3, 2);
        let layer = &engine.doc.layers[0];
        for y in 2..10 {
            for x in 3..12 {
                assert_eq!(
                    layer.mask.as_ref().unwrap().steps[1]
                        .pixels
                        .get16(x - layer.x, y - layer.y),
                    before.mask.as_ref().unwrap().steps[1].pixels.get16(x, y)
                );
            }
        }
        move_by(&mut engine, false, -3, -2);
        assert_eq!(
            engine.doc.layers[0].mask.as_ref().unwrap().steps[1]
                .pixels
                .rgba16(),
            before.mask.as_ref().unwrap().steps[1].pixels.rgba16()
        );
        rotate(&mut engine, false, 90.);
        let layer = &engine.doc.layers[0];
        for y in 0..10 {
            for x in 1..11 {
                assert_eq!(
                    layer.mask.as_ref().unwrap().steps[1]
                        .pixels
                        .get16(x - layer.x, y - layer.y),
                    before.mask.as_ref().unwrap().steps[1].pixels.get16(x, y)
                );
            }
        }
        rotate(&mut engine, false, -90.);
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), before.pixels.rgba16());
        assert_eq!(
            engine.doc.layers[0].mask.as_ref().unwrap().steps[1]
                .pixels
                .rgba16(),
            before.mask.as_ref().unwrap().steps[1].pixels.rgba16()
        );
    }
}

#[test]
fn unlinked_mask_moves_every_paint_step_without_moving_color() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        unlink(&mut engine);
        let mask = engine.doc.layers[0].mask.as_mut().unwrap();
        let mut second = mask.steps[1].clone();
        second.id = peerbrush::engine::id();
        second.pixels = Raster::new_depth(12, 10, depth);
        second.pixels.set16(2, 3, [45683, 45683, 45683, 65535]);
        mask.steps.push(second);
        let before = engine.doc.layers[0].clone();
        move_by(&mut engine, true, 2, 1);
        let layer = &engine.doc.layers[0];
        assert_eq!((layer.x, layer.y), (before.x, before.y));
        assert_eq!(layer.pixels.rgba16(), before.pixels.rgba16());
        for index in [1, 2] {
            for y in 0..9 {
                for x in 0..10 {
                    assert_eq!(
                        layer.mask.as_ref().unwrap().steps[index]
                            .pixels
                            .get16(x + 2, y + 1),
                        before.mask.as_ref().unwrap().steps[index]
                            .pixels
                            .get16(x, y)
                    );
                }
            }
        }
        move_by(&mut engine, true, -2, -1);
        rotate(&mut engine, true, 90.);
        rotate(&mut engine, true, -90.);
        for index in [1, 2] {
            assert_eq!(
                engine.doc.layers[0].mask.as_ref().unwrap().steps[index]
                    .pixels
                    .rgba16(),
                before.mask.as_ref().unwrap().steps[index].pixels.rgba16()
            );
        }
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), before.pixels.rgba16());
    }
}

#[test]
fn linked_selection_moves_only_selected_color_and_mask_pixels() {
    for depth in [8, 16] {
        for mask in [false, true] {
            let mut engine = fixture(depth);
            engine.doc.selection = Some([2, 2, 5, 5]);
            let before = engine.doc.layers[0].clone();
            let mut cmd = command(&engine, "move", mask);
            cmd["selection_only"] = json!(true);
            cmd["dx"] = json!(5);
            cmd["dy"] = json!(1);
            let preview = Engine::preview_edits(engine.doc.clone(), &[cmd.clone()]).unwrap();
            assert!(engine.undo.is_empty());
            edit(&mut engine, cmd);
            assert_eq!(
                engine.doc.export_png().unwrap(),
                preview.export_png().unwrap()
            );
            assert_eq!(engine.doc.selection, Some([7, 3, 10, 6]));
            let layer = &engine.doc.layers[0];
            for y in 0..10 {
                for x in 0..12 {
                    for (source, result) in [
                        (&before.pixels, &layer.pixels),
                        (
                            &before.mask.as_ref().unwrap().steps[1].pixels,
                            &layer.mask.as_ref().unwrap().steps[1].pixels,
                        ),
                    ] {
                        let expected = if (2..5).contains(&x) && (2..5).contains(&y) {
                            [0; 4]
                        } else if (7..10).contains(&x) && (3..6).contains(&y) {
                            source.get16(x - 5, y - 1)
                        } else {
                            source.get16(x, y)
                        };
                        assert_eq!(
                            result.get16(x, y),
                            expected,
                            "depth {depth}, mask {mask}, {x},{y}"
                        );
                    }
                }
            }
            engine.undo("human").unwrap();
            assert_eq!(engine.doc.layers[0].pixels.rgba16(), before.pixels.rgba16());
            assert_eq!(
                engine.doc.layers[0].mask.as_ref().unwrap().steps[1]
                    .pixels
                    .rgba16(),
                before.mask.as_ref().unwrap().steps[1].pixels.rgba16()
            );
        }
    }
}

#[test]
fn unsupported_independent_mask_effects_fail_without_rasterizing_or_history() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let id = engine.doc.layers[0].id.clone();
        edit(
            &mut engine,
            json!({"op":"mask.step.add","layer":id,"kind":"invert"}),
        );
        unlink(&mut engine);
        let before = serde_json::to_value(&engine.doc).unwrap();
        let history = engine.undo.len();
        let mut cmd = command(&engine, "transform", false);
        cmd["angle"] = json!(30);
        assert!(engine
            .edit("human", &[cmd], None, None, "Unsupported")
            .unwrap_err()
            .contains("fill/paint"));
        assert_eq!(serde_json::to_value(&engine.doc).unwrap(), before);
        assert_eq!(engine.undo.len(), history);
    }
}

#[test]
fn source_save_reopen_preserves_link_choice_native_words_and_current_composite() {
    for depth in [8, 16] {
        for linked in [false, true] {
            let mut engine = fixture(depth);
            if !linked {
                unlink(&mut engine);
                move_by(&mut engine, false, 2, 1);
            }
            let pixels = engine.doc.export_png().unwrap();
            let report = compatibility::report(&engine.doc).unwrap();
            assert_eq!(
                report
                    .losses
                    .iter()
                    .any(|loss| loss.code == LossCode::MaskLinkPrivate),
                !linked
            );
            for data in [
                psd::encode(&engine.doc).unwrap(),
                psd::encode_psb(&engine.doc).unwrap(),
            ] {
                let reopened = psd::decode(&data).unwrap();
                assert!(!reopened.read_only, "{:?}", reopened.warnings);
                assert_eq!(reopened.layers[0].mask.as_ref().unwrap().linked, linked);
                assert_eq!(
                    reopened.layers[0].pixels.rgba16(),
                    engine.doc.layers[0].pixels.rgba16()
                );
                assert_eq!(reopened.export_png().unwrap(), pixels);
                assert_eq!(
                    reopened.layers[0].mask.as_ref().unwrap().steps[1]
                        .pixels
                        .rgba16(),
                    engine.doc.layers[0].mask.as_ref().unwrap().steps[1]
                        .pixels
                        .rgba16()
                );
                if !linked {
                    let mut inverse = Engine::new();
                    inverse.doc = reopened;
                    move_by(&mut inverse, false, -2, -1);
                    let original = fixture(depth);
                    assert_eq!(
                        inverse.doc.layers[0].pixels.rgba16(),
                        original.doc.layers[0].pixels.rgba16()
                    );
                    assert_eq!(
                        inverse.doc.layers[0].mask.as_ref().unwrap().steps[1]
                            .pixels
                            .rgba16(),
                        original.doc.layers[0].mask.as_ref().unwrap().steps[1]
                            .pixels
                            .rgba16()
                    );
                }
            }
        }
    }
}

#[test]
fn selective_agent_undo_restores_link_state_without_losing_later_human_paint() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        let id = engine.doc.layers[0].id.clone();
        let task = engine.reserve("agent", "Link mask", vec![]).unwrap().id;
        engine
            .edit(
                "agent",
                &[json!({"op":"mask.link","layer":id,"linked":false})],
                None,
                Some(&task),
                "Unlink",
            )
            .unwrap();
        edit(
            &mut engine,
            json!({"op":"paint","layer":id,"points":[[4,4],[8,6]],"radius":2,"color":[233,84,32,255]}),
        );
        let painted = engine.doc.layers[0].pixels.rgba16();
        engine.undo_task("human", "agent", &task).unwrap();
        assert!(engine.doc.layers[0].mask.as_ref().unwrap().linked);
        assert_eq!(engine.doc.layers[0].pixels.rgba16(), painted);
    }
}

#[test]
fn protocol_reports_link_state_and_rejects_stale_or_locked_updates() {
    let engine = fixture(16);
    let id = engine.doc.layers[0].id.clone();
    let document = engine.doc.id.clone();
    let revision = engine.doc.revision;
    let shared = Arc::new(Mutex::new(engine));
    let params = json!({"actor":"human","document_id":document,"expected_revision":revision,"commands":[{"op":"mask.link","layer":id,"linked":false}]});
    server::dispatch(&shared, "edit", &params).unwrap();
    let result = server::dispatch(&shared, "observe", &json!({"image":false})).unwrap();
    assert_eq!(result["state"]["layers"][0]["mask"]["linked"], false);
    assert!(server::dispatch(&shared, "edit", &params).is_err());
    let mut engine = shared.lock().unwrap();
    engine.doc.layers[0].locked = true;
    let snapshot = serde_json::to_value(&engine.doc).unwrap();
    assert!(engine
        .edit(
            "human",
            &[json!({"op":"mask.link","layer":id,"linked":true})],
            None,
            None,
            "Locked"
        )
        .is_err());
    assert_eq!(serde_json::to_value(&engine.doc).unwrap(), snapshot);
}

#[test]
fn folder_targets_respect_links_and_preserve_hidden_child_masks() {
    for depth in [8, 16] {
        for linked in [false, true] {
            for mask_target in [false, true] {
                let mut engine = fixture(depth);
                let before = engine.doc.layers[0].clone();
                let mut folder = Layer::new("Folder", "group", 12, 10);
                folder.pixels = Raster::new_depth(12, 10, depth);
                folder.mask = before.mask.clone();
                folder.mask.as_mut().unwrap().linked = linked;
                let id = folder.id.clone();
                engine.doc.layers[0].parent = Some(id.clone());
                engine.doc.layers[0].visible = false;
                engine.doc.layers[0].mask.as_mut().unwrap().linked = false;
                engine.doc.layers.insert(0, folder);
                edit(
                    &mut engine,
                    json!({"op":"move","layer":id,"mask":mask_target,"dx":3,"dy":2,"selection_only":false}),
                );
                let child = &engine.doc.layers[1];
                let child_moved = !mask_target || linked;
                assert_eq!(
                    (child.x, child.y),
                    if child_moved { (3, 2) } else { (0, 0) }
                );
                assert!(!child.visible);
                for y in 2..10 {
                    for x in 3..12 {
                        assert_eq!(
                            child.mask.as_ref().unwrap().steps[1]
                                .pixels
                                .get16(x - child.x, y - child.y),
                            before.mask.as_ref().unwrap().steps[1].pixels.get16(x, y)
                        );
                    }
                }
                engine.undo("human").unwrap();
                assert_eq!(engine.doc.layers[1].pixels.rgba16(), before.pixels.rgba16());
                assert_eq!(
                    engine.doc.layers[1].mask.as_ref().unwrap().steps[1]
                        .pixels
                        .rgba16(),
                    before.mask.as_ref().unwrap().steps[1].pixels.rgba16()
                );
            }
        }
    }
}

#[test]
fn large_mask_transforms_reject_excess_work_before_allocating_or_changing_history() {
    for depth in [8, 16] {
        let mut engine = fixture(depth);
        unlink(&mut engine);
        let layer = &mut engine.doc.layers[0];
        layer.pixels = Raster::new_depth(8192, 4096, depth);
        let mask = layer.mask.as_mut().unwrap();
        let mut paint = mask.steps[1].clone();
        paint.pixels = Raster::new_depth(8192, 4096, depth);
        mask.steps.clear();
        for index in 0..5 {
            paint.id = format!("large-mask-{index}");
            mask.steps.push(paint.clone());
        }
        engine.undo.clear();
        let original = serde_json::to_value(&engine.doc).unwrap();
        for op in ["move", "transform"] {
            let mut cmd = command(&engine, op, true);
            cmd["dx"] = json!(1);
            cmd["angle"] = json!(90);
            let error = engine
                .edit("human", &[cmd], None, None, "Too much work")
                .unwrap_err();
            assert!(error.contains("work budget"), "{error}");
            assert_eq!(serde_json::to_value(&engine.doc).unwrap(), original);
            assert!(engine.undo.is_empty());
        }
    }
}
