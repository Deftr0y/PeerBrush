use super::effect_create_tests::compare_raster;
use super::grouping_tests::{compare, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

fn document(depth: u16, kind: &str) -> Document {
    let mut doc = Document::new_depth(8, 8, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.id = "base".into();
    layer.kind = kind.into();
    layer.x = 2;
    layer.y = -1;
    layer.pixels = crate::raster::Raster::new_depth(4, 4, depth);
    layer.color = [91, 171, 211, 173];
    if kind == "paint" {
        layer.pixels.set16(1, 2, [12347, 23459, 34571, 49153]);
    }
    Engine::preview_edits(doc, &[json!({"op":"mask.add","layer":"base","value":137})]).unwrap()
}

fn align(expected: &mut Document, native: &NativeDocument) {
    for (layer, actual) in expected.layers.iter_mut().zip(native.metadata().layers()) {
        if layer.id != "base" {
            layer.id = actual.id().into();
        }
    }
}

fn compare_pixels(native: &NativeDocument, expected: &Document) {
    compare(native, expected);
    for (actual, expected) in native.metadata().layers().zip(&expected.layers) {
        compare_raster(native, actual.pixels(), &expected.pixels);
        if let Some(mask) = actual.mask() {
            for (actual, expected) in mask.steps().zip(&expected.mask.as_ref().unwrap().steps) {
                compare_raster(native, actual.pixels(), &expected.pixels);
            }
        }
    }
}

#[test]
fn legacy_fill_recolor_materialization_folder_insertion_masks_and_history_match() {
    for depth in [8, 16] {
        for kind in ["paint", "fill", "group"] {
            for selection in [
                json!({"op":"selection.clear"}),
                json!({"op":"selection","rect":[1,1,6,7]}),
                json!({"op":"selection","kind":"ellipse","rect":[1,1,6,7],"feather":1}),
                json!({"op":"selection","rect":[0,0,0,0]}),
            ] {
                let root = Fixture::new();
                let (runtime, connection) =
                    root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
                let original = Engine::preview_edits(document(depth, kind), &[selection]).unwrap();
                let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
                let mut staged = CapturedScene::from_parts(
                    original.clone(),
                    "legacy-fill".into(),
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
                for command in [
                    json!({"op":"fill","layer":"base","color":[37,83,127,193]}),
                    json!({"op":"fill","layer":"base","rect":[4,4,4,4],"color":[37,83,127,193]}),
                    json!({"op":"fill","layer":"base","rect":[-2,-2,10,10],"color":[37,83,127,0]}),
                    json!({"op":"fill","layer":"base"}),
                    json!({"op":"fill","layer":"base","mask":true,"color":[37,83,127,193]}),
                ] {
                    let disk = runtime.disk().used().unwrap();
                    let mut expected =
                        Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
                    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                    let preview =
                        prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
                    align(&mut expected, preview.preview());
                    compare_pixels(preview.preview(), &expected);
                    assert_eq!(
                        preview.created_layers().count(),
                        usize::from(kind == "group" && command["mask"] != true)
                    );
                    if kind == "fill" && original.selection.is_none() && command["mask"] != true {
                        assert_eq!(
                            preview
                                .preview()
                                .source(
                                    preview
                                        .preview()
                                        .metadata()
                                        .layer(0)
                                        .unwrap()
                                        .pixels()
                                        .slot()
                                )
                                .unwrap()
                                .key(),
                            native
                                .source(native.metadata().layer(0).unwrap().pixels().slot())
                                .unwrap()
                                .key()
                        );
                    }
                    preview
                        .abort(&session, &|| session.cleanup_check())
                        .unwrap();
                    assert_eq!(runtime.disk().used().unwrap(), disk);
                    if command.get("color").is_some() {
                        continue;
                    }
                    let prepared =
                        prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                    align(&mut expected, prepared.preview());
                    let (base, history, delta) = prepared.into_parts();
                    let commit = history
                        .prepare_commit(
                            &base,
                            delta,
                            HistoryRequest {
                                actor: "human",
                                label: "Fill",
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
                        compare_pixels(state.native().unwrap(), &expected);
                    }
                }
                drop((native, session, staged.seals, cache, runtime, connection));
            }
        }
    }
}

#[test]
fn soft_fill_preserves_old_visual_pixels_and_native_child_alpha() {
    for kind in ["fill", "group"] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = Document::new_depth(8, 8, 16).unwrap();
        original.layers[0].id = "base".into();
        original.layers[0].kind = kind.into();
        original.layers[0].color = [91, 171, 211, 255];
        original.selection = Some([2, 2, 6, 6]);
        let mut mask = crate::raster::Raster::new(8, 8);
        for y in 2..6 {
            for x in 2..6 {
                mask.set(x, y, [77, 77, 77, 255]);
            }
        }
        original.selection_coverage = Some(crate::selection::Coverage {
            mask,
            bounds: [2, 2, 6, 6],
            origin: [0; 2],
            contours: vec![],
        });
        let command = json!({"op":"fill","layer":"base","color":[37,83,127,193]});
        let mut expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
        if kind == "fill" {
            assert_eq!(
                expected.layers[0].pixels.get16(0, 0),
                [91 * 257, 171 * 257, 211 * 257, 65535]
            );
        } else {
            assert_eq!(expected.layers[1].pixels.depth, 16);
            assert_eq!(expected.layers[1].pixels.get16(3, 3)[3], 14978);
        }
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "soft-fill".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let preview = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
        align(&mut expected, preview.preview());
        compare_pixels(preview.preview(), &expected);
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
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

#[test]
fn legacy_fill_reserves_its_actual_semantics_and_retires_failed_batches() {
    for kind in ["fill", "group"] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let original = document(16, kind);
        let command = json!({"op":"fill","layer":"base","rect":[0,0,1,1],"color":[37,83,127,193]});
        let mut engine = Engine::new();
        engine.resident_state_mut().expect("resident fixture").document = original.clone();
        assert_eq!(engine.scopes(&command)[0].rect, None);
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "fill-reservations".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        let prepared = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Fill",
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
            scopes: vec![crate::engine::Scope {
                target: Some("base".into()),
                rect: Some([7, 7, 8, 8]),
            }],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        let failed = prepare(
            capture(
                &state,
                &[command.clone(), json!({"op":"fill","layer":"missing"})],
            ),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
        let cancelled = capture(&state, &[command]);
        cancelled.cancellation().cancel();
        assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.native().unwrap().key(), native.key());
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
}
