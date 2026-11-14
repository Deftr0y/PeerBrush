use super::grouping_tests::{compare, document, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Document, Engine,
};
use serde_json::json;

pub(super) fn fixture(depth: u16) -> Document {
    let mut doc = Engine::preview_edits(document(depth), &[
        json!({"op":"effect.add","layer":"a_base","kind":"blur","settings":{"radius":0.75}}),
        json!({"op":"selection","kind":"ellipse","rect":[1,1,7,7],"feather":1}),
        json!({"op":"liquify.stroke","layer":"a_base","points":[[2.,2.],[5.,4.]],"radius":2.,"strength":0.4}),
        json!({"op":"selection.clear"}),
        json!({"op":"selection","rect":[0,0,2,2]}),
    ]).unwrap();
    for effect in &mut doc
        .layers
        .iter_mut()
        .find(|l| l.id == "a_base")
        .unwrap()
        .effects
    {
        effect.id = effect.kind.clone();
    }
    let layer = doc.layers.iter_mut().find(|l| l.id == "a_base").unwrap();
    let mut second = layer
        .effects
        .iter()
        .find(|e| e.id == "liquify")
        .unwrap()
        .clone();
    second.id = "liquify_second".into();
    let mut settings = (*second.settings).clone();
    let mut coverage: crate::selection::Coverage =
        serde_json::from_value(settings["strokes"][0]["coverage"].clone()).unwrap();
    coverage.mask = crate::raster::Raster::new(coverage.mask.width, coverage.mask.height);
    coverage.mask.set(3, 3, [79, 79, 79, 255]);
    settings["strokes"][0]["coverage"] = json!(coverage);
    second.settings = settings.into();
    layer.effects.push(second);
    Engine::preview_edits(doc, &[]).unwrap()
}

#[test]
fn effect_reorder_delete_preserve_native_clips_pixels_rendering_and_history() {
    for depth in [8, 16] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let original = fixture(depth);
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "effect-stack".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let disk = runtime.disk().used().unwrap();
        let old_layer = native
            .metadata()
            .layers()
            .find(|l| l.id() == "a_base")
            .unwrap();
        assert_eq!(
            old_layer
                .effects()
                .find(|e| e.id() == "liquify")
                .unwrap()
                .clips()
                .len(),
            1
        );
        let clips = old_layer
            .effects()
            .filter(|e| e.kind() == "liquify")
            .map(|e| e.clips().next().unwrap().1.mask().slot())
            .collect::<Vec<_>>();
        assert_eq!(clips.len(), 2);
        assert_ne!(
            native.source(clips[0]).unwrap().key(),
            native.source(clips[1]).unwrap().key()
        );
        for commands in [
            vec![
                json!({"op":"effect.reorder","layer":"a_base","effect":"liquify_second","index":0}),
            ],
            vec![json!({"op":"effect.reorder","layer":"a_base","effect":"liquify","index":0})],
            vec![json!({"op":"effect.delete","layer":"a_base","effect":"liquify"})],
            vec![json!({"op":"effect.delete","layer":"a_base","effect":"blur"})],
            vec![
                json!({"op":"effect.reorder","layer":"a_base","effect":"invert","index":2}),
                json!({"op":"effect.delete","layer":"a_base","effect":"blur"}),
            ],
            ["invert", "blur", "liquify", "liquify_second"]
                .into_iter()
                .map(|effect| json!({"op":"effect.delete","layer":"a_base","effect":effect}))
                .collect(),
        ] {
            let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
            expected.revision = original.revision + 1;
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            compare(preview.preview(), &expected);
            for old in native.metadata().layers() {
                let actual = preview
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == old.id())
                    .unwrap();
                assert_eq!(
                    native.source(old.pixels().slot()).unwrap().key(),
                    preview
                        .preview()
                        .source(actual.pixels().slot())
                        .unwrap()
                        .key()
                );
                for effect in actual.effects() {
                    let original = old.effects().find(|e| e.id() == effect.id()).unwrap();
                    assert_eq!(effect.clips().len(), original.clips().len());
                    for ((stroke, clip), (old_stroke, old_clip)) in
                        effect.clips().zip(original.clips())
                    {
                        assert_eq!(stroke, old_stroke);
                        assert_eq!(clip.bounds(), old_clip.bounds());
                        assert_eq!(clip.origin(), old_clip.origin());
                        assert_eq!(clip.contours(), old_clip.contours());
                        assert_eq!(
                            preview.preview().source(clip.mask().slot()).unwrap().key(),
                            native.source(old_clip.mask().slot()).unwrap().key()
                        );
                    }
                }
            }
            for layer in &preview.preview().core.metadata.layers {
                let old = native
                    .core
                    .metadata
                    .layers
                    .iter()
                    .find(|l| l.id == layer.id)
                    .unwrap();
                for effect in &layer.effects {
                    let old = old.effects.iter().find(|e| e.id == effect.id).unwrap();
                    assert!(effect.settings.same_allocation(&old.settings));
                }
            }
            let kept_liquify = expected
                .layers
                .iter()
                .find(|l| l.id == "a_base")
                .unwrap()
                .effects
                .iter()
                .filter(|e| e.kind == "liquify")
                .count();
            assert_eq!(
                preview.preview().roster().len(),
                native.roster().len() - (2 - kept_liquify)
            );
            let scene = Arc::new(
                SceneSnapshot::from_native(
                    SceneIdentity {
                        project_id: "effect-stack-preview".into(),
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
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let committed = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Effects",
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
fn effect_structural_failures_and_late_reservations_are_atomic() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime();
    let original = fixture(16);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "effect-stack-refusal".into(),
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
    let valid = json!({"op":"effect.delete","layer":"a_base","effect":"liquify"});
    for command in [
        json!({"op":"effect.delete","layer":"a_base","effect":"missing"}),
        json!({"op":"effect.reorder","layer":"a_base","effect":"missing","index":0}),
        json!({"op":"effect.reorder","layer":"a_base","effect":"blur","index":-1}),
        json!({"op":"effect.reorder","layer":"a_base","effect":"blur","index":0.5}),
        json!({"op":"effect.reorder","layer":"a_base","effect":"blur","index":3}),
        json!({"op":"effect.reorder","layer":"a_base","effect":"blur"}),
        json!({"op":"effect.delete","layer":"outside","effect":"invert"}),
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
    for target in ["a_base", "A", "outer"] {
        let failure = prepare(
            capture(
                &state,
                &[
                    json!({"op":"layer.update","layer":target,"locked":true}),
                    valid.clone(),
                ],
            ),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failure.cleanup_error.is_none());
        let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Effects",
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
