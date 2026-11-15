use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    native::worker::{
        self,
        tests::{capture, Fixture},
    },
    scene::{CapturedScene, LegacyTileCache},
    Document,
};
use serde_json::json;
#[path = "selection_import_tests.rs"]
mod imports;
#[path = "selection_sampling_tests.rs"]
mod sampling;
#[path = "selection_large_tests.rs"]
mod large;
#[path = "selection_pixel_large_tests.rs"]
mod pixel_large;

fn coverage(
    native: &NativeDocument,
    actual: Option<CoverageMetadata<'_>>,
    expected: Option<&crate::selection::Coverage>,
) {
    let (actual, expected) = match (actual, expected) {
        (None, None) => return,
        (Some(a), Some(e)) => (a, e),
        _ => panic!("selection coverage presence differs"),
    };
    assert_eq!(actual.bounds(), expected.bounds);
    let surface = native.source(actual.mask().slot()).unwrap();
    assert_eq!(surface.depth(), 8);
    let [ax, ay] = actual.origin();
    let [ex, ey] = expected.origin;
    let left = ax.min(ex);
    let right = (ax + surface.width() as i32).max(ex + expected.mask.width as i32);
    let bottom = (ay + surface.height() as i32).max(ey + expected.mask.height as i32);
    for y in ay.min(ey)..bottom {
        let view = surface
            .prepare_region(
                crate::raster::backed::Region::new([left - ax, y - ay, right - ax, y - ay + 1]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        for x in left..right {
            assert_eq!(
                view.pixel(x - ax, y - ay).unwrap(),
                expected.mask.get(x - ex, y - ey),
                "selection at {x},{y}"
            );
        }
    }
    // Contour starting vertices/order are unspecified; their even-odd coverage is not.
    for y in 0..native.metadata().height() {
        for x in 0..native.metadata().width() {
            let inside = |lines: &[Vec<[f32; 2]>]| {
                lines
                    .iter()
                    .filter(|line| crate::selection::contains(line, x as f32 + 0.5, y as f32 + 0.5))
                    .count()
                    % 2
            };
            assert_eq!(
                inside(actual.contours()),
                inside(&expected.contours),
                "contours at {x},{y}"
            );
        }
    }
}
fn compare(native: &NativeDocument, expected: &Document) {
    let metadata = native.metadata();
    assert_eq!(metadata.selection(), expected.selection);
    assert_eq!(
        metadata.selection_polygon(),
        expected.selection_polygon.as_deref()
    );
    coverage(
        native,
        metadata.selection_coverage(),
        expected.selection_coverage.as_ref(),
    );
    coverage(
        native,
        metadata.selection_previous(),
        expected.selection_previous.as_ref(),
    );
}
fn publication<'a>(leases: &'a [crate::engine::Lease]) -> Publication<'a> {
    Publication {
        actor: "human",
        leases,
        now: 0,
        closed: false,
        write_available: true,
    }
}
fn commit(
    state: &mut DocumentAuthority,
    prepared: worker::PreparedNative,
    session: &SourceSession,
) {
    let (base, history, delta) = prepared.into_parts();
    let committed = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Selection",
                task: None,
                gesture: None,
            },
            &|| Ok(()),
        )
        .unwrap();
    state
        .commit_native(publication(&[]), committed)
        .unwrap()
        .retire(session, &|| session.cleanup_check())
        .unwrap();
}

#[test]
fn geometry_and_history_match_resident_coverage_with_no_selection_geometry_or_offset_pixels() {
    for (depth, prior) in [(8, 0), (8, 1), (8, 2), (16, 0), (16, 1), (16, 2)] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut oracle = Document::new_depth(269, 19, depth).unwrap();
        oracle.layers[0]
            .pixels
            .set16(3, 4, [12347, 23459, 34571, 49153]);
        if prior == 1 {
            let polygon = vec![[1.5, 0.5], [267.5, 1.5], [19.5, 17.5]];
            oracle.selection = Some(crate::selection::bounds(&polygon).unwrap());
            oracle.selection_polygon = Some(polygon);
        } else if prior == 2 {
            let mut mask = crate::raster::Raster::new(9, 7);
            mask.set(4, 3, [137, 0, 0, 0]);
            mask.set(6, 5, [231, 17, 19, 255]);
            let value = crate::selection::Coverage {
                mask,
                bounds: [50, 1, 54, 5],
                origin: [-2, 4],
                contours: vec![],
            };
            oracle.selection = Some(value.bounds);
            oracle.selection_coverage = Some(value);
        }
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(oracle.clone(), "selection-base".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let mut state = DocumentAuthority::from_native(
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap(),
            2,
        )
        .unwrap();
        let session = staged.session.take().unwrap();
        let color_key = state
            .native()
            .unwrap()
            .source(
                state
                    .native()
                    .unwrap()
                    .metadata()
                    .layer(0)
                    .unwrap()
                    .pixels()
                    .slot(),
            )
            .unwrap()
            .key();
        for command in [
            json!({"op":"selection","kind":"rectangle","rect":[3,2,16,15],"mode":"intersect","feather":2.}),
            json!({"op":"selection","kind":"rectangle","rect":[8,2,16,8],"mode":"add","feather":1.}),
            json!({"op":"selection","kind":"ellipse","rect":[-3,2,8,10],"feather":2.}),
            json!({"op":"selection","polygon":[[257.25,8.25],[273.5,9.5],[259.5,21.5]],"mode":"replace","feather":2.}),
            json!({"op":"selection","kind":"ellipse","rect":[0,2,27,16],"mode":"subtract","feather":1.}),
            json!({"op":"selection","rect":[0,0,269,19]}),
            json!({"op":"selection","kind":"ellipse","rect":[40,4,240,16],"mode":"subtract","feather":1.}),
            json!({"op":"selection.modify","mode":"expand","radius":1.6}),
            json!({"op":"selection.modify","mode":"contract","radius":3,"feather":1.}),
            json!({"op":"selection","polygon":[[0.5,0.5],[268.5,0.5],[200.5,18.5],[10.5,18.5]],"mode":"intersect","feather":2.}),
            json!({"op":"selection.clear"}),
            json!({"op":"selection.reselect"}),
            json!({"op":"selection.clear"}),
            json!({"op":"selection.clear"}),
            json!({"op":"selection.reselect"}),
            json!({"op":"selection","rect":[-9,3,12,21]}),
            json!({"op":"selection","kind":"rectangle","rect":[11,1,16,21],"mode":"subtract","feather":2.}),
            json!({"op":"selection","rect":[-9,3,12,21]}),
            json!({"op":"selection.clear"}),
            json!({"op":"selection.reselect"}),
            json!({"op":"selection","rect":[-9,-5,-1,6]}),
            json!({"op":"selection.clear"}),
            json!({"op":"selection.reselect"}),
            json!({"op":"selection.invert","feather":2.}),
            json!({"op":"selection","rect":[12,3,20,14],"mode":"add","feather":3.}),
            json!({"op":"selection","polygon":[[1.5,0.5],[267.5,1.5],[19.5,17.5]]}),
            json!({"op":"selection","rect":[10,10,10,13]}),
            json!({"op":"selection","kind":"rectangle","rect":[10,10,10,13]}),
            json!({"op":"selection.modify","mode":"expand","radius":"default"}),
            json!({"op":"selection.modify","mode":"contract","radius":64}),
        ] {
            let previous = oracle.clone();
            oracle = crate::engine::Engine::preview_edits(oracle, &[command.clone()]).unwrap();
            let key = state.native().unwrap().key();
            let baseline = runtime.disk().used().unwrap();
            let lengths = state.history_lengths();
            let preview =
                worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            compare(preview.preview(), &oracle);
            assert_eq!(state.native().unwrap().key(), key);
            assert_eq!(state.history_lengths(), lengths);
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            let prepared =
                worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            commit(&mut state, prepared, &session);
            for (direction, expected) in [(Direction::Undo, &previous), (Direction::Redo, &oracle)]
            {
                let moved = state
                    .native_history()
                    .unwrap()
                    .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                    .unwrap();
                drop(state.move_native(publication(&[]), moved).unwrap());
                compare(state.native().unwrap(), expected);
            }
            let document = state.native().unwrap();
            assert_eq!(
                document
                    .source(document.metadata().layer(0).unwrap().pixels().slot())
                    .unwrap()
                    .key(),
                color_key
            );
            assert_eq!(document.metadata().depth(), depth);
        }
        drop((state, staged.seals, session, cache, runtime, connection));
    }
}

#[test]
fn maximum_radius_modify_splits_native_read_halos_and_matches_public_engine() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let mut original = Document::new_depth(771, 773, 16).unwrap();
    original.selection = Some([63, 67, 705, 705]);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "morphology-base".into(),
        vec![],
        None,
        None,
    )
    .stage(&runtime, &cache, &|| Ok(()))
    .unwrap();
    let state = DocumentAuthority::from_native(
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap(),
        4,
    )
    .unwrap();
    let session = staged.session.take().unwrap();
    let baseline = runtime.disk().used().unwrap();
    for mode in ["expand", "contract"] {
        let command = json!({"op":"selection.modify","radius":64,"mode":mode,"feather":1});
        let expected =
            crate::engine::Engine::preview_edits(original.clone(), &[command.clone()]).unwrap();
        let preview =
            worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
        compare(preview.preview(), &expected);
        preview
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let completed = crate::selection::CompletedTiles::observe();
        let failure = worker::prepare(capture(&state, &[command]), &session, &|| {
            if completed.count() > 0 {
                Err("morphology canceled after tile".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(failure.reason.contains("morphology canceled after tile"));
        assert!(failure.cleanup_error.is_none());
        assert_eq!(completed.count(), 1);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.history_lengths(), [0, 0]);
    }
    drop((state, staged.seals, session, cache, runtime, connection));
}

#[test]
fn refinement_preserves_native_artwork_noop_geometry_and_atomic_selection_history() {
    for depth in [8, 16] {
        for soft in [false, true] {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
            let mut original = Document::new_depth(269, 19, depth).unwrap();
            original.icc_profile = Some(Arc::new(
                moxcms::ColorProfile::new_display_p3().encode().unwrap(),
            ));
            let target = original.layers[0].id.clone();
            for y in 0..19 {
                for x in 0..269 {
                    let value = ((x * 137 + y * 2197) % 65536) as u16;
                    original.layers[0]
                        .pixels
                        .set16(x, y, [value, 65535 - value, 23459, 49153]);
                }
            }
            original.filters.push(crate::effects::Effect {
                id: crate::engine::id(),
                kind: "grayscale".into(),
                enabled: true,
                weight: 0.71,
                settings: crate::effects::defaults("grayscale").into(),
            });
            let polygon = vec![[1.5, 0.5], [267.5, 1.5], [19.5, 17.5]];
            original.selection = Some(crate::selection::bounds(&polygon).unwrap());
            original.selection_polygon = Some(polygon);
            if soft {
                let mut mask = crate::raster::Raster::new(17, 13);
                mask.set(4, 3, [137, 0, 0, 0]);
                mask.set(12, 11, [231, 17, 19, 255]);
                let value = crate::selection::Coverage {
                    mask,
                    bounds: [252, 1, 265, 15],
                    origin: [250, -2],
                    contours: vec![],
                };
                original.selection = Some(value.bounds);
                original.selection_coverage = Some(value);
            }
            original.selection_previous = crate::selection::current(&Document {
                selection: Some([2, 2, 7, 5]),
                ..original.clone()
            });
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "selection-refine".into(),
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
            let color = native
                .source(native.metadata().layer(0).unwrap().pixels().slot())
                .unwrap()
                .key();
            // Nonzero contrast leaves binary geometry unchanged, despite newly rendered tiles.
            for command in [
                json!({"op":"selection.refine"}),
                json!({"op":"selection.refine","contrast":if soft {0} else {17}}),
            ] {
                let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                let baseline = runtime.disk().used().unwrap();
                let prepared =
                    worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                compare(prepared.preview(), &original);
                let mut expected = json!(native.metadata());
                let actual = json!(prepared.preview().metadata());
                expected["revision"] = actual["revision"].clone();
                assert_eq!(actual, expected);
                assert_eq!(prepared.preview().roster().len(), native.roster().len());
                prepared
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), baseline);
            }
            for merged in [None, Some(true), Some(false)] {
                let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                let mut command = json!({"op":"selection.refine","layer":target,"smooth":2,"feather":3,"shift":1,"contrast":17,"edge":47,"edge_radius":3});
                if let Some(merged) = merged {
                    command["sample_merged"] = json!(merged);
                }
                let expected =
                    crate::engine::Engine::preview_edits(original.clone(), &[command.clone()])
                        .unwrap();
                let baseline = runtime.disk().used().unwrap();
                let preview =
                    worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()))
                        .unwrap();
                compare(preview.preview(), &expected);
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
                    color
                );
                preview
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                let completed = crate::selection::CompletedTiles::observe();
                let failed =
                    worker::prepare(capture(&state, &[command.clone()]), &session, &|| {
                        if completed.count() > 0 {
                            Err("selection refine canceled".into())
                        } else {
                            Ok(())
                        }
                    })
                    .err()
                    .unwrap();
                assert!(failed.reason.contains("selection refine canceled"));
                assert!(failed.cleanup_error.is_none());
                assert_eq!(completed.count(), 1);
                drop(completed);
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                let mut invalid = command.clone();
                invalid["smooth"] = json!(33);
                let failed = worker::prepare(
                    capture(&state, &[command.clone(), invalid]),
                    &session,
                    &|| Ok(()),
                )
                .err()
                .unwrap();
                assert!(failed.reason.contains("smooth"));
                assert!(failed.cleanup_error.is_none());
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                let prepared =
                    worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()))
                        .unwrap();
                let (base, history, delta) = prepared.into_parts();
                let committed = history
                    .prepare_commit(
                        &base,
                        delta,
                        HistoryRequest {
                            actor: "human",
                            label: "Refine",
                            task: None,
                            gesture: None,
                        },
                        &|| Ok(()),
                    )
                    .unwrap();
                let leases = [crate::engine::Lease {
                    id: "late".into(),
                    owner: "other".into(),
                    description: "Selection".into(),
                    scopes: vec![crate::engine::Scope::layer("@selection")],
                    expires: 100,
                }];
                let refused = state
                    .commit_native(publication(&leases), committed)
                    .unwrap_err();
                assert!(refused.reason().contains("Reserved"));
                refused
                    .abort(&session, &|| session.cleanup_check())
                    .unwrap();
                assert_eq!(runtime.disk().used().unwrap(), baseline);
                assert_eq!(state.native().unwrap().key(), native.key());
                assert_eq!(state.history_lengths(), [0, 0]);
                let prepared =
                    worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
                commit(&mut state, prepared, &session);
                for (direction, expected) in
                    [(Direction::Undo, &original), (Direction::Redo, &expected)]
                {
                    let moved = state
                        .native_history()
                        .unwrap()
                        .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                        .unwrap();
                    drop(state.move_native(publication(&[]), moved).unwrap());
                    compare(state.native().unwrap(), expected);
                }
            }
            let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let baseline = runtime.disk().used().unwrap();
            let failed = worker::prepare(
                capture(
                    &state,
                    &[
                        json!({"op":"selection.clear"}),
                        json!({"op":"selection.refine"}),
                    ],
                ),
                &session,
                &|| Ok(()),
            )
            .err()
            .unwrap();
            assert!(failed.reason.contains("Select an area"));
            assert!(failed.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert_eq!(state.native().unwrap().key(), native.key());
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
}

#[test]
fn closed_path_conversion_preserves_native_source_and_respects_late_reservations_and_cancellation()
{
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut oracle = Document::new_depth(269, 19, depth).unwrap();
        crate::path::create(&mut oracle,&json!({"op":"path.create","frame":[-3,2,277,30],"closed":true,
            "nodes":[{"point":[1.5,3.5],"out_handle":[45.,16.]},{"point":[260.5,3.5],"in_handle":[210.,17.]},{"point":[160.5,17.5]}]})).unwrap();
        Arc::make_mut(oracle.layers[0].source.as_mut().unwrap()).matrix =
            [-0.875, 0.015625, 0.0625, 0.875, 240.25, -1.625];
        let path_id = oracle.layers[0].id.clone();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged = CapturedScene::from_parts(
            oracle.clone(),
            "path-selection-base".into(),
            vec![],
            None,
            None,
        )
        .stage(&runtime, &cache, &|| Ok(()))
        .unwrap();
        let mut state = DocumentAuthority::from_native(
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap(),
            4,
        )
        .unwrap();
        let session = staged.session.take().unwrap();
        let key = state.native().unwrap().key();
        let baseline = runtime.disk().used().unwrap();
        let command = json!({"op":"path.to_selection","path":path_id,"feather":1.});
        crate::path::to_selection(&mut oracle, &command).unwrap();
        for target in [&path_id, "@selection"] {
            let prepared =
                worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(())).unwrap();
            compare(prepared.preview(), &oracle);
            assert_eq!(
                json!(prepared.preview().metadata().layer(0).unwrap().source()),
                json!(state
                    .native()
                    .unwrap()
                    .metadata()
                    .layer(0)
                    .unwrap()
                    .source())
            );
            let (base, history, delta) = prepared.into_parts();
            let committed = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Path selection",
                        task: None,
                        gesture: None,
                    },
                    &|| Ok(()),
                )
                .unwrap();
            let leases = [crate::engine::Lease {
                id: "late".into(),
                owner: "another-human".into(),
                description: "Path selection".into(),
                scopes: vec![crate::engine::Scope::layer(target)],
                expires: 100,
            }];
            let refused = state
                .commit_native(publication(&leases), committed)
                .unwrap_err();
            assert!(refused.reason().contains("Reserved"));
            refused
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert_eq!(state.native().unwrap().key(), key);
        }
        for invalid in [
            json!({"op":"selection","polygon":[[0,0],[1,1]]}),
            json!({"op":"selection","rect":[0,0,2,2],"mode":"bad"}),
            json!({"op":"path.to_selection","path":"missing"}),
            json!({"op":"selection","kind":"not-a-kind"}),
        ] {
            let failure = worker::prepare(
                capture(&state, &[command.clone(), invalid]),
                &session,
                &|| Ok(()),
            )
            .err()
            .unwrap();
            assert!(failure.cleanup_error.is_none());
            assert_eq!(runtime.disk().used().unwrap(), baseline);
            assert_eq!(state.native().unwrap().key(), key);
        }
        let completed = crate::selection::CompletedTiles::observe();
        let failure = worker::prepare(capture(&state, &[command]), &session, &|| {
            if completed.count() > 0 {
                Err("selection canceled after tile".into())
            } else {
                Ok(())
            }
        })
        .err()
        .unwrap();
        assert!(failure.reason.contains("selection canceled after tile"));
        assert!(failure.cleanup_error.is_none());
        assert_eq!(completed.count(), 1);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), key);
        assert_eq!(state.history_lengths(), [0, 0]);
        drop((state, staged.seals, session, cache, runtime, connection));
    }
}
