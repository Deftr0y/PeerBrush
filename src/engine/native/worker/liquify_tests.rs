use super::effect_create_tests::{align_ids, compare_raster};
use super::effect_stack_tests::fixture;
use super::grouping_tests::{compare, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Engine,
};
use serde_json::json;

#[test]
fn liquify_strokes_share_selection_sources_and_match_native_depth_rendering_and_history() {
    for depth in [8, 16] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = Engine::preview_edits(
            fixture(depth),
            &[
                json!({"op":"layer.update","layer":"A","blend":"normal"}),
                json!({"op":"selection","kind":"ellipse","rect":[1,1,7,7],"feather":1}),
            ],
        )
        .unwrap();
        original
            .layers
            .iter_mut()
            .find(|l| l.id == "a_base")
            .unwrap()
            .x = -2;
        original
            .layers
            .iter_mut()
            .find(|l| l.id == "a_base")
            .unwrap()
            .y = 1;
        original.layers.iter_mut().find(|l| l.id == "A").unwrap().x = 4;
        let coverage = original.selection_coverage.as_mut().unwrap();
        coverage.mask.retained = Some(Box::new(crate::retained::Original::from(&coverage.mask)));
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(),
            "liquify-command".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let coverage = native.metadata().selection_coverage().unwrap();
        let clip_key = native.source(coverage.mask().slot()).unwrap().key();
        let original_key = native
            .source(coverage.mask().retained().unwrap().pixels().slot())
            .unwrap()
            .key();
        let stroke = json!({"op":"liquify.stroke","layer":"a_base","points":[[2.,2.],[5.,4.]],"radius":2.,"strength":0.4});
        for commands in [
            vec![stroke.clone()],
            vec![
                json!({"op":"liquify.stroke","layer":"a_base","effect":"liquify","points":[[2.,2.],[4.,4.]],"radius":2.}),
            ],
            vec![
                json!({"op":"liquify.stroke","layer":"outside","points":[[2.,2.],[4.,4.]],"radius":2.}),
            ],
            vec![json!({"op":"liquify.stroke","layer":"A","points":[[2.,2.],[4.,4.]],"radius":2.})],
            vec![
                json!({"op":"liquify.stroke","layer":"a_base","points":[[3.,3.]],"mode":"expand","radius":2.}),
            ],
            vec![
                json!({"op":"liquify.stroke","layer":"a_base","points":[[3.,3.]],"mode":"pinch","radius":2.}),
            ],
            vec![
                json!({"op":"liquify.stroke","layer":"a_base","points":[[3.,3.]],"mode":"restore","radius":2.}),
            ],
            vec![
                json!({"op":"liquify.stroke","layer":"a_base","points":[[2.,2.],[5.,4.]],"stroke_axis":"horizontal","radius":2.}),
            ],
            vec![
                json!({"op":"effect.update","layer":"a_base","effect":"liquify_second","enabled":false}),
                stroke.clone(),
            ],
            vec![
                json!({"op":"effect.update","layer":"a_base","effect":"liquify_second","weight":0}),
                stroke.clone(),
            ],
            vec![
                stroke.clone(),
                json!({"op":"selection.clear"}),
                stroke.clone(),
            ],
            vec![
                stroke.clone(),
                json!({"op":"selection","rect":[0,0,2,2]}),
                stroke.clone(),
                json!({"op":"selection.clear"}),
            ],
            vec![json!({"op":"selection.clear"}), stroke.clone()],
        ] {
            let disk = runtime.disk().used().unwrap();
            let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
            expected.revision = original.revision + 1;
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            align_ids(&mut expected, preview.preview(), &original);
            compare(preview.preview(), &expected);
            let mut shared = 0;
            for layer in preview.preview().metadata().layers() {
                let old = native
                    .metadata()
                    .layers()
                    .find(|l| l.id() == layer.id())
                    .unwrap();
                assert_eq!(
                    native.source(old.pixels().slot()).unwrap().key(),
                    preview
                        .preview()
                        .source(layer.pixels().slot())
                        .unwrap()
                        .key()
                );
                let expected = expected.layers.iter().find(|l| l.id == layer.id()).unwrap();
                for effect in layer.effects() {
                    let old_effect = old.effects().find(|e| e.id() == effect.id());
                    let old_strokes = old_effect.map_or(0, |e| {
                        e.settings()["strokes"].as_array().map_or(0, Vec::len)
                    });
                    let expected = expected
                        .effects
                        .iter()
                        .find(|e| e.id == effect.id())
                        .unwrap();
                    for (stroke, clip) in effect.clips() {
                        let expected: crate::selection::Coverage = serde_json::from_value(
                            expected.settings["strokes"][stroke as usize]["coverage"].clone(),
                        )
                        .unwrap();
                        compare_raster(preview.preview(), clip.mask(), &expected.mask);
                        if let Some((_, old_clip)) =
                            old_effect.and_then(|e| e.clips().find(|(i, _)| *i == stroke))
                        {
                            assert_eq!(
                                native.source(old_clip.mask().slot()).unwrap().key(),
                                preview.preview().source(clip.mask().slot()).unwrap().key()
                            );
                        }
                        if stroke as usize >= old_strokes
                            && preview.preview().source(clip.mask().slot()).unwrap().key()
                                == clip_key
                        {
                            shared += 1;
                            assert_eq!(
                                preview
                                    .preview()
                                    .source(clip.mask().retained().unwrap().pixels().slot())
                                    .unwrap()
                                    .key(),
                                original_key
                            );
                        }
                    }
                }
            }
            assert_eq!(shared > 0, commands[0]["op"] != "selection.clear");
            let scene = Arc::new(
                SceneSnapshot::from_native(
                    SceneIdentity {
                        project_id: "liquify-preview".into(),
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
            let request =
                crate::render::native_preview::Request::capture(scene, read, None, 8, None, false)
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
            let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            align_ids(&mut expected, prepared.preview(), &original);
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Liquify",
                        task: None,
                        gesture: Some("liquify-gesture"),
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

#[test]
fn liquify_refusals_and_late_locks_leave_history_and_sources_unchanged() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = fixture(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "liquify-refusal".into(),
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
    let valid =
        json!({"op":"liquify.stroke","layer":"a_base","points":[[2.,2.],[4.,4.]],"radius":2.});
    for patch in [
        json!({"points":[]}),
        json!({"points":[[2.,2.,0.5]]}),
        json!({"pressures":[0.5,0.5]}),
        json!({"mode":"invalid"}),
        json!({"radius":0}),
        json!({"strength":1.5}),
        json!({"mask":true}),
        json!({"effect":"missing"}),
        json!({"effect":"blur"}),
        json!({"points":vec![[2.,2.];2049]}),
    ] {
        let mut invalid = valid.clone();
        for (key, value) in patch.as_object().unwrap() {
            invalid[key] = value.clone();
        }
        let commands = [valid.clone(), invalid];
        assert!(Engine::preview_edits(original.clone(), &commands).is_err());
        let failure = prepare(capture(&state, &commands), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(failure.cleanup_error.is_none(), "{}", failure.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    for target in ["a_base", "A", "outer"] {
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
                    label: "Liquify",
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
    }
    let cancelled = capture(&state, &[valid.clone()]);
    cancelled.cancellation().cancel();
    assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
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
fn liquify_empty_selection_targets_and_control_limits_match_the_shared_engine() {
    for variant in 0..9 {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = fixture(16);
        let mut command = json!({"op":"liquify.stroke","layer":"a_base","points":[[2.,2.]],"mode":"expand","radius":2.,"strength":0.});
        let expected_success = matches!(variant, 0 | 1 | 4 | 5 | 7);
        let layer = original
            .layers
            .iter_mut()
            .find(|l| l.id == "a_base")
            .unwrap();
        match variant {
            0 => {
                original.selection = Some([0; 4]);
                original.selection_polygon = None;
                original.selection_coverage = Some(crate::selection::Coverage {
                    mask: crate::raster::Raster::new(8, 8),
                    bounds: [0; 4],
                    origin: [0; 2],
                    contours: vec![],
                });
                command["effect"] = json!("missing");
                command["radius"] = json!(0);
            }
            1 | 2 => {
                for effect in &mut layer.effects {
                    if effect.kind == "liquify" {
                        effect.enabled = false;
                    }
                }
                if variant == 2 {
                    command["effect"] = json!("liquify");
                }
            }
            3 | 4 => {
                let invert = layer
                    .effects
                    .iter()
                    .find(|e| e.kind == "invert")
                    .unwrap()
                    .clone();
                let liquify = layer
                    .effects
                    .iter()
                    .find(|e| e.id == "liquify")
                    .unwrap()
                    .clone();
                layer.effects = (0..32)
                    .map(|i| {
                        let mut effect = if i == 31 && variant == 4 {
                            liquify.clone()
                        } else {
                            invert.clone()
                        };
                        effect.id = format!("effect-{i}");
                        effect
                    })
                    .collect();
            }
            5 | 6 => {
                command["effect"] = json!("liquify");
                let effect = layer
                    .effects
                    .iter_mut()
                    .find(|e| e.id == "liquify")
                    .unwrap();
                effect.settings = json!({"radius":2.,"strength":1.,"strokes":vec![json!({"mode":"expand","points":[[2.,2.]],"strength":0.}); if variant == 5 { 127 } else { 128 }]}).into();
            }
            7 | 8 => {
                command["effect"] = json!("liquify");
                let effect = layer
                    .effects
                    .iter_mut()
                    .find(|e| e.id == "liquify")
                    .unwrap();
                let strokes: Vec<_> = (0..4).map(|i| json!({"points":vec![[2.,2.]; if i == 3 && variant == 7 { 2047 } else { 2048 }],"strength":0.})).collect();
                effect.settings = json!({"radius":2.,"strength":1.,"strokes":strokes}).into();
            }
            _ => unreachable!(),
        }
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(),
            "liquify-boundaries".into(),
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
        let expected = Engine::preview_edits(original.clone(), &[command.clone()]);
        assert_eq!(
            expected.is_ok(),
            expected_success,
            "resident variant {variant}"
        );
        let disk = runtime.disk().used().unwrap();
        let actual = prepare(capture(&state, &[command]), &session, &|| Ok(()));
        assert_eq!(
            actual.is_ok(),
            expected_success,
            "native variant {variant}: {:?}",
            actual.as_ref().err()
        );
        if let Ok(actual) = actual {
            let mut expected = expected.unwrap();
            align_ids(&mut expected, actual.preview(), &original);
            compare(actual.preview(), &expected);
            if variant == 0 {
                assert!(Arc::ptr_eq(
                    &native.core.roster,
                    &actual.preview().core.roster
                ));
            }
            actual.abort(&session, &|| session.cleanup_check()).unwrap();
        }
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
}
