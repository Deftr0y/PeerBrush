use super::*;
use super::{
    grouping_tests::gate,
    tests::{capture, exercise_edits, Fixture},
};
use crate::engine::{
    authority::DocumentAuthority,
    scene::{CapturedScene, LegacyTileCache},
    Engine,
};
use serde_json::json;

#[test]
fn qoi_import_placement_and_patch_preserve_native_documents_and_undo() {
    let fixture = Fixture::new();
    let path = fixture.path().join("Native.QOI");
    for channels in [3, 4] {
        std::fs::write(
            &path,
            crate::image_import::qoi::tests::fixture(270, 9, channels),
        )
        .unwrap();
        for depth in [8, 16] {
            let original = super::canvas_tests::document(depth);
            let commands = [
                json!({"op":"image.import","path":path,"x":-2,"y":3}),
                json!({"op":"image.import","path":path,"parent":"B","frame":0}),
                json!({"op":"image.paste","path":path,"layer":"outside","origin":[-4,7]}),
                json!({"op":"image.place","path":path,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
                json!({"op":"image.place","path":path,"new_layer":true,"parent":"B","rect":[2,1,8,6]}),
                json!({"op":"image.patch","path":path,"layer":"outside","x":-3,"y":0,"rect":[0,0,6,5]}),
            ];
            for command in &commands {
                exercise_edits(original.clone(), &[command.clone()], true);
            }
            exercise_edits(original.clone(), &commands, true);
            for choice in [json!({"frame":1}), json!({"page":0}), json!({"variant":0})] {
                let mut command = commands[0].clone();
                command
                    .as_object_mut()
                    .unwrap()
                    .extend(choice.as_object().unwrap().clone());
                exercise_edits(original.clone(), &[commands[0].clone(), command], false);
            }
        }
    }
}

#[test]
fn failed_qoi_streams_batches_cancellation_and_late_leases_adopt_nothing() {
    failed_streams("qoi", crate::image_import::qoi::tests::fixture(270, 9, 4));
}

pub(super) fn failed_streams(extension: &str, bytes: Vec<u8>) {
    let fixture = Fixture::new();
    let good = fixture.path().join(format!("good.{extension}"));
    let bad = fixture.path().join(format!("bad.{extension}"));
    std::fs::write(&good, &bytes).unwrap();
    std::fs::write(&bad, &bytes[..bytes.len() - 1]).unwrap();
    let original = super::canvas_tests::document(16);
    let (runtime, connection) = fixture.runtime_with_budgets(512 * 1024 * 1024, 256 * 1024 * 1024);
    let cache = LegacyTileCache::new(&runtime.resident(), 64).unwrap();
    let mut staged = CapturedScene::from_parts(
        original.clone(),
        "image-rollback".into(),
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
    for command in [
        json!({"op":"image.import","path":good,"parent":"B"}),
        json!({"op":"image.paste","path":good,"layer":"outside"}),
        json!({"op":"image.place","path":good,"layer":"outside","new_layer":false,"rect":[-5,-2,9,7]}),
        json!({"op":"image.patch","path":good,"layer":"outside","x":-3,"y":0}),
    ] {
        let mut invalid = command.clone();
        invalid["path"] = json!(bad);
        assert!(Engine::preview_edits(original.clone(), &[invalid.clone()]).is_err());
        for commands in [vec![invalid.clone()], vec![command.clone(), invalid]] {
            let refused = prepare(capture(&state, &commands), &session, &|| Ok(()))
                .err()
                .unwrap();
            assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
        let peak = std::cell::Cell::new(baseline);
        let prepared = prepare(capture(&state, &[command.clone()]), &session, &|| {
            peak.set(peak.get().max(runtime.disk().used().unwrap()));
            Ok(())
        })
        .unwrap();
        prepared
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        for threshold in [baseline + 1, peak.get()] {
            let refused = prepare(capture(&state, &[command.clone()]), &session, &|| {
                if runtime.disk().used().unwrap() >= threshold {
                    Err("Image command interrupted".into())
                } else {
                    Ok(())
                }
            })
            .err()
            .unwrap();
            assert!(
                refused.reason.contains("Image command interrupted"),
                "{}",
                refused.reason
            );
            assert!(refused.cleanup_error.is_none(), "{}", refused.reason);
            assert_eq!(runtime.disk().used().unwrap(), baseline);
        }
        let prepared = prepare(capture(&state, &[command]), &session, &|| Ok(())).unwrap();
        let (base, history, delta) = prepared.into_parts();
        let commit = history
            .prepare_commit(
                &base,
                delta,
                HistoryRequest {
                    actor: "human",
                    label: "Image edit",
                    task: None,
                    gesture: None,
                },
                &|| Ok(()),
            )
            .unwrap();
        let leases = [crate::engine::Lease {
            id: "late".into(),
            owner: "other".into(),
            description: "Document edit".into(),
            scopes: vec![crate::engine::Scope {
                target: None,
                rect: None,
            }],
            expires: 100,
        }];
        let refused = state.commit_native(gate(&leases), commit).unwrap_err();
        assert!(refused.reason().contains("Reserved"));
        refused
            .abort(&session, &|| session.cleanup_check())
            .unwrap();
        assert_eq!(runtime.disk().used().unwrap(), baseline);
        assert_eq!(state.history_lengths(), [0, 0]);
        assert_eq!(state.native().unwrap().key(), native.key());
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
