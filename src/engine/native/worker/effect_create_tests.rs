use super::effect_stack_tests::fixture;
use super::grouping_tests::{compare, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Document, Engine,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};

fn pattern(depth: u16) -> String {
    let words = [12347, 23459, 34571, 49153, 45123, 571, 137, 65535];
    STANDARD.encode(if depth == 16 {
        crate::raster::png16(2, 1, &words).unwrap()
    } else {
        crate::raster::png(2, 1, &words.map(crate::raster::project16)).unwrap()
    })
}

pub(super) fn align_ids(expected: &mut Document, native: &NativeDocument, original: &Document) {
    for layer in &mut expected.layers {
        let old = original.layers.iter().find(|l| l.id == layer.id).unwrap();
        let actual = native
            .metadata()
            .layers()
            .find(|l| l.id() == layer.id)
            .unwrap();
        for (effect, actual) in layer.effects.iter_mut().zip(actual.effects()) {
            if !old.effects.iter().any(|e| e.id == effect.id) {
                assert!(!old.effects.iter().any(|e| e.id == actual.id()));
                effect.id = actual.id().into();
            }
        }
    }
}

pub(super) fn compare_raster(
    native: &NativeDocument,
    actual: RasterMetadata<'_>,
    expected: &crate::raster::Raster,
) {
    let source = native.source(actual.slot()).unwrap();
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
    assert_eq!(source.depth(), expected.depth);
    for y in 0..source.height() as i32 {
        for x in 0..source.width() as i32 {
            assert_eq!(rows.pixel16(x, y).unwrap(), expected.get16(x, y));
        }
    }
    match (actual.retained(), &expected.retained) {
        (Some(actual), Some(expected)) => compare_raster(native, actual.pixels(), &expected.pixels),
        (None, None) => (),
        _ => panic!("Historic original was not preserved"),
    }
}

#[test]
fn effect_creation_and_settings_preserve_native_sources_clips_and_atomic_history() {
    for depth in [8, 16] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = fixture(depth);
        let layer = original.layers.iter_mut().find(|layer| layer.id == "a_base").unwrap();
        let mut shadow = crate::effects::defaults("inner_shadow");
        shadow["angle"] = json!(35.);
        layer.effects.push(crate::effects::Effect {
            id: "local-shadow".into(), kind: "inner_shadow".into(), enabled: true, weight: 1., settings: shadow.into(),
        });
        let layer = original.layers.iter().find(|l| l.id == "a_base").unwrap();
        let mut settings = (*layer
            .effects
            .iter()
            .find(|e| e.id == "liquify")
            .unwrap()
            .settings)
            .clone();
        let mut coverage: crate::selection::Coverage =
            serde_json::from_value(settings["strokes"][0]["coverage"].clone()).unwrap();
        coverage.mask.retained = Some(Box::new(crate::retained::Original::from(&coverage.mask)));
        settings["strokes"][0]["coverage"] = json!(coverage);
        settings["strokes"][0]["strength"] = json!(0.2);
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "effect-create".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let mut cases: Vec<Vec<Value>> = crate::effects::KINDS
            .iter()
            .map(|kind| vec![json!({"op":"effect.add","layer":"a_base","kind":kind,"weight":0.7})])
            .collect();
        cases.extend([
            vec![json!({"op":"effect.add","layer":"a_base","kind":"levels","settings":{"input_mapping":"exact","gamma":1.8,"channels":[{"output_black":1.,"output_white":0.},{"gamma":1.2},{}]},"weight":0.7})],
            vec![json!({"op":"effect.add","layer":"a_base","kind":"levels","settings":{"input_mapping":"exact","black":17./255.,"white":18./255.,"gamma":9.99},"weight":0.7})],
            vec![json!({"op":"effect.add","layer":"a_base","kind":"levels","settings":{"output_black":0.81,"output_white":0.13},"weight":0.7})],
            vec![json!({"op":"effect.add","layer":"a_base","kind":"pattern_overlay","settings":{"pattern":"bitmap","bitmap_png":pattern(depth)}})],
            vec![json!({"op":"effect.add","layer":"a_base"})],
            vec![json!({"op":"effect.add","layer":"a_base","kind":"drop_shadow","use_global_light":true})],
            vec![json!({"op":"effect.update","layer":"a_base","effect":"local-shadow","use_global_light":true})],
            vec![json!({"op":"document.global_light","angle":90}),
                json!({"op":"effect.update","layer":"a_base","effect":"local-shadow","use_global_light":true}),
                json!({"op":"effect.update","layer":"a_base","effect":"local-shadow","use_global_light":false})],
            vec![json!({"op":"effect.add","layer":"a_base","kind":"liquify","settings":settings})],
            vec![json!({"op":"effect.update","layer":"a_base","effect":"liquify","settings":settings})],
            vec![json!({"op":"effect.update","layer":"a_base","effect":"liquify","settings":{"radius":2.,"strength":0.5,"strokes":[]}})],
            vec![json!({"op":"effect.update","layer":"a_base","effect":"liquify","weight":0.4,"enabled":false})],
            vec![json!({"op":"effect.update","layer":"a_base","effect":"blur","kind":"ignored","enabled":"ignored","settings":{"radius":1.25}})],
            vec![json!({"op":"effect.update","layer":"a_base","effect":"invert","enabled":false}),
                json!({"op":"effect.update","layer":"a_base","effect":"blur","weight":0.25})],
        ]);
        for commands in cases {
            let baseline = runtime.disk().used().unwrap();
            let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
            expected.revision = original.revision + 1;
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            align_ids(&mut expected, preview.preview(), &original);
            compare(preview.preview(), &expected);
            for layer in preview.preview().metadata().layers() {
                let expected = expected.layers.iter().find(|l| l.id == layer.id()).unwrap();
                for effect in layer.effects() {
                    let expected = expected
                        .effects
                        .iter()
                        .find(|e| e.id == effect.id())
                        .unwrap();
                    for (stroke, coverage) in effect.clips() {
                        let expected: crate::selection::Coverage = serde_json::from_value(
                            expected.settings["strokes"][stroke as usize]["coverage"].clone(),
                        )
                        .unwrap();
                        compare_raster(preview.preview(), coverage.mask(), &expected.mask);
                    }
                }
            }
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
            }
            if commands
                .iter()
                .all(|c| c.get("settings").is_none() && c["op"] == "effect.update")
            {
                let actual = &preview.preview().core.metadata.layers;
                for (old, actual) in native.core.metadata.layers.iter().zip(actual) {
                    for (old, actual) in old.effects.iter().zip(&actual.effects) {
                        assert!(old.settings.same_allocation(&actual.settings));
                    }
                }
            }
            let scene = Arc::new(
                SceneSnapshot::from_native(
                    SceneIdentity {
                        project_id: "effect-create-preview".into(),
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
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert_eq!(state.history_lengths(), [0, 0]);

            let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            align_ids(&mut expected, prepared.preview(), &original);
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
            // Published clip sources remain owned by history; only aborted previews
            // return immediately to the pre-command disk baseline.
            drop(state);
        }
        drop((native, session, staged.seals, cache, runtime, connection));
    }
}

#[test]
fn effect_creation_failures_retire_historic_clip_outputs() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let original = fixture(16);
    let settings = &original
        .layers
        .iter()
        .find(|l| l.id == "a_base")
        .unwrap()
        .effects
        .iter()
        .find(|e| e.id == "liquify")
        .unwrap()
        .settings;
    let valid = json!({"op":"effect.add","layer":"a_base","kind":"liquify","settings":settings});
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "effect-create-refusal".into(),
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
    let baseline = runtime.disk().used().unwrap();
    let mut invalid_coverage = valid.clone();
    invalid_coverage["settings"]["strokes"][0]["coverage"]["bounds"] = json!([2, 2, 1, 1]);
    let mut invalid_geometry = valid.clone();
    invalid_geometry["settings"]["strokes"][0]["points"] = json!([]);
    for invalid in [
        invalid_coverage,
        invalid_geometry,
        json!({"op":"effect.update","layer":"a_base","effect":"missing"}),
        json!({"op":"effect.update","layer":"a_base","effect":"blur","settings":null}),
        json!({"op":"effect.update","layer":"a_base","effect":"blur","weight":-0.1}),
        json!({"op":"effect.add","layer":"a_base","kind":"unsupported"}),
        json!({"op":"effect.add","layer":"a_base","kind":"pattern_overlay","settings":{"pattern":"bitmap","bitmap_png":"broken"}}),
    ] {
        let commands = [valid.clone(), invalid];
        assert!(Engine::preview_edits(original.clone(), &commands).is_err());
        let failure = prepare(capture(&state, &commands), &session, &|| Ok(()))
            .err()
            .unwrap();
        assert!(failure.cleanup_error.is_none(), "{}", failure.reason);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
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
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    let cancelled = capture(&state, &[valid.clone()]);
    cancelled.cancellation().cancel();
    assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
    let interrupted = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > baseline {
            Err("Interrupted during clip staging".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(interrupted.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let edit = capture(&state, &[valid]);
    let pool = runtime.resident();
    let remaining = pool.limit() - pool.used();
    let held = pool.reserve_bytes(remaining, remaining).unwrap();
    assert!(prepare(edit, &session, &|| Ok(())).is_err());
    drop(held);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), baseline);
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
fn effect_creation_keeps_stack_precision_and_current_frame_limits() {
    let root = Fixture::new();
    let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    for depth in [8, 16] {
        let native = NativeDocument::new_blank(
            16000,
            16000,
            depth,
            &runtime.resident(),
            &runtime.disk(),
            &|| Ok(()),
        )
        .unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let target = native.metadata().layers().next().unwrap().id();
        let baseline = runtime.disk().used().unwrap();
        let command = json!({"op":"effect.add","layer":target,"kind":"invert"});
        assert!(prepare(capture(&state, &[command]), &session, &|| Ok(())).is_err());
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
    for count in [31, 32] {
        let mut original = fixture(8);
        let layer = original
            .layers
            .iter_mut()
            .find(|l| l.id == "a_base")
            .unwrap();
        let effect = layer.effects[0].clone();
        layer.effects = (0..count)
            .map(|i| {
                let mut effect = effect.clone();
                effect.id = format!("effect-{i}");
                effect
            })
            .collect();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let staged =
            CapturedScene::from_parts(original.clone(), "effect-limit".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let command = json!({"op":"effect.add","layer":"a_base","kind":"invert"});
        let result = prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()));
        assert_eq!(result.is_ok(), count == 31);
        assert_eq!(
            Engine::preview_edits(original, &[command]).is_ok(),
            count == 31
        );
        if let Ok(preview) = result {
            assert_eq!(
                preview
                    .preview()
                    .metadata()
                    .layers()
                    .find(|l| l.id() == "a_base")
                    .unwrap()
                    .effects()
                    .len(),
                32
            );
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
        }
        let command = json!({"op":"effect.add","layer":"outside","kind":"pattern_overlay","settings":{"pattern":"bitmap","bitmap_png":pattern(16)}});
        assert!(prepare(capture(&state, &[command]), &session, &|| Ok(())).is_err());
    }
    drop((session, runtime, connection));
}
