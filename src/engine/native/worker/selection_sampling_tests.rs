use super::*;

fn artwork(depth: u16, prior: usize) -> Document {
    let mut doc = Document::new_depth(269, 19, depth).unwrap();
    doc.icc_profile = Some(Arc::new(
        moxcms::ColorProfile::new_display_p3().encode().unwrap(),
    ));
    for y in 0..19 {
        for x in 0..269 {
            let pixel = if x == 90 {
                [0; 4]
            } else if (6..14).contains(&y) && (10..60).contains(&x) {
                [51713, 7317, 18739, 49153]
            } else if x > 250 {
                [41637, 17371, 31831, 65535]
            } else {
                [12347, 23459, 34571, 65535]
            };
            doc.layers[0].pixels.set16(x, y, pixel);
        }
    }
    doc.filters.push(crate::effects::Effect {
        id: crate::engine::id(),
        kind: "grayscale".into(),
        enabled: true,
        weight: 0.71,
        settings: crate::effects::defaults("grayscale").into(),
    });
    let mut overlay = doc.layers[0].clone();
    overlay.id = crate::engine::id();
    overlay.pixels = crate::raster::Raster::new_depth(269, 19, depth);
    overlay.pixels.set16(4, 3, [61237, 4321, 1377, 65535]);
    doc.layers.insert(0, overlay);
    let mut remembered = doc.clone();
    remembered.selection = Some([2, 2, 7, 5]);
    doc.selection_previous = crate::selection::current(&remembered);
    if prior == 1 {
        let polygon = vec![[1.5, 0.5], [267.5, 1.5], [19.5, 17.5]];
        doc.selection = Some(crate::selection::bounds(&polygon).unwrap());
        doc.selection_polygon = Some(polygon);
    } else if prior == 2 {
        let mut mask = crate::raster::Raster::new(17, 13);
        mask.set(4, 3, [137, 0, 0, 0]);
        mask.set(12, 11, [231, 17, 19, 255]);
        let coverage = crate::selection::Coverage {
            mask,
            bounds: [252, 1, 265, 15],
            origin: [250, -2],
            contours: vec![],
        };
        doc.selection = Some(coverage.bounds);
        doc.selection_coverage = Some(coverage);
    }
    doc
}

#[test]
fn sampled_selections_match_display_color_flood_feather_and_all_combination_modes() {
    for depth in [8, 16] {
        for prior in 0..3 {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
            let original = artwork(depth, prior);
            let target = original.layers[1].id.clone();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "selection-sampling".into(),
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
            let color_keys: Vec<_> = native
                .metadata()
                .layers()
                .map(|l| native.source(l.pixels().slot()).unwrap().key())
                .collect();
            for settings in [
                json!({"kind":"wand","point":[4.7,3.2],"tolerance":0}),
                json!({"kind":"wand","point":[4,3],"contiguous":false,"tolerance":41,"color_similarity":true,"sample_merged":true,"feather":1.6}),
                json!({"kind":"wand","point":[4,3],"points":[[-1,4],[4,3],[260,3]],"tolerance":51,"feather":1}),
                json!({"kind":"quick","point":[4,3],"points":[[30.7,10.2],[260,3],[-1,2]],"radius":7.8,"tolerance":43,"sample_merged":true,"feather":2}),
                json!({"kind":"quick","point":[4,3],"contiguous":false,"points":[],"tolerance":71,"feather":1}),
                json!({"kind":"object","rect":[-5,-2,267,18],"tolerance":19,"feather":1}),
                json!({"kind":"object","rect":[2,2,268,19],"tolerance":17,"color_similarity":true,"sample_merged":true}),
                json!({"kind":"wand","point":[4,3],"points":[]}),
                json!({"kind":"wand","point":[4,3],"points":[["invalid",3]],"tolerance":0}),
            ] {
                for mode in ["replace", "add", "subtract", "intersect"] {
                    let mut command = settings.clone();
                    command["op"] = json!("selection");
                    command["layer"] = json!(target);
                    command["mode"] = json!(mode);
                    let expected =
                        crate::engine::Engine::preview_edits(original.clone(), &[command.clone()])
                            .unwrap();
                    let state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
                    let baseline = runtime.disk().used().unwrap();
                    let prepared =
                        worker::prepare(capture(&state, &[command.clone()]), &session, &|| Ok(()))
                            .unwrap();
                    compare(prepared.preview(), &expected);
                    for (layer, &key) in prepared.preview().metadata().layers().zip(&color_keys) {
                        assert_eq!(
                            prepared
                                .preview()
                                .source(layer.pixels().slot())
                                .unwrap()
                                .key(),
                            key
                        );
                    }
                    assert_eq!(
                        json!(prepared.preview().metadata())["layers"],
                        json!(native.metadata())["layers"]
                    );
                    assert_eq!(
                        prepared.preview().wand().is_some(),
                        command["kind"] == "wand"
                    );
                    prepared
                        .abort(&session, &|| session.cleanup_check())
                        .unwrap();
                    assert_eq!(runtime.disk().used().unwrap(), baseline);
                    assert_eq!(state.native().unwrap().key(), native.key());
                    assert_eq!(state.history_lengths(), [0, 0]);
                }
            }
            drop((native, session, staged.seals, cache, runtime, connection));
        }
    }
}

#[test]
fn wand_updates_replay_original_combination_and_restore_parameters_through_history() {
    for prior in 0..3 {
        let fixture = Fixture::new();
        let (runtime, connection) =
            fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
        let original = artwork(16, prior);
        let mut oracle = original.clone();
        let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
        let mut staged =
            CapturedScene::from_parts(original.clone(), "wand-replay".into(), vec![], None, None)
                .stage(&runtime, &cache, &|| Ok(()))
                .unwrap();
        let native =
            NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
        let session = staged.session.take().unwrap();
        let mut state = DocumentAuthority::from_native(native.clone(), 2).unwrap();
        let first = json!({"op":"selection","kind":"wand","point":[4,3],"mode":"subtract","tolerance":41,"feather":2});
        let mut replay_id = None;
        for command in [
            first.clone(),
            json!({"op":"selection.wand_update","tolerance":0}),
            json!({"op":"selection.wand_update","tolerance":71,"contiguous":false,"color_similarity":true,"sample_merged":true,"feather":1}),
            json!({"op":"selection.wand_update","tolerance":24,"contiguous":true,"feather":0}),
        ] {
            let before = oracle.clone();
            let before_command = state.native().unwrap().wand().map(|w| w.command().clone());
            oracle = crate::engine::Engine::preview_edits(oracle, &[command.clone()]).unwrap();
            let prepared =
                worker::prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
            compare(prepared.preview(), &oracle);
            let replay = prepared.preview().wand().unwrap();
            assert_eq!(
                replay.command(),
                &oracle.wand_source.as_ref().unwrap().command
            );
            let id = replay.id().to_owned();
            if let Some(previous_id) = &replay_id {
                assert_eq!(&id, previous_id);
            }
            replay_id = Some(id.clone());
            commit(&mut state, prepared, &session);
            for (direction, expected) in [(Direction::Undo, &before), (Direction::Redo, &oracle)] {
                let moved = state
                    .native_history()
                    .unwrap()
                    .prepare_move(state.native().unwrap(), direction, "human", &|| Ok(()))
                    .unwrap();
                drop(state.move_native(publication(&[]), moved).unwrap());
                compare(state.native().unwrap(), expected);
                if matches!(direction, Direction::Undo) {
                    assert_eq!(
                        state.native().unwrap().wand().map(|w| w.command()),
                        before_command.as_ref()
                    );
                } else {
                    assert_eq!(state.native().unwrap().wand().unwrap().id(), id);
                }
            }
        }
        let mut batch_state = DocumentAuthority::from_native(native.clone(), 2).unwrap();
        let commands = [
            first,
            json!({"op":"selection.wand_update","tolerance":0,"feather":1}),
        ];
        let expected = crate::engine::Engine::preview_edits(original.clone(), &commands).unwrap();
        let prepared =
            worker::prepare(capture(&batch_state, &commands), &session, &|| Ok(())).unwrap();
        compare(prepared.preview(), &expected);
        commit(&mut batch_state, prepared, &session);
        assert_eq!(
            batch_state.native().unwrap().metadata().revision(),
            native.metadata().revision() + 1
        );
        assert_eq!(batch_state.history_lengths(), [1, 0]);
        let ordinary = worker::prepare(
            capture(&state, &[json!({"op":"selection.reselect"})]),
            &session,
            &|| Ok(()),
        )
        .unwrap();
        assert!(ordinary.preview().wand().is_none());
        commit(&mut state, ordinary, &session);
        let baseline = runtime.disk().used().unwrap();
        let failed = worker::prepare(
            capture(
                &state,
                &[json!({"op":"selection.wand_update","tolerance":1})],
            ),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.reason.contains("Click with the Magic Wand again"));
        assert!(failed.cleanup_error.is_none());
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        drop((
            state,
            batch_state,
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
fn sampled_selection_failure_cancellation_and_late_reservations_retire_all_provisional_output() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(128 * 1024 * 1024, 64 * 1024 * 1024);
    let session = runtime.begin().unwrap();
    let native = NativeDocument::new_blank(
        269,
        19,
        16,
        &runtime.resident(),
        &runtime.disk(),
        &|| Ok(()),
    )
    .unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let valid = json!({"op":"selection","kind":"wand","point":[4,3],"feather":1});
    let baseline = runtime.disk().used().unwrap();
    for invalid in [
        json!({"op":"selection","kind":"wand","point":[-1,2]}),
        json!({"op":"selection","kind":"wand","color_similarity":"true"}),
        json!({"op":"selection","kind":"wand","color_similarity":true,"tolerance":256}),
        json!({"op":"selection","kind":"quick","points":vec![[1,1];8193]}),
        json!({"op":"selection","kind":"wand","mode":"invalid"}),
        json!({"op":"selection","kind":"wand","layer":"missing"}),
        json!({"op":"selection","kind":"object","rect":[2,2,2,3]}),
        json!({"op":"selection.wand_update","feather":65}),
        json!({"op":"selection.wand_update","sample_merged":1}),
    ] {
        let failed = worker::prepare(
            capture(&state, &[valid.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(failed.cleanup_error.is_none(), "{}", failed.reason);
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
    let completed = crate::selection::CompletedTiles::observe();
    let failed = worker::prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if completed.count() > 0 {
            Err("Sampled selection cancelled after coverage tile".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(failed.reason.contains("cancelled after coverage tile"));
    assert!(failed.cleanup_error.is_none());
    assert_eq!(completed.count(), 1);
    drop(completed);
    assert_eq!(runtime.disk().used().unwrap(), baseline);
    let prepared = worker::prepare(capture(&state, &[valid]), &session, &|| Ok(())).unwrap();
    let (base, history, delta) = prepared.into_parts();
    let committed = history
        .prepare_commit(
            &base,
            delta,
            HistoryRequest {
                actor: "human",
                label: "Sample selection",
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
    assert!(state.native().unwrap().wand().is_none());
    assert_eq!(state.history_lengths(), [0, 0]);
    drop((base, state, native, session, runtime, connection));
}
