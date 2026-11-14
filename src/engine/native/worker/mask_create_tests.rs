use super::grouping_tests::{compare, document, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

fn align(expected: &mut Document, native: &NativeDocument, target: &str) {
    let mask = expected
        .layers
        .iter_mut()
        .find(|l| l.id == target)
        .unwrap()
        .mask
        .as_mut()
        .unwrap();
    let actual = native
        .metadata()
        .layers()
        .find(|l| l.id() == target)
        .unwrap()
        .mask()
        .unwrap();
    assert_eq!(mask.steps.len(), actual.steps().len());
    let mut ids = std::collections::HashSet::new();
    for (step, actual) in mask.steps.iter_mut().zip(actual.steps()) {
        assert!(ids.insert(actual.id()));
        step.id = actual.id().into();
    }
}

#[test]
fn native_mask_creation_preserves_precision_selection_and_single_history_change() {
    for depth in [8, 16] {
        for (selection, target) in [(0, "a_base"), (1, "a_base"), (2, "a_base"), (1, "A")] {
            let root = Fixture::new();
            let (runtime, connection) =
                root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let mut original = document(depth);
            original
                .layers
                .iter_mut()
                .find(|l| l.id == target)
                .unwrap()
                .mask = None;
            if selection == 0 {
                original =
                    Engine::preview_edits(original, &[json!({"op":"selection.clear"})]).unwrap();
            } else if selection == 2 {
                original = Engine::preview_edits(
                    original,
                    &[json!({"op":"selection","kind":"ellipse","rect":[0,0,6,7],"feather":1})],
                )
                .unwrap();
            }
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "mask-create".into(),
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
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let disk = runtime.disk().used().unwrap();
            // Selected creation ignores both value and mode, as the public command does.
            let command = if selection == 0 {
                json!({"op":"mask.add","layer":target,"value":47,"mode":"unused"})
            } else {
                json!({"op":"mask.add","layer":target,"value":-99,"mode":"unused"})
            };
            let mut expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            expected.revision = original.revision + 1;
            let prepared =
                prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            align(&mut expected, prepared.preview(), target);
            compare(prepared.preview(), &expected);
            assert_eq!(
                prepared.preview().metadata().selection(),
                native.metadata().selection()
            );
            for (a, b) in [
                (
                    native.metadata().selection_coverage(),
                    prepared.preview().metadata().selection_coverage(),
                ),
                (
                    native.metadata().selection_previous(),
                    prepared.preview().metadata().selection_previous(),
                ),
            ] {
                match (a, b) {
                    (Some(a), Some(b)) => {
                        assert_eq!(a.bounds(), b.bounds());
                        assert_eq!(a.origin(), b.origin());
                        assert_eq!(a.contours(), b.contours());
                        assert_eq!(
                            native.source(a.mask().slot()).unwrap().key(),
                            prepared.preview().source(b.mask().slot()).unwrap().key()
                        );
                    }
                    (None, None) => {}
                    _ => panic!("Mask creation changed selection coverage"),
                }
            }
            for layer in native.metadata().layers() {
                let actual = prepared
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == layer.id())
                    .unwrap();
                assert_eq!(
                    native.source(layer.pixels().slot()).unwrap().key(),
                    prepared
                        .preview()
                        .source(actual.pixels().slot())
                        .unwrap()
                        .key()
                );
            }
            let mask = prepared
                .preview()
                .metadata()
                .layers()
                .find(|l| l.id() == target)
                .unwrap()
                .mask()
                .unwrap();
            assert!(mask.enabled() && mask.linked());
            assert_eq!(mask.steps().len(), if selection == 0 { 2 } else { 1 });
            let expected_mask = expected
                .layers
                .iter()
                .find(|l| l.id == target)
                .unwrap()
                .mask
                .as_ref()
                .unwrap();
            for (step, expected_step) in mask.steps().zip(&expected_mask.steps) {
                let source = prepared.preview().source(step.pixels().slot()).unwrap();
                assert_eq!(source.depth(), depth);
                if selection == 0 {
                    assert!(source.is_known_zero());
                }
                let rows = source
                    .prepare_region(
                        crate::raster::backed::Region::new([
                            0,
                            0,
                            source.width() as i32,
                            source.height() as i32,
                        ])
                        .unwrap(),
                        &mut || Ok(()),
                    )
                    .unwrap();
                for y in 0..source.height() as i32 {
                    for x in 0..source.width() as i32 {
                        assert_eq!(
                            rows.pixel16(x, y).unwrap(),
                            expected_step.pixels.get16(x, y)
                        );
                    }
                }
            }
            prepared
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            align(&mut expected, prepared.preview(), target);
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Add mask",
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
            drop((
                base,
                history,
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
}

#[test]
fn mask_creation_refuses_existing_masks_and_retires_failed_selected_output() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime();
    let original = document(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged =
        CapturedScene::from_parts(original, "mask-create-refusal".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let valid = json!({"op":"mask.add","layer":"outside"});
    for command in [
        json!({"op":"mask.add","layer":"a_base"}),
        valid.clone(),
        json!({"op":"mask.add","layer":"missing"}),
    ] {
        let failure = prepare(
            capture(&state, &[valid.clone(), command]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failure.cleanup_error.is_none(), "{}", failure.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    for reserved in ["outside", "@selection"] {
        let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Mask",
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
            scopes: vec![crate::engine::Scope::layer(reserved)],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    for target in ["outside", "A"] {
        let failure = prepare(
            capture(
                &state,
                &[
                    json!({"op":"layer.update","layer":target,"locked":true}),
                    json!({"op":"mask.add","layer":target}),
                ],
            ),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failure.cleanup_error.is_none());
    }
    let cancelled = capture(&state, &[valid.clone()]);
    cancelled.cancellation().cancel();
    assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
    let edit = capture(&state, &[valid]);
    let pool = runtime.resident();
    let remaining = pool.limit() - pool.used();
    let held = pool.reserve_bytes(remaining, remaining).unwrap();
    assert!(prepare(edit, &session, &|| Ok(())).is_err());
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
fn empty_native_masks_do_not_materialize_a_large_frame() {
    for depth in [8, 16] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime();
        let session = runtime.begin().unwrap();
        let native = NativeDocument::new_blank(
            16000,
            16000,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let layer = native.metadata().layer(0).unwrap().id().to_owned();
        let state = DocumentAuthority::from_native(native, 4).unwrap();
        for command in [
            json!({"op":"mask.add","layer":layer}),
            json!({"op":"mask.add","layer":layer,"value":0}),
        ] {
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            let mask = prepared
                .preview()
                .metadata()
                .layer(0)
                .unwrap()
                .mask()
                .unwrap();
            assert_eq!(mask.steps().len(), 2);
            for step in mask.steps() {
                assert!(prepared
                    .preview()
                    .source(step.pixels().slot())
                    .unwrap()
                    .is_known_zero());
            }
            prepared
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
        }
        for value in [-1, 256] {
            assert!(prepare(
                capture(
                    &state,
                    &[json!({"op":"mask.add","layer":layer,"value":value})]
                ),
                &session,
                &|| Ok(())
            )
            .is_err());
        }
        assert_eq!(runtime.disk().used().unwrap(), 0);
        assert_eq!(state.history_lengths(), [0, 0]);
        drop((state, session, runtime, connection));
    }
}

#[test]
fn full_size_native_masks_admit_stacked_feathers_and_render_a_bounded_row() {
    for depth in [8, 16] {
        let root = Fixture::new();
        let (runtime, _connection) = root.runtime_with_budgets(64 * 1024 * 1024, 8 * 1024 * 1024);
        let session = runtime.begin().unwrap();
        let native = NativeDocument::new_blank(
            16000,
            16000,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let layer = native.metadata().layer(0).unwrap().id().to_owned();
        let state = DocumentAuthority::from_native(native, 4).unwrap();
        let commands = [
            json!({"op":"mask.add","layer":layer,"value":128}),
            json!({"op":"mask.step.add","layer":layer,"kind":"gaussian","settings":{"radius":64}}),
            json!({"op":"mask.step.add","layer":layer,"kind":"gaussian","settings":{"radius":64}}),
        ];
        let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        let candidate = prepared.preview();
        let layer = candidate.metadata().layer(0).unwrap();
        assert_eq!(layer.mask().unwrap().steps().count(), 4);
        let row = crate::mask::region::prepare_native(candidate, layer, [0, 7999, 16000, 8000], &mut || {
            session.check()
        })
        .unwrap();
        for x in [0, 7999, 15999] {
            assert_eq!(
                row.native(x, 7999).unwrap(),
                if depth == 16 { 128 * 257 } else { 128 }
            );
        }
        drop(row);
        prepared.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), 0);
        assert!(state
            .native()
            .unwrap()
            .metadata()
            .layer(0)
            .unwrap()
            .mask()
            .is_none());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
}
