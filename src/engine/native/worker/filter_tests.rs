use super::grouping_tests::{compare, document, gate};
use super::tests::{capture, Fixture};
use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache, SceneIdentity, SceneSnapshot},
    Engine,
};
use serde_json::json;

#[test]
fn filters_share_native_artwork_and_match_resident_rendering_and_history() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
        let mut original = Engine::preview_edits(
            document(depth),
            &[
                json!({"op":"filter.add","kind":"invert","weight":0.3}),
                json!({"op":"filter.add","kind":"blur","settings":{"radius":1.25}}),
            ],
        )
        .unwrap();
        original.filters[0].id = "invert".into();
        original.filters[1].id = "blur".into();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "filter-test".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let disk = runtime.disk().used().unwrap();
        let unchanged = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        for command in [
            json!({"op":"filter.update","filter":"blur","enabled":"yes"}),
            json!({"op":"filter.update","filter":"blur","kind":null}),
            json!({"op":"filter.update","filter":"blur","kind":"liquify"}),
            json!({"op":"filter.update","filter":"blur","weight":-0.1}),
            json!({"op":"filter.update","filter":"blur","settings":null}),
            json!({"op":"filter.reorder","filter":"blur","index":2}),
            json!({"op":"filter.reorder","filter":"blur","index":-1}),
        ] {
            let commands = [json!({"op":"filter.delete","filter":"invert"}), command];
            assert!(Engine::preview_edits(original.clone(), &commands).is_err());
            let failed = prepare(capture(&unchanged, &commands), &session, &|| Ok(()))
                .err()
                .unwrap();
            assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
            assert_eq!(unchanged.native().unwrap().key(), native.key());
            assert_eq!(unchanged.history_lengths(), [0, 0]);
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop(unchanged);
        for commands in [
            vec![
                json!({"op":"filter.add","kind":"posterize","weight":0.6,"settings":{"levels":5}}),
            ],
            vec![
                json!({"op":"filter.update","filter":"invert","kind":"bloom","settings":{"spread":1.5,"strength":0.4,"threshold":0.2}}),
            ],
            vec![json!({"op":"filter.reorder","filter":"blur","index":0})],
            vec![
                json!({"op":"filter.update","filter":"blur","weight":0.25}),
                json!({"op":"filter.update","filter":"invert","enabled":false}),
            ],
            vec![
                json!({"op":"filter.update","filter":"blur","kind":"grayscale"}),
                json!({"op":"filter.delete","filter":"invert"}),
            ],
        ] {
            let mut expected = Engine::preview_edits(original.clone(), &commands).unwrap();
            expected.revision = original.revision + 1;
            let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            // Only an added filter receives a fresh ID in either implementation.
            for (expected, actual) in expected
                .filters
                .iter_mut()
                .zip(preview.preview().metadata().filters())
            {
                if original.filters.iter().any(|old| old.id == expected.id) {
                    assert_eq!(expected.id, actual.id());
                } else {
                    assert!(!original.filters.iter().any(|old| old.id == actual.id()));
                    expected.id = actual.id().into();
                }
            }
            compare(preview.preview(), &expected);
            assert!(Arc::ptr_eq(
                &native.core.roster,
                &preview.preview().core.roster
            ));
            for filter in &preview.preview().core.metadata.filters {
                if let Some(old) = native
                    .core
                    .metadata
                    .filters
                    .iter()
                    .find(|old| old.id == filter.id && *old.settings == *filter.settings)
                {
                    assert!(filter.settings.same_allocation(&old.settings));
                }
            }
            let scene = SceneSnapshot::from_native(
                SceneIdentity {
                    project_id: "filter-preview".into(),
                    document_id: expected.id.clone(),
                    revision: expected.revision,
                },
                preview.preview().clone(),
                vec![],
                None,
                None,
            )
            .unwrap();
            for target in [None, Some("a_base")] {
                let frame = crate::render::native_preview::test_frame(
                    &scene,
                    None,
                    8,
                    target,
                    false,
                    &mut || Ok(()),
                )
                .unwrap();
                assert_eq!(
                    &*frame.copy_rgba8(&mut || Ok(())).unwrap(),
                    expected
                        .preview(None, 8, target, false)
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
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            for (expected, actual) in expected
                .filters
                .iter_mut()
                .zip(prepared.preview().metadata().filters())
            {
                expected.id = actual.id().into();
            }
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Filters",
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
            compare(state.native().unwrap(), &expected);
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
                assert!(Arc::ptr_eq(
                    &native.core.roster,
                    &state.native().unwrap().core.roster
                ));
            }
            assert_eq!(runtime.disk().used().unwrap(), disk);
        }
        drop((native, session, staged.seals, cache, runtime, connection));
    }
}

#[test]
fn filter_failures_and_global_reservations_leave_no_partial_batch() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let native =
        NativeDocument::new_blank(8, 8, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let layer = native.metadata().layer(0).unwrap().id().to_owned();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let valid = json!({"op":"filter.add","kind":"invert"});
    let disk = runtime.disk().used().unwrap();
    for command in [
        json!({"op":"filter.add","kind":"liquify"}),
        json!({"op":"filter.add","kind":"color_overlay"}),
        json!({"op":"filter.add","kind":"blur","weight":1.5}),
        json!({"op":"filter.add","kind":"blur","settings":{"unknown":0}}),
        json!({"op":"filter.add","kind":"blur","settings":{"radius":-1}}),
        json!({"op":"filter.add","kind":"blur","settings":{"radius":"\0".repeat(11000)}}),
        json!({"op":"filter.add","kind":"blur","layer":layer}),
        json!({"op":"filter.add","kind":"blur","rect":[0,0,2,2]}),
        json!({"op":"filter.add","kind":"blur","mask":false}),
        json!({"op":"filter.update","filter":"missing","enabled":false}),
        json!({"op":"filter.delete","filter":"missing"}),
        json!({"op":"filter.reorder","filter":"missing","index":0}),
    ] {
        let failed = prepare(
            capture(&state, &[valid.clone(), command]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let failed = prepare(
        capture(
            &state,
            &[
                json!({"op":"layer.update","layer":layer,"locked":true}),
                valid.clone(),
            ],
        ),
        &session,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert!(failed.reason.contains("Unlock"));
    assert!(failed.cleanup_error.is_none());
    let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let commit = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Filter",
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
        scopes: vec![crate::engine::Scope::layer(&layer)],
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
    let edit = capture(&state, &[valid]);
    let pool = runtime.resident();
    let remaining = pool.limit() - pool.used();
    let held = pool.reserve_bytes(remaining, remaining).unwrap();
    assert!(prepare(edit, &session, &|| Ok(())).is_err());
    drop(held);
    assert_eq!(state.native().unwrap().key(), native.key());
    assert_eq!(state.history_lengths(), [0, 0]);
    assert_eq!(runtime.disk().used().unwrap(), disk);
    drop((base, history, state, native, session, runtime, connection));
}

#[test]
fn native_filter_stack_limit_remains_enforced() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = runtime.begin().unwrap();
    let native =
        NativeDocument::new_blank(8, 8, 8, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let state = DocumentAuthority::from_native(native, 4).unwrap();
    let commands = vec![json!({"op":"filter.add","kind":"invert"}); 32];
    let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
    assert_eq!(preview.preview().metadata().filters().len(), 32);
    preview
        .abort(&session, &|| session.cleanup_check())
        .unwrap();
    let mut over = commands;
    over.push(json!({"op":"filter.add","kind":"invert"}));
    assert!(prepare(capture(&state, &over), &session, &|| Ok(()))
        .err()
        .unwrap()
        .reason
        .contains("limit: 32"));
    assert_eq!(runtime.disk().used().unwrap(), 0);
    drop((state, session, runtime, connection));
}

#[test]
fn wide_filter_stages_match_regional_samples_and_retire_on_cancel() {
    use crate::{
        compositor::native::{prepare_filters, prepare_filters_with_layers, Stored, Topology},
        engine::Document,
        raster::backed::Region,
    };
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime_with_budgets(24 * 1024 * 1024, 32 * 1024 * 1024);
        let mut doc = Document::new_depth(1025, 512, depth).unwrap();
        for y in 248..265 {
            for x in 502..524 {
                doc.layers[0].pixels.set16(x, y, [64121, 41503, 34891, 57343]);
            }
        }
        doc.layers[0].pixels.set16(258, 161, [61113, 46291, 38901, 65535]);
        let doc = Engine::preview_edits(doc, &[
            json!({"op":"filter.add","kind":"blur","weight":0.67,"settings":{"radius":36.}}),
            json!({"op":"filter.add","kind":"bloom","settings":{"threshold":0.1,"spread":36.,"strength":0.75}}),
            json!({"op":"filter.add","kind":"invert","weight":0.25}),
        ]).unwrap();
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let staged = CapturedScene::from_parts(doc, "staged-filter-test".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let topology = Topology::new(&staged.scene).unwrap();
        let session = staged.session.as_ref().unwrap();
        assert!(!Stored::needed(&topology));
        assert!(Stored::composite_needed(&topology, None));
        assert!(!Stored::composite_needed(&topology, Some([0, 0, 10, 10])));
        let baseline = runtime.disk().used().unwrap();
        let stored = Stored::prepare_composite(topology.clone(), session, None, &mut || Ok(())).unwrap();
        let full_usage = runtime.disk().used().unwrap();
        assert!(full_usage > baseline);
        for bounds in [[509, 248, 516, 260], [0, 0, 11, 4], [1017, 506, 1025, 512]] {
            let region = Region::new(bounds).unwrap();
            let direct = prepare_filters(&topology, region, &mut || Ok(())).unwrap().unwrap();
            let spilled = prepare_filters_with_layers(&topology, region, Some(&stored), &mut || Ok(())).unwrap().unwrap();
            assert_eq!(direct.bytes(), spilled.bytes());
        }
        stored.retire(&mut || session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let crop = Some([260, 160, 760, 352]);
        assert!(Stored::composite_needed(&topology, crop));
        let stored = Stored::prepare_composite(topology.clone(), session, crop, &mut || Ok(())).unwrap();
        let cropped_usage = runtime.disk().used().unwrap();
        assert!(cropped_usage > baseline && cropped_usage - baseline < (full_usage - baseline) / 2);
        for bounds in [[260, 160, 272, 176], [509, 248, 516, 260], [749, 343, 760, 352]] {
            let region = Region::new(bounds).unwrap();
            let direct = prepare_filters(&topology, region, &mut || Ok(())).unwrap().unwrap();
            let cropped = prepare_filters_with_layers(&topology, region, Some(&stored), &mut || Ok(())).unwrap().unwrap();
            assert_eq!(cropped.bounds(), bounds);
            assert_eq!(direct.bytes(), cropped.bytes());
        }
        let outside = Region::new([259, 160, 261, 161]).unwrap();
        assert!(prepare_filters_with_layers(&topology, outside, Some(&stored), &mut || Ok(())).is_err());
        stored.retire(&mut || session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        let cancelled = Stored::prepare_composite(topology, session, crop, &mut || {
            if runtime.disk().used().unwrap() > baseline {
                Err("Cancel stored filter".into())
            } else {
                Ok(())
            }
        }).err().unwrap();
        assert_eq!(cancelled, "Cancel stored filter");
        assert_eq!(runtime.disk().used().unwrap(), baseline);
    }
}

#[test]
fn native_large_effects_and_channel_filters_render_exact_cropped_samples() {
    use crate::{
        compositor::native::{prepare_filters, Topology},
        engine::Document,
        raster::{backed::Region, Raster},
    };
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let mut original = Document::new_depth(2, 2, depth).unwrap();
        original.width = 16000;
        original.height = 16000;
        let layer = original.layers[0].id.clone();
        let mut pixels = Raster::new_depth(16000, 16000, depth);
        pixels.set16(15994, 15994, [12347, 23459, 34571, 65535]);
        let [red, green, blue, alpha] = pixels.get16(15994, 15994);
        original.layers[0].pixels = pixels;
        let cache = LegacyTileCache::new(&runtime.resident(), 8).unwrap();
        let mut staged = CapturedScene::from_parts(original, "large-filters".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(())).unwrap();
        let native = NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
        let disk = runtime.disk().used().unwrap();
        let commands = [
            json!({"op":"effect.add","layer":layer,"kind":"invert"}),
            json!({"op":"filter.add","kind":"levels","settings":{"channels":[{"output_black":1.,"output_white":0.},{},{}]}}),
            json!({"op":"filter.add","kind":"curves","settings":{
                "interpolation":"linear","points":[[0.,0.],[1.,1.]],"channels":[
                    {"interpolation":"linear","points":[[0.,0.],[1.,1.]]},
                    {"interpolation":"linear","points":[[0.,1.],[1.,0.]]},
                    {"interpolation":"linear","points":[[0.,0.],[1.,1.]]},
                ]
            }}),
            json!({"op":"canvas.resize","width":15999,"height":15999,"anchor":"top_left"}),
        ];
        let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
        let candidate = preview.preview();
        let before = native.metadata().layer(0).unwrap().pixels();
        let after = candidate.metadata().layer(0).unwrap().pixels();
        assert_eq!(native.source(before.slot()).unwrap().key(), candidate.source(after.slot()).unwrap().key());
        let scene = SceneSnapshot::from_native(
            SceneIdentity {
                project_id: "large-filters".into(),
                document_id: candidate.metadata().id().into(),
                revision: candidate.metadata().revision(),
            }, candidate.clone(), vec![], None, None,
        ).unwrap();
        let topology = Topology::new(&scene).unwrap();
        let region = Region::new([15990, 15990, 15999, 15999]).unwrap();
        let output = prepare_filters(&topology, region, &mut || Ok(())).unwrap().unwrap();
        assert_eq!(output.pixel16(15994, 15994).unwrap(), [red, green, 65535 - blue, alpha]);
        assert_eq!(output.pixel16(15995, 15994).unwrap(), [0; 4]);
        drop((output, topology));
        drop(scene);
        preview.abort(&session, &|| session.cleanup_check()).unwrap();

        let stroke = json!({"op":"liquify.stroke","layer":layer,"points":[[15993.,15994.],[15995.,15994.]],"radius":2.});
        let preview = prepare(capture(&state, &[stroke]), &session, &|| Ok(())).unwrap();
        assert_eq!(preview.preview().metadata().layer(0).unwrap().effects().next().unwrap().kind(), "liquify");
        preview.abort(&session, &|| session.cleanup_check()).unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
}
