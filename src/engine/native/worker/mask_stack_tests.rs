use super::grouping_tests::{compare, document, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Document, Engine,
};
use serde_json::json;

fn fixture(depth: u16) -> Document {
    let mut doc = document(depth);
    let layer = doc.layers.iter_mut().find(|l| l.id == "a_base").unwrap();
    let mask = layer.mask.as_mut().unwrap();
    mask.linked = false;
    mask.steps[0].id = "fill".into();
    let mut pixels =
        crate::raster::Raster::new_depth(layer.pixels.width, layer.pixels.height, depth);
    pixels.set16(2, 3, [12347, 12347, 12347, 49153]);
    let paint = mask.steps.iter_mut().find(|s| s.kind == "paint").unwrap();
    paint.pixels = pixels;
    paint.weight = 0.7;
    let mut doc = Engine::preview_edits(doc, &[
        json!({"op":"mask.step.add","layer":"a_base","kind":"blur","value":1.5}),
        json!({"op":"mask.step.add","layer":"a_base","kind":"gaussian","settings":{"radius":0.8}}),
        json!({"op":"mask.step.add","layer":"a_base","kind":"curves","settings":{"points":[[0.,0.],[1.,1.]]}}),
    ]).unwrap();
    for step in &mut doc
        .layers
        .iter_mut()
        .find(|l| l.id == "a_base")
        .unwrap()
        .mask
        .as_mut()
        .unwrap()
        .steps
    {
        step.id = step.kind.clone();
    }
    doc
}

fn align_added_steps(expected: &mut Document, original: &Document, actual: &NativeDocument) {
    let old = original
        .layers
        .iter()
        .find(|l| l.id == "a_base")
        .unwrap()
        .mask
        .as_ref()
        .unwrap();
    let expected = expected
        .layers
        .iter_mut()
        .find(|l| l.id == "a_base")
        .unwrap()
        .mask
        .as_mut()
        .unwrap();
    let actual = actual
        .metadata()
        .layers()
        .find(|l| l.id() == "a_base")
        .unwrap()
        .mask()
        .unwrap();
    assert_eq!(expected.steps.len(), actual.steps().len());
    for (expected, actual) in expected.steps.iter_mut().zip(actual.steps()) {
        if old.steps.iter().any(|step| step.id == expected.id) {
            assert_eq!(expected.id, actual.id());
        } else {
            assert!(!old.steps.iter().any(|step| step.id == actual.id()));
            expected.id = actual.id().into();
        }
    }
}

#[test]
fn mask_stack_controls_preserve_native_sources_and_render_through_atomic_history() {
    for depth in [8, 16] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let original = fixture(depth);
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "mask-stack".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let disk = runtime.disk().used().unwrap();
        for commands in [
            vec![json!({"op":"mask.step.add","layer":"a_base"})],
            vec![json!({"op":"mask.step.add","layer":"a_base","kind":"levels"})],
            vec![
                json!({"op":"mask.step.add","layer":"a_base","kind":"gaussian","settings":{"radius":1.25},"weight":0.4}),
            ],
            vec![
                json!({"op":"mask.step.add","layer":"a_base","kind":"adjust","settings":{"brightness":0.2,"contrast":1.1}}),
            ],
            vec![
                json!({"op":"mask.step.update","layer":"a_base","step":"fill","value":96,"weight":0.4}),
            ],
            vec![json!({"op":"mask.step.update","layer":"a_base","step":"blur","enabled":false})],
            vec![
                json!({"op":"mask.step.update","layer":"a_base","step":"gaussian","settings":{"radius":1.75}}),
            ],
            vec![
                json!({"op":"mask.step.update","layer":"a_base","step":"curves","settings":{"points":[[0.,0.2],[0.5,0.8],[1.,1.]]}}),
            ],
            vec![json!({"op":"mask.step.reorder","layer":"a_base","step":"paint","index":0})],
            vec![json!({"op":"mask.step.delete","layer":"a_base","step":"paint"})],
            vec![json!({"op":"mask.step.delete","layer":"a_base","step":"missing"})],
            vec![
                json!({"op":"mask.toggle","layer":"a_base","enabled":false}),
                json!({"op":"mask.step.update","layer":"a_base","step":"blur","value":0.25}),
            ],
            ["fill", "paint", "blur", "gaussian", "curves"]
                .into_iter()
                .map(|step| json!({"op":"mask.step.delete","layer":"a_base","step":step}))
                .collect(),
        ] {
            let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
            expected.revision = original.revision + 1;
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            align_added_steps(&mut expected, &original, preview.preview());
            compare(preview.preview(), &expected);
            for old in native.metadata().layers() {
                let layer = preview
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == old.id())
                    .unwrap();
                assert_eq!(
                    native.source(old.pixels().slot()).unwrap().key(),
                    preview
                        .preview()
                        .source(layer.pixels().slot())
                        .unwrap()
                        .key()
                );
                if let (Some(a), Some(b)) = (old.mask(), layer.mask()) {
                    for step in b.steps() {
                        let source = preview.preview().source(step.pixels().slot()).unwrap();
                        if let Some(original) = a.steps().find(|s| s.id() == step.id()) {
                            assert_eq!(
                                native.source(original.pixels().slot()).unwrap().key(),
                                source.key()
                            );
                        } else {
                            assert!(source.is_known_zero());
                        }
                        assert_eq!(step.pixels().depth(), depth);
                    }
                }
            }
            let scene = SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: "mask-stack-preview".into(),
                    document_id: expected.id.clone(),
                    revision: expected.revision,
                },
                preview.preview().clone(),
                vec![],
                None,
                None,
            )
            .unwrap();
            for (target, mask) in [(None, false), (Some("a_base"), true)] {
                let frame = crate::render::native_preview::test_frame(
                    &scene,
                    None,
                    8,
                    target,
                    mask,
                    &mut || Ok(()),
                )
                .unwrap();
                assert_eq!(
                    &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                    expected
                        .preview(None, 8, target, mask)
                        .unwrap()
                        .2
                        .as_slice()
                );
            }
            drop(scene);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
            let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            align_added_steps(&mut expected, &original, prepared.preview());
            let (base, history, delta) = prepared.into_parts();
            let committed = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Mask stack",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            state
                .commit_native(gate(&[]), committed)
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
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop((native, session, staged.seals, cache, runtime, connection));
    }
}

#[test]
fn mask_stack_failures_locks_and_late_reservations_preserve_human_work() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime();
    let original = fixture(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "mask-stack-refusal".into(),
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
    let valid = json!({"op":"mask.step.reorder","layer":"a_base","step":"paint","index":0});
    let disk = runtime.disk().used().unwrap();
    for command in [
        json!({"op":"mask.step.add","layer":"a_base","kind":"liquify"}),
        json!({"op":"mask.step.add","layer":"a_base","kind":"paint"}),
        json!({"op":"mask.step.add","layer":"a_base","kind":"levels","value":0}),
        json!({"op":"mask.step.add","layer":"a_base","kind":"gaussian","settings":{"radius":65}}),
        json!({"op":"mask.step.add","layer":"outside"}),
        json!({"op":"mask.step.update","layer":"a_base","step":"missing","value":1}),
        json!({"op":"mask.step.update","layer":"outside","step":"fill","value":1}),
        json!({"op":"mask.step.update","layer":"a_base","step":"blur","value":65}),
        json!({"op":"mask.step.update","layer":"a_base","step":"paint","weight":1.5}),
        json!({"op":"mask.step.update","layer":"a_base","step":"curves","settings":{"points":[[1.,0.],[0.,1.]]}}),
        json!({"op":"mask.step.update","layer":"a_base","step":"gaussian","settings":{"radius":-1}}),
        json!({"op":"mask.step.update","layer":"a_base","step":"paint","settings":{}}),
        json!({"op":"mask.step.reorder","layer":"a_base","step":"paint","index":5}),
        json!({"op":"mask.step.reorder","layer":"a_base","step":"missing","index":0}),
    ] {
        let commands = [valid.clone(), command];
        assert!(Engine::preview_edits(original.clone(), &commands).is_err());
        let failure = prepare(capture(&state, &commands), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(failure.cleanup_error.is_none(), "{}", failure.reason);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    for locked in ["a_base", "A", "outer"] {
        assert!(prepare(
            capture(
                &state,
                &[
                    json!({"op":"layer.update","layer":locked,"locked":true}),
                    valid.clone()
                ]
            ),
            &session,
            &|| Ok(())
        )
        .is_err());
    }
    for reserved in ["a_base", "A", "outer"] {
        let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Mask stack",
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
fn native_mask_creation_keeps_the_32_step_limit() {
    for count in [31, 32] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime();
        let mut original = fixture(16);
        let mask = original
            .layers
            .iter_mut()
            .find(|l| l.id == "a_base")
            .unwrap()
            .mask
            .as_mut()
            .unwrap();
        let step = mask.steps[0].clone();
        mask.steps = (0..count)
            .map(|i| {
                let mut step = step.clone();
                step.id = format!("step{i}");
                step
            })
            .collect();
        let command = json!({"op":"mask.step.add","layer":"a_base","kind":"invert"});
        assert_eq!(
            Engine::preview_edits(original.clone(), &[command.clone()]).is_ok(),
            count == 31
        );
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "mask-stack-limit".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native, 4).unwrap();
        match prepare(capture(&state, &[command]), &session, &|| Ok(())) {
            Ok(prepared) => {
                assert_eq!(count, 31);
                assert_eq!(
                    prepared
                        .preview()
                        .metadata()
                        .layers()
                        .find(|l| l.id() == "a_base")
                        .unwrap()
                        .mask()
                        .unwrap()
                        .steps()
                        .len(),
                    32
                );
                prepared
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
            }
            Err(failure) => {
                assert_eq!(count, 32);
                assert!(failure.reason.contains("32 steps"));
                assert!(failure.cleanup_error.is_none());
            }
        }
        assert_eq!(state.history_lengths(), [0, 0]);
        drop((state, session, staged.seals, cache, runtime, connection));
    }
}
