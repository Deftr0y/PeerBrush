use super::*;
use super::{
    grouping_tests::gate,
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine, Layer,
};
use serde_json::{json, Value};

fn document(depth: u16) -> Document {
    let mut doc = Document::new_depth(32, 24, depth).unwrap();
    doc.layers[0].id = "target".into();
    for y in 0..24 {
        for x in 0..32 {
            doc.layers[0].pixels.set16(
                x,
                y,
                [
                    10001 + x as u16 * 317,
                    20003 + y as u16 * 271,
                    40007,
                    if (8..12).contains(&x) { 0 } else { 65535 },
                ],
            );
        }
    }
    doc
}
fn compare(actual: &NativeDocument, expected: &Document, original: &Document) {
    let mut expected = expected.clone();
    super::duplicate_tests::align_ids(&mut expected, actual, original);
    super::import_tests::pixels(actual, &expected, original);
}
fn exercise(original: Document, commands: &[Value], succeeds: bool) {
    let expected = Engine::preview_edits(original.clone(), commands);
    assert_eq!(expected.is_ok(), succeeds, "{expected:?}");
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged = CapturedScene::from_parts(original.clone(), "bake".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let baseline = runtime.disk().used().unwrap();
    let preview = prepare(capture(&state, commands), &session, &|| Ok(()));
    if let Ok(mut expected) = expected {
        expected.revision = original.revision + 1;
        let preview = preview.unwrap();
        compare(preview.preview(), &expected, &original);
        for (old, kept) in [
            (
                native.metadata().selection_coverage(),
                preview.preview().metadata().selection_coverage(),
            ),
            (
                native.metadata().selection_previous(),
                preview.preview().metadata().selection_previous(),
            ),
        ] {
            if let Some(old) = old {
                assert_eq!(
                    native.source(old.mask().slot()).unwrap().key(),
                    preview
                        .preview()
                        .source(kept.unwrap().mask().slot())
                        .unwrap()
                        .key()
                );
            }
        }
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let prepared = prepare(capture(&state, commands), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Bake",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        state
            .commit_native(gate(&[]), commit)
            .unwrap()
            .retire(&session, &|| session.cleanup_check())
            .unwrap();
        for (direction, mut expected) in [
            (Direction::Undo, original.clone()),
            (Direction::Redo, expected),
        ] {
            let moved = state
                .native_history()
                .unwrap()
                .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                .unwrap();
            drop(state.move_native(gate(&[]), moved).unwrap());
            expected.revision = state.view().revision();
            compare(state.native().unwrap(), &expected, &original);
        }
    } else {
        let refused = preview.err().expect("Inexact bake was accepted");
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
    drop(state);
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    drop((native, session, staged.seals, cache, runtime, connection));
}

#[test]
fn native_color_bakes_preserve_suffix_masks_selection_and_editable_source_undo() {
    for depth in [8, 16] {
        let mut doc=Engine::preview_edits(document(depth),&[
            json!({"op":"mask.add","layer":"target","value":193}),
            json!({"op":"effect.add","layer":"target","kind":"invert","weight":0.3}),
            json!({"op":"effect.add","layer":"target","kind":"blur","settings":{"radius":2.}}),
            json!({"op":"effect.add","layer":"target","kind":"posterize","settings":{"levels":7}}),
            json!({"op":"selection","rect":[3,4,16,20],"feather":1.}),
            json!({"op":"filter.add","kind":"invert"}),
        ]).unwrap();
        doc.layers[0].effects[0].enabled = false;
        doc.layers[0].opacity = 181. / 255.;
        doc.selection_previous = doc.selection_coverage.clone();
        for at in [0, 1, 2] {
            exercise(
                doc.clone(),
                &[
                    json!({"op":"effect.bake","layer":"target","effect":doc.layers[0].effects[at].id}),
                ],
                true,
            );
        }
        for enabled in [false, true] {
            let mut doc = Engine::preview_edits(
                super::canvas_tests::document(depth),
                &[json!({"op":"effect.add","layer":"b_child","kind":"invert"})],
            )
            .unwrap();
            let l = doc.layers.iter_mut().find(|l| l.id == "b_child").unwrap();
            l.effects[0].enabled = enabled;
            let effect = l.effects[0].id.clone();
            exercise(
                doc,
                &[json!({"op":"effect.bake","layer":"b_child","effect":effect})],
                true,
            );
        }
    }
}

#[test]
fn native_bake_styles_and_folders_include_hidden_sources_and_backdrop_independence() {
    for depth in [8, 16] {
        for kind in ["drop_shadow", "gradient_overlay", "bevel_emboss"] {
            let mut doc = document(depth);
            doc.layers[0].x = 3;
            doc.layers[0].y = 4;
            doc.layers[0].pixels = crate::raster::Raster::new_depth(16, 16, depth);
            for y in 0..16 {
                for x in 0..16 {
                    doc.layers[0]
                        .pixels
                        .set16(x, y, [12345, 23457, 34569, 65535]);
                }
            }
            let mut doc = Engine::preview_edits(
                doc,
                &[
                    json!({"op":"effect.add","layer":"target","kind":kind}),
                    json!({"op":"effect.add","layer":"target","kind":"invert","weight":0.2}),
                ],
            )
            .unwrap();
            doc.layers[0].visible = false;
            let command =
                json!({"op":"effect.bake","layer":"target","effect":doc.layers[0].effects[0].id});
            exercise(doc, &[command], true);
        }
        let mut doc = document(depth);
        let mut child = doc.layers[0].clone();
        child.id = "child".into();
        child.parent = Some("target".into());
        let mut hidden = Layer::new("Hidden source", "paint", 32, 24);
        hidden.parent = Some("target".into());
        hidden.visible = false;
        hidden.pixels = crate::raster::Raster::new_depth(32, 24, depth);
        doc.layers[0].kind = "group".into();
        doc.layers[0].pixels = crate::raster::Raster::new_depth(32, 24, depth);
        doc.layers.extend([child, hidden]);
        let doc = Engine::preview_edits(
            doc,
            &[
                json!({"op":"mask.add","layer":"target","value":211}),
                json!({"op":"effect.add","layer":"target","kind":"invert","weight":0.4}),
            ],
        )
        .unwrap();
        let command =
            json!({"op":"effect.bake","layer":"target","effect":doc.layers[0].effects[0].id});
        exercise(doc.clone(), &[command.clone()], true);
        let mut locked = doc;
        locked.layers[2].locked = true;
        exercise(locked, &[command], false);
    }
}

#[test]
fn native_mask_bake_matches_full_stack_quantization_and_refuses_changed_samples() {
    for depth in [8, 16] {
        let doc=Engine::preview_edits(document(depth),&[
            json!({"op":"mask.add","layer":"target","value":255}),
            json!({"op":"paint","layer":"target","mask":true,"points":[[16,12]],"radius":6.,"hardness":1.,"color":[0,0,0,255]}),
            json!({"op":"mask.step.add","layer":"target","kind":"invert"}),
            json!({"op":"mask.step.add","layer":"target","kind":"blur","value":2.}),
        ]).unwrap();
        let steps = &doc.layers[0].mask.as_ref().unwrap().steps;
        for at in [steps.len() - 2, steps.len() - 1] {
            let command = json!({"op":"mask.step.bake","layer":"target","step":steps[at].id});
            exercise(doc.clone(), &[command.clone()], true);
            let mut disabled = doc.clone();
            disabled.layers[0].mask.as_mut().unwrap().enabled = false;
            exercise(disabled, &[command], true);
        }
    }
    let mut doc = document(16);
    for y in 0..24 {
        for x in 0..32 {
            doc.layers[0].pixels.set16(
                x,
                y,
                [12345, 23457, 34569, 1000 + x as u16 * 1500 + y as u16 * 600],
            );
        }
    }
    let doc=Engine::preview_edits(doc,&[
        json!({"op":"mask.add","layer":"target","value":193}),
        json!({"op":"mask.step.add","layer":"target","kind":"levels","value":2.3,"weight":0.37}),
        json!({"op":"mask.step.add","layer":"target","kind":"blur","value":2.}),
        json!({"op":"mask.step.add","layer":"target","kind":"invert","weight":0.3}),
    ]).unwrap();
    let steps = &doc.layers[0].mask.as_ref().unwrap().steps;
    exercise(
        doc.clone(),
        &[json!({"op":"mask.step.bake","layer":"target","step":steps[3].id})],
        false,
    );
    exercise(
        doc.clone(),
        &[json!({"op":"mask.step.bake","layer":"target","step":steps.last().unwrap().id})],
        true,
    );
}

#[test]
fn baked_outputs_retire_after_late_cancellation_or_reservation_and_failed_batches() {
    let original = Engine::preview_edits(
        document(16),
        &[json!({"op":"effect.add","layer":"target","kind":"invert"})],
    )
    .unwrap();
    let command =
        json!({"op":"effect.bake","layer":"target","effect":original.layers[0].effects[0].id});
    exercise(original.clone(), &[command.clone(), command.clone()], false);
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(original, "bake-cancel".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let baseline = runtime.disk().used().unwrap();
    let peak = std::cell::Cell::new(baseline);
    let preview = prepare(capture(&state, &[command.clone()]), &session, &|| {
        peak.set(peak.get().max(runtime.disk().used().unwrap()));
        Ok(())
    })
    .unwrap();
    preview
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert!(peak.get() > baseline);
    for threshold in [baseline + 1, peak.get()] {
        let refused = prepare(capture(&state, &[command.clone()]), &session, &|| {
            if runtime.disk().used().unwrap() >= threshold {
                Err("Bake interrupted".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(refused.reason.contains("Bake interrupted"));
        assert!(refused.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Bake",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    let leases = [crate::engine::Lease {
        id: "late".into(),
        owner: "other".into(),
        description: "Layer edit".into(),
        scopes: vec![crate::engine::Scope::layer("target")],
        expires: 100,
    }];
    let refused = state.commit_native(gate(&leases), commit).unwrap_err();
    assert!(refused.reason().contains("Reserved"));
    refused
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    assert_eq!(state.history_lengths(), [0, 0]);
    drop((
        state,
        native,
        session,
        staged.seals,
        cache,
        runtime,
        connection,
    ));
}
