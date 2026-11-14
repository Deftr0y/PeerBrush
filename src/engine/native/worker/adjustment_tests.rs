use super::effect_create_tests::compare_raster;
use super::grouping_tests::{compare, document, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Document, Engine,
};
use serde_json::json;

fn align(expected: &mut Document, native: &NativeDocument, original: &Document) {
    let mapping: Vec<_> = expected
        .layers
        .iter()
        .zip(native.metadata().layers())
        .filter(|(layer, _)| !original.layers.iter().any(|l| l.id == layer.id))
        .map(|(layer, actual)| (layer.id.clone(), actual.id().to_owned()))
        .collect();
    for (layer, actual) in expected.layers.iter_mut().zip(native.metadata().layers()) {
        let created = !original.layers.iter().any(|l| l.id == layer.id);
        for (old, new) in &mapping {
            if layer.id == *old {
                layer.id = new.clone();
            }
            if layer.parent.as_ref() == Some(old) {
                layer.parent = Some(new.clone());
            }
            if layer.clip_to.as_ref() == Some(old) {
                layer.clip_to = Some(new.clone());
            }
        }
        if created {
            for (effect, actual) in layer.effects.iter_mut().zip(actual.effects()) {
                effect.id = actual.id().into();
            }
            if let Some(mask) = &mut layer.mask {
                for (step, actual) in mask.steps.iter_mut().zip(actual.mask().unwrap().steps()) {
                    step.id = actual.id().into();
                }
            }
        }
    }
}

#[test]
fn native_adjustments_preserve_artwork_selection_masks_hierarchy_and_atomic_history() {
    for depth in [8, 16] {
        for selection in [
            json!({"op":"selection.clear"}),
            json!({"op":"selection","rect":[1,1,6,7]}),
            json!({"op":"selection","kind":"ellipse","rect":[1,1,7,7],"feather":1}),
            json!({"op":"selection","kind":"polygon","polygon":[[1.,1.],[6.,2.],[2.,7.]]}),
        ] {
            let root = Fixture::new();
            let (runtime, connection) =
                root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let original = Engine::preview_edits(document(depth), &[selection]).unwrap();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "adjustments".into(),
                vec![],
                None,
                None,
            )
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let session = staged.session.take().unwrap();
            let mut commands = vec![
                json!({"op":"adjustment.add","layer":"a_base","kind":"levels","settings":{"channels":[{"output_black":1.,"output_white":0.},{},{}]}}),
                json!({"op":"adjustment.add","layer":"a_base","kind":"levels","settings":{"output_black":0.19,"output_white":0.83}}),
                json!({"op":"adjustment.add","layers":[],"kind":"invert"}),
                json!({"op":"adjustment.add","layer":"a_base","kind":"invert"}),
                json!({"op":"adjustment.add","layer":"a_clip","kind":"invert"}),
                json!({"op":"adjustment.add","layer":"A","kind":"invert"}),
                json!({"op":"adjustment.add","layers":["a_clip","a_base"],"kind":"invert"}),
                json!({"op":"adjustment.add","layers":["A","outside"],"kind":"invert"}),
                json!({"op":"adjustment.add","layers":["outside","outside"],"kind":"invert"}),
                json!({"op":"adjustment.add","layer":"a_base","name":"Color adjustment"}),
            ];
            commands.extend(
                [
                    "color_balance",
                    "hsl",
                    "levels",
                    "curves",
                    "adjust",
                    "grayscale",
                    "posterize",
                    "threshold",
                ]
                .map(|kind| json!({"op":"adjustment.add","layer":"a_base","kind":kind})),
            );
            for command in commands {
                let disk = runtime.disk().used().unwrap();
                let mut expected =
                    Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
                let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                let preview =
                    prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
                align(&mut expected, preview.preview(), &original);
                compare(preview.preview(), &expected);
                assert_eq!(
                    preview.created_layers().count(),
                    if command["layers"].as_array().is_some_and(|a| a.len() > 1) {
                        2
                    } else {
                        1
                    }
                );
                for layer in preview.preview().metadata().layers() {
                    let expected = expected.layers.iter().find(|l| l.id == layer.id()).unwrap();
                    if let Some(old) = native.metadata().layers().find(|l| l.id() == layer.id()) {
                        assert_eq!(
                            native.source(old.pixels().slot()).unwrap().key(),
                            preview
                                .preview()
                                .source(layer.pixels().slot())
                                .unwrap()
                                .key()
                        );
                    } else {
                        assert!(preview
                            .preview()
                            .source(layer.pixels().slot())
                            .unwrap()
                            .is_known_zero());
                        assert_eq!(layer.pixels().depth(), depth);
                        if let Some(mask) = layer.mask() {
                            assert!(mask.enabled() && mask.linked());
                            assert_eq!(mask.steps().len(), 2);
                            for (step, expected) in
                                mask.steps().zip(&expected.mask.as_ref().unwrap().steps)
                            {
                                assert_eq!(step.pixels().depth(), depth);
                                compare_raster(preview.preview(), step.pixels(), &expected.pixels);
                            }
                        } else if layer.kind() == "adjustment" {
                            assert!(original.selection.is_none());
                        }
                    }
                }
                assert_eq!(
                    preview.preview().metadata().selection(),
                    native.metadata().selection()
                );
                if let Some(old) = native.metadata().selection_coverage() {
                    assert_eq!(
                        native.source(old.mask().slot()).unwrap().key(),
                        preview
                            .preview()
                            .source(
                                preview
                                    .preview()
                                    .metadata()
                                    .selection_coverage()
                                    .unwrap()
                                    .mask()
                                    .slot()
                            )
                            .unwrap()
                            .key()
                    );
                }
                let scene = Arc::new(
                    SceneSnapshot::from_native(
                        SceneIdentity {
                            project_id: "adjustment-preview".into(),
                            document_id: expected.id.clone(),
                            revision: expected.revision,
                        },
                        preview.preview().clone(),
                        vec![],
                        None,
                        None,
                    )
                    .unwrap(),
                );
                let read = runtime.begin_read(&scene).unwrap();
                let request = crate::render::native_preview::Request::capture(
                    scene, read, None, 8, None, false,
                )
                .unwrap();
                let frame = request.render(&mut || Ok(())).unwrap();
                assert_eq!(
                    &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                    expected.preview(None, 8, None, false).unwrap().2.as_slice()
                );
                drop((frame, request));
                preview
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), disk);
                assert_eq!(state.history_lengths(), [0, 0]);
                if command["layers"] != json!(["a_clip", "a_base"]) {
                    continue;
                }
                let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                align(&mut expected, prepared.preview(), &original);
                let (base, history, delta) = prepared.into_parts();
                let commit = history
                    .prepare_commit(
                        &base,
                        delta,
                        HistoryRequest {
                            actor: "human",
                            label: "Adjustment",
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
                assert_eq!(state.history_lengths(), [1, 0]);
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
                    compare(state.native().unwrap(), &expected);
                }
            }
            drop((native, session, staged.seals, cache, runtime, connection));
        }
    }
}

#[test]
fn failed_adjustments_and_late_reservations_retire_masks_and_partial_groups() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "adjustment-refusal".into(),
        vec![],
        None,
        None,
    )
    .stage(&runtime, &cache, &|| Ok(()))
    .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let valid = json!({"op":"adjustment.add","layers":["a_clip","a_base"],"kind":"invert"});
    for invalid in [
        json!({"op":"adjustment.add","kind":"blur"}),
        json!({"op":"adjustment.add","kind":"channel_clamp"}),
        json!({"op":"adjustment.add","kind":"levels","settings":{"gamma":0}}),
        json!({"op":"adjustment.add","settings":null}),
        json!({"op":"adjustment.add","layer":"missing"}),
        json!({"op":"adjustment.add","layers":["a_base","outside"]}),
        json!({"op":"adjustment.add","layers":[7]}),
        json!({"op":"adjustment.add","layers":null}),
    ] {
        let commands = [valid.clone(), invalid];
        assert!(Engine::preview_edits(original.clone(), &commands).is_err());
        let failed = prepare(capture(&state, &commands), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    for target in ["a_base", "a_clip", "A", "outer"] {
        assert!(prepare(
            capture(
                &state,
                &[
                    json!({"op":"layer.update","layer":target,"locked":true}),
                    valid.clone()
                ]
            ),
            &session,
            &|| Ok(())
        )
        .is_err());
        let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Adjustment",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let leases = [crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "Artist work".into(),
            scopes: vec![crate::engine::Scope::layer(target)],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let cancelled = capture(&state, &[valid.clone()]);
    cancelled.cancellation().cancel();
    assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
    let failed = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("Interrupted adjustment mask".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(failed.cleanup_error.is_none());
    let captured = capture(&state, &[valid]);
    let pool = runtime.resident();
    let remaining = pool.limit() - pool.used();
    let held = pool.reserve_bytes(remaining, remaining).unwrap();
    assert!(prepare(captured, &session, &|| Ok(())).is_err());
    drop(held);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), disk);
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

#[test]
fn adjustment_selection_edges_and_layer_limits_match_resident_creation() {
    for variant in 0..7 {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = Document::new_depth(8, 8, 16).unwrap();
        original.layers[0].id = "base".into();
        let mut command = json!({"op":"adjustment.add","layer":"base","kind":"invert"});
        let success = !matches!(variant, 4 | 6);
        match variant {
            0 => original.selection = Some([10, 10, 20, 20]),
            1 => {
                original.selection = Some([0; 4]);
                original.selection_coverage = Some(crate::selection::Coverage {
                    mask: crate::raster::Raster::new(8, 8),
                    bounds: [0; 4],
                    origin: [0; 2],
                    contours: vec![],
                });
            }
            2 => {
                original.selection = Some([0, 0, 8, 8]);
                original.selection_polygon = Some(vec![[0., 0.], [8., 0.], [0., 8.]]);
                let mut mask = crate::raster::Raster::new(8, 8);
                for y in 0..8 {
                    for x in 0..8 {
                        mask.set(x, y, [77, 77, 77, 255]);
                    }
                }
                original.selection_coverage = Some(crate::selection::Coverage {
                    mask,
                    bounds: [0, 0, 8, 8],
                    origin: [0; 2],
                    contours: vec![],
                });
            }
            _ => {
                let count = match variant {
                    3 => 98,
                    4 | 5 => 99,
                    _ => 100,
                };
                while original.layers.len() < count {
                    let mut layer = crate::engine::Layer::new("Empty", "paint", 8, 8);
                    layer.id = format!("layer-{}", original.layers.len());
                    layer.pixels = crate::raster::Raster::new_depth(8, 8, 16);
                    original.layers.push(layer);
                }
                if variant <= 4 {
                    command["layers"] = json!(["base", "layer-1"]);
                }
            }
        }
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(),
            "adjustment-boundaries".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        let expected = Engine::preview_edits(original.clone(), &[command.clone()]);
        assert_eq!(expected.is_ok(), success, "resident variant {variant}");
        let result = prepare(capture(&state, &[command]), &session, &|| Ok(()));
        assert_eq!(
            result.is_ok(),
            success,
            "native variant {variant}: {:?}",
            result.as_ref().err()
        );
        if let Ok(prepared) = result {
            let mut expected = expected.unwrap();
            align(&mut expected, prepared.preview(), &original);
            compare(prepared.preview(), &expected);
            let actual = prepared
                .preview()
                .metadata()
                .layers()
                .find(|l| l.kind() == "adjustment")
                .unwrap();
            assert_eq!([actual.pixels().width(), actual.pixels().height()], [8, 8]);
            if let Some(mask) = actual.mask() {
                let expected = expected
                    .layers
                    .iter()
                    .find(|l| l.id == actual.id())
                    .unwrap()
                    .mask
                    .as_ref()
                    .unwrap();
                for (step, expected) in mask.steps().zip(&expected.steps) {
                    compare_raster(prepared.preview(), step.pixels(), &expected.pixels);
                }
                let surface = prepared
                    .preview()
                    .source(mask.steps().last().unwrap().pixels().slot())
                    .unwrap();
                if variant < 2 {
                    assert!(surface.is_known_zero());
                }
                if variant == 2 {
                    let pixels = surface
                        .prepare_region(
                            crate::raster::backed::Region::new([0, 0, 8, 8]).unwrap(),
                            &mut || Ok(()),
                        )
                        .unwrap();
                    assert_eq!(
                        pixels.pixel16(1, 1).unwrap(),
                        [65535, 65535, 65535, 77 * 257]
                    );
                    assert_eq!(pixels.pixel16(7, 7).unwrap(), [0; 4]);
                }
            }
            prepared
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
        }
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(runtime.disk().used().unwrap(), disk);
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
}
