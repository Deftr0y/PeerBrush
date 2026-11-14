use super::*;
use crate::{
    engine::{
        authority::DocumentAuthority,
        native::worker::{
            self,
            tests::{capture, Fixture},
        },
        scene::{CapturedScene, LegacyTileCache},
        Document, Mask, MaskStep,
    },
    raster::Raster,
};
use serde_json::json;
#[path = "mask_large_tests.rs"]
mod large;

pub(crate) fn compare(
    base: &NativeDocument,
    native: &NativeDocument,
    expected: &Document,
    target: &str,
) {
    for old in base.metadata().layers() {
        let actual = native
            .metadata()
            .layers()
            .find(|layer| layer.id() == old.id())
            .unwrap();
        assert_eq!(
            base.source(old.pixels().slot()).unwrap().key(),
            native.source(actual.pixels().slot()).unwrap().key()
        );
        assert_eq!(json!(old.source()), json!(actual.source()));
    }
    assert_eq!(base.metadata().selection(), native.metadata().selection());
    assert_eq!(
        base.metadata().selection_polygon(),
        native.metadata().selection_polygon()
    );
    for (old, actual) in [
        (
            base.metadata().selection_coverage(),
            native.metadata().selection_coverage(),
        ),
        (
            base.metadata().selection_previous(),
            native.metadata().selection_previous(),
        ),
    ] {
        match (old, actual) {
            (None, None) => {}
            (Some(old), Some(actual)) => {
                assert_eq!(old.bounds(), actual.bounds());
                assert_eq!(old.origin(), actual.origin());
                assert_eq!(old.contours(), actual.contours());
                assert_eq!(
                    base.source(old.mask().slot()).unwrap().key(),
                    native.source(actual.mask().slot()).unwrap().key()
                );
            }
            _ => panic!("Mask edit changed selection coverage"),
        }
    }
    let actual = native
        .metadata()
        .layers()
        .find(|layer| layer.id() == target)
        .unwrap()
        .mask()
        .unwrap();
    let expected = expected
        .layers
        .iter()
        .find(|layer| layer.id == target)
        .unwrap()
        .mask
        .as_ref()
        .unwrap();
    assert_eq!(actual.enabled(), expected.enabled);
    assert_eq!(actual.linked(), expected.linked);
    assert_eq!(actual.steps().len(), expected.steps.len());
    for (index, (step, expected)) in actual.steps().zip(&expected.steps).enumerate() {
        if base
            .metadata()
            .layers()
            .find(|l| l.id() == target)
            .and_then(|l| l.mask())
            .is_some_and(|mask| mask.steps().any(|old| old.id() == expected.id))
        {
            assert_eq!(step.id(), expected.id);
        }
        assert_eq!(step.kind(), expected.kind);
        assert_eq!(step.enabled(), expected.enabled);
        assert_eq!(step.weight(), expected.weight);
        assert_eq!(step.value(), expected.value);
        assert_eq!(step.settings(), &expected.settings);
        let surface = native.source(step.pixels().slot()).unwrap();
        assert_eq!(surface.depth(), expected.pixels.depth);
        for y in 0..surface.height() as i32 {
            let view = surface
                .prepare_region(
                    crate::raster::backed::Region::new([0, y, surface.width() as i32, y + 1])
                        .unwrap(),
                    &mut || Ok(()),
                )
                .unwrap();
            for x in 0..surface.width() as i32 {
                assert_eq!(
                    view.pixel16(x, y).unwrap(),
                    expected.pixels.get16(x, y),
                    "mask step {index}, {x},{y}"
                );
            }
        }
    }
}
fn gate<'a>(leases: &'a [crate::engine::Lease]) -> Publication<'a> {
    Publication {
        actor: "human",
        leases,
        now: 0,
        closed: false,
        write_available: true,
    }
}
#[test]
fn mask_batch_refusals_preserve_sources_selection_and_history() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let mut original = Document::new_depth(4, 3, 16).unwrap();
    let target = original.layers[0].id.clone();
    original.selection = Some([0, 0, 2, 2]);
    original.layers[0].mask = Some(Mask {
        enabled: true,
        linked: true,
        cache_key: crate::engine::id(),
        steps: (0..31)
            .map(|_| MaskStep {
                id: crate::engine::id(),
                kind: "fill".into(),
                enabled: true,
                weight: 1.,
                value: 0.5,
                pixels: Raster::new_depth(1, 1, 16),
                settings: json!(null),
            })
            .collect(),
    });
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(original, "mask-refusal".into(), vec![], None, None)
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let command = json!({"op":"mask.from_selection","layer":target});
    let baseline = runtime.disk().used().unwrap();
    let history = state.history_lengths();
    for (first, reason) in [
        (command.clone(), "32 steps"),
        (json!({"op":"selection.clear"}), "Select an area"),
        (
            json!({"op":"layer.update","layer":target,"locked":true}),
            "locked",
        ),
    ] {
        let failed = worker::prepare(
            capture(&state, &[first, command.clone()]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(
            failed
                .reason
                .to_lowercase()
                .contains(&reason.to_lowercase()),
            "{}",
            failed.reason
        );
        assert!(failed.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), history);
    }
    drop((
        state,
        native,
        staged.seals,
        session,
        cache,
        runtime,
        connection,
    ));
}
#[test]
fn selection_and_path_masks_preserve_steps_precision_selection_and_atomic_history() {
    for depth in [8, 16] {
        for old_mask in [false, true] {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let mut original = Document::new_depth(271, 31, depth).unwrap();
            let target = original.layers[0].id.clone();
            let layer = &mut original.layers[0];
            layer.x = -7;
            layer.y = 3;
            layer.pixels = Raster::new_depth(280, 35, depth);
            layer.pixels.set16(2, 3, [12347, 23459, 34571, 49153]);
            if old_mask {
                let mut paint = Raster::new_depth(280, 35, depth);
                paint.set16(1, 2, [12347, 23459, 34571, 49153]);
                paint.set16(265, 20, [54321, 32109, 10987, 32769]);
                layer.mask = Some(Mask {
                    enabled: false,
                    linked: false,
                    cache_key: crate::engine::id(),
                    steps: vec![
                        MaskStep {
                            id: crate::engine::id(),
                            kind: "fill".into(),
                            enabled: true,
                            weight: 0.87,
                            value: 0.2,
                            pixels: Raster::new_depth(7, 5, depth),
                            settings: json!({}).into(),
                        },
                        MaskStep {
                            id: crate::engine::id(),
                            kind: "paint".into(),
                            enabled: true,
                            weight: 0.61,
                            value: 0.,
                            pixels: paint,
                            settings: json!({}).into(),
                        },
                        MaskStep {
                            id: crate::engine::id(),
                            kind: "blur".into(),
                            enabled: true,
                            weight: 0.73,
                            value: 3.,
                            pixels: Raster::new_depth(1, 1, depth),
                            settings: json!({}).into(),
                        },
                    ],
                });
            }
            crate::path::create(&mut original,&json!({"op":"path.create","closed":true,"frame":[-7,3,273,38],
            "nodes":[{"point":[-2.5,4.5],"out_handle":[75.,34.]},{"point":[268.5,4.5],"in_handle":[200.,31.]},{"point":[180.5,28.5]}]})).unwrap();
            let path = original.layers[0].id.clone();
            let mut coverage = Raster::new(17, 11);
            coverage.set(2, 3, [137, 0, 0, 0]);
            coverage.set(12, 8, [231, 0, 0, 255]);
            let coverage = crate::selection::Coverage {
                mask: coverage,
                bounds: [252, 6, 264, 15],
                origin: [250, 5],
                contours: vec![],
            };
            original.selection = Some(coverage.bounds);
            original.selection_coverage = Some(coverage.clone());
            original.selection_previous = Some(coverage);
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged =
                CapturedScene::from_parts(original.clone(), "mask-base".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(()))
                    .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let session = staged.session.take().unwrap();
            for op in ["mask.from_selection", "path.to_mask"] {
                for mode in ["replace", "add", "subtract", "intersect"] {
                    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                    let command = json!({"op":op,"layer":target,"path":path,"mode":mode,"mask_mode":mode,"feather":2});
                    let expected =
                        crate::engine::Engine::preview_edits(original.clone(), &[command.clone()])
                            .unwrap();
                    let baseline = runtime.disk().used().unwrap();
                    let preview =
                        worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()))
                            .unwrap();
                    compare(&native, preview.preview(), &expected, &target);
                    preview
                        .abort(&session, &|| session.cleanup_check())
                        .unwrap();
                    assert_eq!(runtime.disk().used().unwrap(), baseline);
                    let completed =
                        crate::selection::regional_masks::tests::CompletedTiles::observe();
                    let failed =
                        worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
                            if completed.count() > 0 {
                                Err("mask canceled after tile".into())
                            } else {
                                Ok(())
                            }
                        })
                        .err()
                        .unwrap();
                    assert!(failed.reason.contains("mask canceled after tile"));
                    assert!(failed.cleanup_error.is_none());
                    assert_eq!(completed.count(), 1);
                    assert_eq!(runtime.disk().used().unwrap(), baseline);
                    drop(completed);
                    if mode == "replace" {
                        let key = state.native().unwrap().key();
                        let history = state.history_lengths();
                        let mut invalid = command.clone();
                        invalid["mode"] = json!("invalid");
                        invalid["mask_mode"] = json!("invalid");
                        let failed = worker::prepare(
                            capture(&state, &[command.clone(), invalid]),
                            &session,
                            &|| Ok(()),
                        )
                        .err()
                        .unwrap();
                        assert!(failed.reason.contains("Mask mode"));
                        assert!(failed.cleanup_error.is_none());
                        assert_eq!(runtime.disk().used().unwrap(), baseline);
                        assert_eq!(state.native().unwrap().key(), key);
                        assert_eq!(state.history_lengths(), history);
                    }
                    if mode == "subtract" {
                        for held in [
                            target.as_str(),
                            if op == "path.to_mask" {
                                path.as_str()
                            } else {
                                "@selection"
                            },
                        ] {
                            let prepared = worker::prepare(
                                capture(&state, &[command.clone()]),
                                &session,
                                &|| Ok(()),
                            )
                            .unwrap();
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
                                owner: "another-human".into(),
                                description: "Mask".into(),
                                scopes: vec![crate::engine::Scope::layer(held)],
                                expires: 100,
                            }];
                            let refused = state.commit_native(gate(&leases), commit).unwrap_err();
                            assert!(refused.reason().contains("Reserved"));
                            refused
                                .abort(&session, &|| session.cleanup_check())
                                .unwrap();
                            assert_eq!(runtime.disk().used().unwrap(), baseline);
                        }
                        let prepared =
                            worker::prepare(capture(&state, &[command]), &session, &|| Ok(()))
                                .unwrap();
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
                        state
                            .commit_native(gate(&[]), commit)
                            .unwrap()
                            .retire(&session, &|| session.cleanup_check())
                            .unwrap();
                        let moved = state
                            .native_history()
                            .unwrap()
                            .prepare_move(
                                state.native().unwrap(),
                                Direction::Undo,
                                "human",
                                &|| Ok(()),
                            )
                            .unwrap();
                        drop(state.move_native(gate(&[]), moved).unwrap());
                        let restored = json!(state.native().unwrap().metadata());
                        let mut before = json!(native.metadata());
                        assert_eq!(
                            state.native().unwrap().metadata().revision(),
                            native.metadata().revision() + 2
                        );
                        before["revision"] = restored["revision"].clone();
                        assert_eq!(restored, before);
                        let moved = state
                            .native_history()
                            .unwrap()
                            .prepare_move(
                                state.native().unwrap(),
                                Direction::Redo,
                                "human",
                                &|| Ok(()),
                            )
                            .unwrap();
                        drop(state.move_native(gate(&[]), moved).unwrap());
                        compare(&native, state.native().unwrap(), &expected, &target);
                    }
                }
            }
            drop((native, staged.seals, session, cache, runtime, connection));
        }
    }
}

#[test]
fn native_mask_refinement_uses_rendered_guides_and_retires_them_on_success_or_failure() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
        let mut original = Document::new_depth(273, 31, depth).unwrap();
        original.icc_profile = Some(Arc::new(
            moxcms::ColorProfile::new_display_p3().encode().unwrap(),
        ));
        original.selection = Some([3, 4, 23, 19]);
        let layer = &mut original.layers[0];
        let target = layer.id.clone();
        layer.x = 3;
        layer.y = 3;
        layer.pixels = Raster::new_depth(267, 25, depth);
        let mut mask = Raster::new_depth(267, 25, depth);
        for y in 0..25 {
            for x in 0..267 {
                let value = ((x * 137 + y * 3157) % 65536) as u16;
                layer.pixels.set16(
                    x,
                    y,
                    [
                        value,
                        65535 - value,
                        23459,
                        if (x + y) % 5 == 0 { 19321 } else { 65535 },
                    ],
                );
                mask.set16(x, y, [value, 0, 0, 49153]);
            }
        }
        layer.mask = Some(Mask {
            enabled: false,
            linked: false,
            cache_key: crate::engine::id(),
            steps: vec![MaskStep {
                id: crate::engine::id(),
                kind: "paint".into(),
                enabled: true,
                weight: 0.71,
                value: 0.,
                pixels: mask,
                settings: json!(null),
            }],
        });
        layer.effects.push(crate::effects::Effect {
            id: crate::engine::id(),
            kind: "drop_shadow".into(),
            enabled: true,
            weight: 0.63,
            settings: crate::effects::defaults("drop_shadow").into(),
        });
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "mask-refine".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let baseline = runtime.disk().used().unwrap();
        let history = state.history_lengths();
        for guide_target in [None, Some(target.as_str())] {
            let expected = original
                .preview(Some([3, 3, 270, 28]), 267, guide_target, false)
                .unwrap();
            let guide = crate::render::native_preview::stage_guide(
                &native,
                &session,
                [3, 3, 270, 28],
                guide_target,
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(guide.surface().depth(), 8);
            assert_eq!(
                [guide.surface().width(), guide.surface().height()],
                [expected.0, expected.1]
            );
            for y in 0..expected.1 as i32 {
                let view = guide
                    .surface()
                    .prepare_region(
                        crate::raster::backed::Region::new([0, y, expected.0 as i32, y + 1])
                            .unwrap(),
                        &mut || Ok(()),
                    )
                    .unwrap();
                for x in 0..expected.0 as i32 {
                    let at = ((y as u32 * expected.0 + x as u32) * 4) as usize;
                    assert_eq!(
                        view.pixel(x, y).unwrap().as_slice(),
                        &expected.2[at..at + 4]
                    );
                }
            }
            guide.abort(&session, &|| session.cleanup_check()).unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
        for edge in [0., 47.] {
            let command = json!({"op":"mask.refine","layer":target,"smooth":2,"feather":3,"shift":-1,"contrast":21,"edge":edge,"edge_radius":3});
            let expected =
                crate::engine::Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            let prepared =
                worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            compare(&native, prepared.preview(), &expected, &target);
            prepared
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            let completed = crate::selection::regional_masks::tests::CompletedTiles::observe();
            let failed = worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
                if completed.count() > 0 {
                    Err("refinement canceled after tile".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert!(failed.reason.contains("refinement canceled after tile"));
            assert!(failed.cleanup_error.is_none());
            assert_eq!(completed.count(), 1);
            drop(completed);
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            // Invalid settings fail after the guide was prepared; both provisional sources retire.
            let mut invalid = command.clone();
            invalid["smooth"] = json!(33);
            let failed = worker::prepare(capture(&state, &[invalid]), &session, &|| Ok(()))
                .err()
                .unwrap();
            assert!(failed.reason.contains("smooth"));
            assert!(failed.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), history);
        }
        assert!(crate::render::native_preview::stage_guide(
            &native,
            &session,
            [-1, 3, 266, 28],
            Some(&target),
            &|| Ok(())
        )
        .is_err());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        drop((
            state,
            native,
            staged.seals,
            session,
            cache,
            runtime,
            connection,
        ));
    }
}
