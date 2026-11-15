use super::*;
use super::{
    grouping_tests::gate,
    import_tests::{file, pixels},
    tests::{capture, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine, Layer,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::json;

fn document(depth: u16, case: usize) -> (Document, &'static str) {
    let mut doc = super::settings_tests::document(depth);
    doc.selection_previous = doc.selection_coverage.clone();
    doc.layers
        .iter_mut()
        .find(|l| l.id == "outside")
        .unwrap()
        .blend = "multiply".into();
    if case == 5 {
        doc.layers.clear();
        return (doc, "removed-target");
    }
    if matches!(case, 3 | 4 | 6) {
        let base = doc.layers.iter_mut().find(|l| l.id == "outside").unwrap();
        if case == 6 {
            base.parent = Some("B".into());
        }
        let mut clip = Layer::new("Clipped", "paint", 8, 8);
        clip.id = "outside_clip".into();
        clip.clip_to = Some(base.id.clone());
        clip.parent = base.parent.clone();
        clip.pixels = crate::raster::Raster::new_depth(8, 8, depth);
        clip.pixels.set16(2, 3, [12347, 23459, 34571, 49153]);
        let at = doc.layers.iter().position(|l| l.id == "outside").unwrap();
        doc.layers.insert(at, clip);
    }
    (
        doc,
        match case {
            1 => "B",
            2 => "b_child",
            3 => "outside_clip",
            _ => "outside",
        },
    )
}

#[test]
fn native_image_paste_preserves_clip_units_sources_precision_and_undo() {
    for (depth, incoming) in [(8, 8), (8, 16), (16, 8), (16, 16)] {
        for case in 0..7 {
            let fixture = Fixture::new();
            let (runtime, connection) =
                fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
            let (original, target) = document(depth, case);
            let path = file(&fixture, incoming);
            let mut command = if case % 2 == 0 {
                json!({"op":"image.paste","layer":target,"path":path,"frame":0})
            } else {
                json!({"op":"image.paste","layer":target,"png":STANDARD.encode(std::fs::read(path).unwrap())})
            };
            if case % 2 == 0 {
                command["origin"] = json!([-4, 7]);
            }
            let commands = if case == 0 {
                vec![command.clone(), command]
            } else {
                vec![command]
            };
            let expected = Engine::preview_edits(original.clone(), &commands).unwrap();
            let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
            let mut staged = CapturedScene::from_parts(
                original.clone(),
                "native-paste".into(),
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
            let preview = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            pixels(preview.preview(), &expected, &original);
            assert!(preview.preview().metadata().selection().is_none());
            assert!(preview.preview().metadata().selection_coverage().is_none());
            let old = native.metadata().selection_previous().unwrap();
            let kept = preview.preview().metadata().selection_previous().unwrap();
            assert_eq!(
                native.source(old.mask().slot()).unwrap().key(),
                preview.preview().source(kept.mask().slot()).unwrap().key()
            );
            preview
                .abort(&session, &|| session.cleanup_check())
                .unwrap();
            assert_eq!(runtime.disk().used().unwrap(), disk);
            let prepared = prepare(capture(&state, &commands), &session, &|| Ok(())).unwrap();
            let (base, history, delta) = prepared.into_parts();
            let commit = history
                .prepare_commit(
                    &base,
                    delta,
                    HistoryRequest {
                        actor: "human",
                        label: "Paste",
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
                pixels(state.native().unwrap(), &expected, &original);
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
}

#[test]
fn paste_failures_and_late_selection_or_clipping_leases_leave_no_provisional_sources() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime_with_budgets(256 * 1024 * 1024, 128 * 1024 * 1024);
    let (original, target) = document(16, 3);
    let valid = json!({"op":"image.paste","layer":target,"png":STANDARD.encode(std::fs::read(file(&fixture,16)).unwrap())});
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let mut staged =
        CapturedScene::from_parts(original.clone(), "paste-refusal".into(), vec![], None, None)
            .stage(&runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let session = staged.session.take().unwrap();
    let mut state = DocumentAuthority::from_native(native.clone(), 4).unwrap();
    let disk = runtime.disk().used().unwrap();
    let file_command = json!({"op":"image.paste","layer":target,"path":file(&fixture,16)});
    for command in [&valid, &file_command] {
        let (_, _, edit) = capture(&state, &[command.clone()]).into_parts();
        let mut budget = crate::image_import::BUDGET as u64 + 1;
        let reason = super::super::import::prepare(edit, &session, &mut budget, None, &|| Ok(()))
            .err()
            .unwrap();
        assert!(reason.contains("256 MiB"), "{reason}");
        assert_eq!(budget, crate::image_import::BUDGET as u64 + 1);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
    }
    for fields in [
        json!({"path":file(&fixture,16)}),
        json!({"page":0}),
        json!({"variant":0}),
        json!({"frame":1}),
        json!({"frame":null}),
        json!({"png":"invalid"}),
        json!({"png":23}),
        json!({"origin":[0.5,1]}),
        json!({"origin":[100001,0]}),
        json!({"origin":[]}),
        json!({"layer":"missing"}),
        json!({"layer":"a_base"}),
    ] {
        let mut invalid = valid.clone();
        invalid
            .as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        assert!(Engine::preview_edits(original.clone(), &[invalid.clone()]).is_err());
        let refused = prepare(
            capture(&state, &[valid.clone(), invalid]),
            &session,
            &|| Ok(()),
        )
        .err()
        .unwrap();
        assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
        assert_eq!(runtime.disk().used().unwrap(), disk);
    }
    let refused = prepare(capture(&state, &[valid.clone()]), &session, &|| {
        if runtime.disk().used().unwrap() > disk {
            Err("paste interrupted".into())
        } else {
            Ok(())
        }
    })
    .err()
    .unwrap();
    assert!(refused.reason.contains("paste interrupted"));
    assert!(refused.cleanup_error.is_none());
    assert_eq!(runtime.disk().used().unwrap(), disk);
    for target in ["@selection", "outside"] {
        let prepared = prepare(capture(&state, &[valid.clone()]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Paste",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let leases = [crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "Paste neighbor".into(),
            scopes: vec![crate::engine::Scope::layer(target)],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(state.native().unwrap().key(), native.key());
        assert_eq!(state.history_lengths(), [0, 0]);
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
