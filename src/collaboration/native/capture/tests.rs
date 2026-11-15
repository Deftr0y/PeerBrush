use super::*;
use crate::{
    engine::{
        native::worker::tests::Fixture,
        scene::{CapturedScene, LegacyTileCache},
        Document, Scope,
    },
    workspace::{self, source_runtime::SourceRuntime, Registry},
};
use serde_json::json;
use std::sync::{atomic::Ordering, Mutex};

pub(crate) fn project(runtime: &Arc<SourceRuntime>, depth: u16) -> (Shared, Registry, Document) {
    let mut original = Document::new_depth(19, 13, depth).unwrap();
    original.layers[0]
        .pixels
        .set16(2, 3, [12347, 23459, 34571, 61379]);
    let cache = LegacyTileCache::new(&runtime.resident(), 32).unwrap();
    let staged =
        CapturedScene::from_parts(original.clone(), "capture-test".into(), vec![], None, None)
            .stage(runtime, &cache, &|| Ok(()))
            .unwrap();
    let native =
        NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap()).unwrap();
    let shared = Arc::new(Mutex::new(Engine::from_native(native, 8).unwrap()));
    let workspace = workspace::attach(&shared);
    workspace.lock().unwrap().source_runtime = Some(runtime.clone());
    (shared, workspace, original)
}
fn failure(result: std::result::Result<Draft, CaptureFailure>) -> CaptureFailure {
    result.err().expect("Capture must refuse")
}
fn retire(refused: CaptureFailure) {
    if let Some(retirement) = refused
        .prepare_retirement()
        .unwrap_or_else(|(refused, reason)| panic!("{}; {reason}", refused.reason()))
    {
        retirement
            .retire()
            .unwrap_or_else(|refused| panic!("{}", refused.reason));
    }
}

#[test]
fn shared_capture_reviews_regions_targets_and_selection_without_publishing_sources() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, _workspace, original) = project(&runtime, depth);
        let layer = &original.layers[0].id;
        let (key, metadata, disk_before, project) = {
            let engine = shared.lock().unwrap();
            let doc = engine.native_document().unwrap();
            (
                doc.key(),
                json!(doc.metadata()),
                runtime.disk().used().unwrap(),
                engine.project_id.clone(),
            )
        };
        let commands = [
            json!({"op":"paint.fill","layer":layer,"rect":[4,3,10,9],"color":[31,73,149,177]}),
            json!({"op":"selection","kind":"rectangle","rect":[5,4,9,8]}),
            json!({"op":"mask.from_selection","layer":layer}),
        ];
        let draft = Draft::prepare_shared(
            &shared,
            "agent",
            &commands,
            &original.id,
            0,
            "Review",
            None,
            &|| {
                assert!(
                    shared.try_lock().is_ok(),
                    "No app check may run under Engine"
                );
                Ok(())
            },
        )
        .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        assert!(runtime.disk().used().unwrap() > disk_before);
        let mut oracle = Engine::from_document(original.clone());
        oracle
            .edit("agent", &commands, Some(0), None, "Review")
            .unwrap();
        let expected = &oracle.resident_state().unwrap().document;
        for (target, mask, mode) in [
            (None, false, None),
            (Some(layer.as_str()), false, None),
            (Some(layer.as_str()), true, None),
            (None, false, Some("mask")),
            (None, false, Some("cutout")),
            (None, false, Some("overlay")),
        ] {
            let rect = Some([3, 2, 12, 10]);
            let frame = draft
                .render_shared(&shared, rect, 9, target, mask, mode, &|| {
                    assert!(shared.try_lock().is_ok());
                    Ok(())
                })
                .unwrap();
            let (width, height, mut bytes, bounds) =
                expected.preview(rect, 9, target, mask).unwrap();
            if let Some(mode) = mode {
                crate::selection::display::apply_region(
                    expected, width, height, &mut bytes, bounds, mode,
                );
            }
            assert_eq!(frame.frame().key().rect, rect.unwrap());
            assert!(frame.origin().matches_source(&project, key, 0));
            assert!(frame.origin().matches_frame(frame.frame()));
            assert_eq!(&*frame.frame().copy_rgba8(&mut || Ok(())).unwrap(), &bytes);
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        }
        assert!(draft
            .render_shared(&shared, None, 9, None, false, None, &|| Err(
                "Stop review".into()
            ))
            .is_err());
        // Display owners carry no candidate raster/session and cannot stop abort.
        let frame = draft
            .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
            .unwrap();
        draft.discard().unwrap();
        assert_eq!(runtime.disk().used().unwrap(), disk_before);
        assert!(!frame.frame().copy_rgba8(&mut || Ok(())).unwrap().is_empty());
        let engine = shared.lock().unwrap();
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert_eq!(
            json!(engine.native_document().unwrap().metadata()),
            metadata
        );
        assert_eq!(engine.history_lengths(), [0, 0]);
        assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
    }
}

#[test]
fn shared_capture_refuses_old_sources_reservations_tasks_and_invalid_input_before_source_work() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, workspace, original) = project(&runtime, depth);
        let commands = [
            json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
        ];
        let before = runtime.disk().used().unwrap();
        for refusal in [
            "document",
            "revision",
            "actor",
            "label",
            "task",
            "reservation",
            "cancel",
            "worker",
            "commands",
            "runtime",
        ] {
            let task = if refusal == "reservation" {
                Some(
                    shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "human",
                            "Keep work",
                            vec![Scope::layer(&original.layers[0].id)],
                        )
                        .unwrap()
                        .id,
                )
            } else {
                None
            };
            if refusal == "runtime" {
                workspace.lock().unwrap().source_runtime = None;
            }
            let other_commands = match refusal {
                "worker" => vec![json!({"op":"new","width":10,"height":10})],
                "commands" => Vec::new(),
                _ => commands.to_vec(),
            };
            let refused = failure(Draft::prepare_shared(
                &shared,
                if refusal == "actor" { "" } else { "agent" },
                &other_commands,
                if refusal == "document" {
                    "replaced"
                } else {
                    &original.id
                },
                if refusal == "revision" { 1 } else { 0 },
                if refusal == "label" { "" } else { "Review" },
                (refusal == "task").then_some("ended-task"),
                &|| {
                    if refusal == "cancel" {
                        Err("Stop capture".into())
                    } else {
                        Ok(())
                    }
                },
            ));
            assert!(
                matches!(&refused, CaptureFailure::Refused(_)),
                "{refusal}: {}",
                refused.reason()
            );
            retire(refused);
            workspace.lock().unwrap().source_runtime = Some(runtime.clone());
            if let Some(task) = task {
                shared.lock().unwrap().finish_task(&task, "ended");
            }
            assert_eq!(runtime.disk().used().unwrap(), before, "{refusal}");
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        }
    }
}

#[test]
fn late_capture_refusal_returns_prepared_ownership_and_review_observes_takeover() {
    for depth in [8, 16] {
        for change in ["takeover", "reservation", "source", "project", "runtime"] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let (shared, workspace, original) = project(&runtime, depth);
            let task = shared
                .lock()
                .unwrap()
                .reserve(
                    "agent",
                    "Propose",
                    vec![Scope::layer(&original.layers[0].id)],
                )
                .unwrap()
                .id;
            let before = runtime.disk().used().unwrap();
            let commands = [
                json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
            ];
            let draft = Draft::prepare_shared(
                &shared,
                "agent",
                &commands,
                &original.id,
                0,
                "Review",
                Some(&task),
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
            let project_id = shared.lock().unwrap().project_id.clone();
            let held = runtime.disk().used().unwrap();
            match change {
                "takeover" => shared.lock().unwrap().take_over_tasks(),
                "reservation" => {
                    shared.lock().unwrap().finish_task(&task, "ended");
                    shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "human",
                            "Keep work",
                            vec![Scope::layer(&original.layers[0].id)],
                        )
                        .unwrap();
                }
                "source" => {
                    let replacement = NativeDocument::new_blank(
                        19,
                        13,
                        depth,
                        &runtime.resident(),
                        &runtime.disk(),
                        &|| Ok(()),
                    )
                    .unwrap();
                    let retired = std::mem::replace(
                        &mut shared.lock().unwrap().authority,
                        crate::engine::authority::DocumentAuthority::from_native(replacement, 8)
                            .unwrap(),
                    );
                    drop(retired);
                }
                "project" => shared.lock().unwrap().project_id = "different-project".into(),
                "runtime" => workspace.lock().unwrap().source_runtime = None,
                _ => unreachable!(),
            }
            assert!(
                draft
                    .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
                    .is_err(),
                "{change}"
            );
            assert_eq!(runtime.disk().used().unwrap(), held);
            // Cleanup uses the original session even after registry/source changes.
            draft.discard().unwrap();
            assert_eq!(runtime.disk().used().unwrap(), before);
            shared.lock().unwrap().project_id = project_id;
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        }
    }
}

#[test]
fn changes_after_native_preparation_keep_the_owner_for_retryable_retirement() {
    for depth in [8, 16] {
        for change in ["cancel", "takeover", "reservation", "source"] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let (shared, _workspace, original) = project(&runtime, depth);
            let task = shared
                .lock()
                .unwrap()
                .reserve(
                    "agent",
                    "Propose",
                    vec![Scope::layer(&original.layers[0].id)],
                )
                .unwrap()
                .id;
            let before = runtime.disk().used().unwrap();
            let changed = std::cell::Cell::new(false);
            let commands = [
                json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
            ];
            let refused = failure(Draft::prepare_shared(
                &shared,
                "agent",
                &commands,
                &original.id,
                0,
                "Review",
                Some(&task),
                &|| {
                    assert!(shared.try_lock().is_ok());
                    // The source is a small one-tile write. Its committed fragment
                    // exists before the final live-source/reservation revalidation.
                    if runtime.disk().used().unwrap() > before && !changed.replace(true) {
                        match change {
                            "cancel" => return Err("Stop after preparation".into()),
                            "takeover" => shared.lock().unwrap().take_over_tasks(),
                            "reservation" => {
                                shared.lock().unwrap().finish_task(&task, "ended");
                                shared
                                    .lock()
                                    .unwrap()
                                    .reserve(
                                        "human",
                                        "Keep work",
                                        vec![Scope::layer(&original.layers[0].id)],
                                    )
                                    .unwrap();
                            }
                            "source" => {
                                shared.lock().unwrap().project_id = "changed-project".into()
                            }
                            _ => unreachable!(),
                        }
                    }
                    Ok(())
                },
            ));
            assert!(changed.get(), "{change}");
            // Cancellation can interrupt the producer itself; its cleanup then
            // remains the worker's responsibility. Other changes leave a draft.
            if change != "cancel" {
                assert!(
                    matches!(&refused, CaptureFailure::Prepared(_)),
                    "{change}: {}",
                    refused.reason()
                );
                assert!(runtime.disk().used().unwrap() > before);
            }
            if let Some(retirement) = refused
                .prepare_retirement()
                .unwrap_or_else(|(_, reason)| panic!("{reason}"))
            {
                let paused = retirement
                    .retire_with(&|| Err("Pause cleanup".into()))
                    .err()
                    .expect("Cleanup refusal retains the source owner");
                assert_eq!(paused.reason, "Pause cleanup");
                let retirement = paused.into_retirement();
                assert!(retirement.retryable());
                retirement
                    .retire()
                    .unwrap_or_else(|refused| panic!("{}", refused.reason));
            }
            assert_eq!(runtime.disk().used().unwrap(), before);
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        }
    }
}

#[test]
fn changes_during_review_return_no_frame_and_keep_the_draft_for_original_session_cleanup() {
    for depth in [8, 16] {
        for change in ["takeover", "source", "runtime", "closed"] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let (shared, workspace, original) = project(&runtime, depth);
            let task = shared
                .lock()
                .unwrap()
                .reserve(
                    "agent",
                    "Propose",
                    vec![Scope::layer(&original.layers[0].id)],
                )
                .unwrap()
                .id;
            let before = runtime.disk().used().unwrap();
            let commands = [
                json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
            ];
            let draft = Draft::prepare_shared(
                &shared,
                "agent",
                &commands,
                &original.id,
                0,
                "Review",
                Some(&task),
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
            let held = runtime.disk().used().unwrap();
            let checks = std::cell::Cell::new(0);
            let review = draft.render_shared(&shared, None, 19, None, false, None, &|| {
                assert!(shared.try_lock().is_ok());
                checks.set(checks.get() + 1);
                if checks.get() == 3 {
                    match change {
                        "takeover" => shared.lock().unwrap().take_over_tasks(),
                        "source" => shared.lock().unwrap().project_id = "changed-project".into(),
                        "runtime" => workspace.lock().unwrap().source_runtime = None,
                        "closed" => shared.lock().unwrap().closed = true,
                        _ => unreachable!(),
                    }
                }
                Ok(())
            });
            assert!(checks.get() >= 3);
            assert!(review.is_err(), "{change}");
            assert_eq!(runtime.disk().used().unwrap(), held);
            draft.discard().unwrap();
            assert_eq!(runtime.disk().used().unwrap(), before);
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        }
    }
}
