use super::effect_create_tests::compare_raster;
use super::grouping_tests::{compare, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Document, Engine,
};
use serde_json::json;

fn document(depth: u16) -> Document {
    let mut doc = Document::new_depth(270, 8, depth).unwrap();
    let layer = &mut doc.layers[0];
    layer.id = "base".into();
    layer.x = -2;
    layer.y = -1;
    layer.pixels = crate::raster::Raster::new_depth(272, 10, depth);
    for y in 0..10 {
        for x in 0..272 {
            layer.pixels.set16(x, y, [12347, 23459, 34571, 49153]);
        }
    }
    layer.pixels.retained = Some(Box::new(crate::retained::Original::from(&layer.pixels)));
    Engine::preview_edits(doc, &[json!({"op":"mask.add","layer":"base","value":137})]).unwrap()
}

#[test]
fn primitive_pixels_masks_previews_and_history_match_resident_at_both_depths() {
    for depth in [8, 16] {
        for selection in [
            json!({"op":"selection.clear"}),
            json!({"op":"selection","rect":[250,1,268,7]}),
            json!({"op":"selection","kind":"ellipse","rect":[250,1,268,7],"feather":1}),
            json!({"op":"selection","kind":"polygon","polygon":[[250.,1.],[268.,2.],[254.,7.]]}),
        ] {
            let root = Fixture::new();
            let (runtime, connection) =
                root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
            let original = Engine::preview_edits(document(depth), &[selection]).unwrap();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "primitives".into(),
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
                json!({"op":"shape","layer":"base","rect":[248,0,270,8],"color":[211,73,99,173]}),
                json!({"op":"shape","kind":"ellipse","layer":"base","rect":[248,0,270,8],"color":[211,73,99,173]}),
                json!({"op":"gradient","layer":"base","rect":[248,0,270,8],"color":[211,73,99,173]}),
                json!({"op":"gradient","layer":"base","rect":[248,-3,270,8],"direction":"left","color":[211,73,99,173]}),
                json!({"op":"gradient","layer":"base","rect":[248,-3,270,8],"direction":"down","color":[211,73,99,173]}),
                json!({"op":"gradient","layer":"base","mask":true,"rect":[248,-3,270,8],"direction":"up","color":[83,191,211,173]}),
                json!({"op":"shape","kind":"ellipse","layer":"base","mask":true,"rect":[248,0,270,8],"color":[83,191,211,173]}),
                json!({"op":"gradient","layer":"base","mask":true,"rect":[248,0,270,8],"color":[83,191,211,173]}),
                json!({"op":"gradient","layer":"base"}),
                json!({"op":"shape","layer":"base","rect":[2,2,2,2]}),
                json!({"op":"shape","layer":"base","rect":[248,0,270,8],"color":[2,3,4,0]}),
                json!({"op":"gradient","layer":"base","rect":[300,0,320,8]}),
                json!({"op":"adjust","layer":"base","rect":[248,0,270,8],"brightness":0.13,"contrast":1.4,"saturation":0.73}),
                json!({"op":"adjust","layer":"base","brightness":-0.2,"contrast":0.9,"saturation":2.1}),
                json!({"op":"adjust","layer":"base"}),
                json!({"op":"adjust","layer":"base","rect":[2,2,2,2],"contrast":0}),
                json!({"op":"adjust","layer":"base","rect":[300,0,320,8],"contrast":0}),
            ] {
                let disk = runtime.disk().used().unwrap();
                let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
                let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                let preview =
                    prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
                compare(preview.preview(), &expected);
                let layer = preview.preview().metadata().layer(0).unwrap();
                compare_raster(
                    preview.preview(),
                    layer.pixels(),
                    &expected.layers[0].pixels,
                );
                for (step, expected) in layer
                    .mask()
                    .unwrap()
                    .steps()
                    .zip(&expected.layers[0].mask.as_ref().unwrap().steps)
                {
                    compare_raster(preview.preview(), step.pixels(), &expected.pixels);
                }
                if command["mask"] == true {
                    assert_eq!(
                        preview
                            .preview()
                            .source(layer.pixels().slot())
                            .unwrap()
                            .key(),
                        native
                            .source(native.metadata().layer(0).unwrap().pixels().slot())
                            .unwrap()
                            .key()
                    );
                }
                let scene = Arc::new(
                    SceneSnapshot::from_native(
                        SceneIdentity {
                            project_id: "primitive-preview".into(),
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
                    scene, read, None, 270, None, false,
                )
                .unwrap();
                let frame = request.render(&mut || Ok(())).unwrap();
                assert_eq!(
                    &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                    expected
                        .preview(None, 270, None, false)
                        .unwrap()
                        .2
                        .as_slice()
                );
                drop((frame, request));
                preview
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), disk);
                assert_eq!(state.history_lengths(), [0, 0]);
                if !(command["op"] == "gradient" && command["mask"] == true
                    || command["op"] == "adjust" && command.get("brightness").is_some())
                {
                    continue;
                }
                let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                let (base, history, delta) = prepared.into_parts();
                let commit = history
                    .prepare_commit(
                        &base,
                        delta,
                        HistoryRequest {
                            actor: "human",
                            label: "Raster edit",
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
                    let layer = state.native().unwrap().metadata().layer(0).unwrap();
                    for (step, expected) in layer
                        .mask()
                        .unwrap()
                        .steps()
                        .zip(&expected.layers[0].mask.as_ref().unwrap().steps)
                    {
                        compare_raster(state.native().unwrap(), step.pixels(), &expected.pixels);
                    }
                }
            }
            drop((native, session, staged.seals, cache, runtime, connection));
        }
    }
}

#[test]
fn refused_primitives_preserve_sources_reservations_and_history() {
    for op in ["gradient", "adjust"] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let original = document(16);
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original, "primitive-refusal".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        let valid = json!({"op":op,"layer":"base","rect":[248,0,270,8],"color":[211,73,99,173],"brightness":0.21});
        for invalid in [
            json!({"op":"shape","layer":"missing"}),
            json!({"op":"shape","layer":"base","mask":true,"step":"missing"}),
            json!({"op":"gradient","layer":"base","mask":"true"}),
            json!({"op":"gradient","layer":"base","direction":"diagonal"}),
            json!({"op":"adjust","layer":"base","mask":true}),
            json!({"op":"adjust","layer":"base","rect":["invalid",0,2,2]}),
        ] {
            let failed = prepare(
                capture(&state, &[valid.clone(), invalid]),
                &session,
                &|| Ok(()),
            )
            .err()
            .unwrap();
            assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        assert!(prepare(
            capture(
                &state,
                &[
                    json!({"op":"layer.update","layer":"base","locked":true}),
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
                    label: "Gradient",
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
            scopes: vec![crate::engine::Scope::layer("base")],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        let cancelled = capture(&state, &[valid.clone()]);
        cancelled.cancellation().cancel();
        assert!(prepare(cancelled, &session, &|| Ok(())).is_err());
        let failed = prepare(capture(&state, &[valid.clone()]), &session, &|| {
            if runtime.disk().used().unwrap() > disk {
                Err("Interrupted primitive".into())
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
}

#[test]
fn primitive_empty_coverage_polygon_intersection_and_expansion_match_resident() {
    for variant in 0..3 {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = document(16);
        if variant == 2 {
            let layer = &mut original.layers[0];
            layer.x = 1;
            layer.y = 1;
            layer.pixels = crate::raster::Raster::new_depth(4, 4, 16);
            layer.mask = None;
        } else {
            let bounds = if variant == 0 { [0; 4] } else { [0, 0, 270, 8] };
            original.selection = Some(bounds);
            let mut mask = crate::raster::Raster::new(270, 8);
            if variant == 1 {
                original.selection_polygon = Some(vec![[0., 0.], [270., 0.], [0., 8.]]);
                for y in 0..8 {
                    for x in 0..270 {
                        mask.set(x, y, [77, 77, 77, 255]);
                    }
                }
            }
            original.selection_coverage = Some(crate::selection::Coverage {
                mask,
                bounds,
                origin: [0; 2],
                contours: vec![],
            });
        }
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            original.clone(),
            "primitive-edges".into(),
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
        for op in ["shape", "gradient", "adjust"] {
            let command = json!({"op":op,"layer":"base","rect":[0,0,270,8],"color":[211,73,99,173],"brightness":0.21});
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(()));
            let preview = prepared.unwrap();
            compare(preview.preview(), &expected);
            compare_raster(
                preview.preview(),
                preview.preview().metadata().layer(0).unwrap().pixels(),
                &expected.layers[0].pixels,
            );
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(state.native().unwrap().key(), native.key());
            assert_eq!(state.history_lengths(), [0, 0]);
        }
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
fn native_adjust_keeps_sparse_holes_and_default_originals_with_off_canvas_color() {
    for depth in [8, 16] {
        let root = Fixture::new();
        let (runtime, connection) = root.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = document(depth);
        let layer = &mut original.layers[0];
        layer.pixels = crate::raster::Raster::new_depth(780, 10, depth);
        layer.pixels.set16(1, 1, [12347, 23459, 34571, 49153]);
        layer.pixels.set16(779, 1, [7777, 17777, 27777, 0]);
        layer.pixels.retained = Some(Box::new(crate::retained::Original::from(&layer.pixels)));
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "sparse-adjust".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        for command in [
            json!({"op":"adjust","layer":"base"}),
            json!({"op":"adjust","layer":"base","contrast":0}),
        ] {
            let expected = Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
            let preview =
                prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            compare(preview.preview(), &expected);
            let pixels = preview.preview().metadata().layer(0).unwrap().pixels();
            compare_raster(preview.preview(), pixels, &expected.layers[0].pixels);
            let surface = preview.preview().source(pixels.slot()).unwrap();
            assert!(surface
                .physical_tiles()
                .all(|tile| tile.coordinate().0 != 1 && tile.coordinate().0 != 2));
            if command.get("contrast").is_none() {
                assert_eq!(
                    surface.key(),
                    native
                        .source(native.metadata().layer(0).unwrap().pixels().slot())
                        .unwrap()
                        .key()
                );
                assert!(pixels.retained().is_some());
            } else {
                assert!(pixels.retained().is_none());
            }
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
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
