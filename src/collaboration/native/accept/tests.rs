use super::*;
use crate::{
    collaboration::native::capture::tests::project,
    engine::{
        native::{worker::tests::Fixture, Direction},
        Scope,
    },
    server,
};
use serde_json::json;
use std::sync::atomic::Ordering;

#[test]
fn human_acceptance_keeps_frozen_native_pixels_actor_task_and_one_history_entry() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, _workspace, original) = project(&runtime, depth);
        let layer = &original.layers[0].id;
        let task = shared
            .lock()
            .unwrap()
            .reserve(
                "proposal-agent",
                "Propose",
                vec![Scope {
                    target: None,
                    rect: None,
                }],
            )
            .unwrap()
            .id;
        let commands = [
            json!({"op":"paint.fill","layer":layer,"rect":[4,3,10,9],"color":[31,73,149,177]}),
            json!({"op":"layer.add","name":"Proposed"}),
        ];
        let draft = Draft::prepare_shared(
            &shared,
            "proposal-agent",
            &commands,
            &original.id,
            0,
            "Frozen review",
            Some(&task),
            &|| Ok(()),
        )
        .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let frozen_key = draft.prepared.preview().key();
        let pixels = draft
            .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
            .unwrap();
        let frozen = pixels.frame().copy_rgba8(&mut || Ok(())).unwrap();
        let created = draft
            .prepared
            .created_layers()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 1);
        let control = EditControl::default();
        let reply = draft
            .accept_shared(&shared, "human", &original.id, 0, &control, &|| {
                assert!(
                    shared.try_lock().is_ok(),
                    "No supplied callback runs under Engine"
                );
                Ok(())
            })
            .unwrap_or_else(|refused| panic!("{}", refused.reason));
        assert!(control.is_finished());
        assert!(!control.cancel());
        assert_eq!(reply.data()["revision"], 1);
        assert_eq!(reply.data()["applied"], commands.len());
        assert_eq!(reply.data()["created"], json!(created));
        {
            let engine = shared.lock().unwrap();
            assert_eq!(engine.native_document().unwrap().key(), frozen_key);
            assert_eq!(engine.native_document().unwrap().metadata().depth(), depth);
            assert_eq!(engine.history_lengths(), [1, 0]);
            let entry = engine.history_entries().next().unwrap();
            assert_eq!(entry.actor(), "proposal-agent");
            assert_eq!(entry.task(), Some(task.as_str()));
            assert_eq!(entry.label(), "Frozen review");
            assert_eq!(engine.ai_change.as_ref().unwrap().actor, "proposal-agent");
            assert_eq!(engine.ai_change.as_ref().unwrap().label, "Frozen review");
            assert!(engine
                .leases
                .iter()
                .any(|lease| lease.id == task && lease.owner == "proposal-agent"));
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        }
        let scene = shared
            .lock()
            .unwrap()
            .capture_scene(None)
            .unwrap()
            .into_native()
            .unwrap();
        let read = crate::workspace::native_dispatch::begin_read_scene(&shared, &scene).unwrap();
        let request =
            native_preview::Request::capture(Arc::new(scene), read, None, 19, None, false).unwrap();
        let accepted = request.render(&mut || Ok(())).unwrap();
        assert_eq!(&*accepted.copy_rgba8(&mut || Ok(())).unwrap(), &*frozen);
        drop(request);
        server::history_shared(
            &shared,
            "human",
            Direction::Undo,
            Some(1),
            Some(&original.id),
        )
        .unwrap();
        let native = {
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [0, 1]);
            engine.native_document().unwrap().clone()
        };
        assert_eq!(native.metadata().layers().len(), original.layers.len());
        let source = native.source(native.layer_slot(layer).unwrap()).unwrap();
        let region = source
            .prepare_region(
                crate::raster::backed::Region::new([2, 3, 3, 4]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        assert_eq!(
            region.pixel16(2, 3).unwrap(),
            original.layers[0].pixels.get16(2, 3)
        );
        server::history_shared(
            &shared,
            "human",
            Direction::Redo,
            Some(2),
            Some(&original.id),
        )
        .unwrap();
        assert_eq!(shared.lock().unwrap().history_lengths(), [1, 0]);
        assert_eq!(
            &*pixels.frame().copy_rgba8(&mut || Ok(())).unwrap(),
            &*frozen
        );
    }
}

#[test]
fn accepting_an_import_uses_reviewed_native_words_when_the_input_file_changes() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, _workspace, original) = project(&runtime, depth);
        let path = fixture.path().join("reviewed.png");
        let words = [12347, 23459, 34571, 49153, 54321, 32109, 10987, 0];
        let png = if depth == 16 {
            crate::raster::png16(2, 1, &words)
        } else {
            crate::raster::png(2, 1, &words.map(crate::raster::project16))
        }
        .unwrap();
        std::fs::write(&path, png).unwrap();
        let commands = [json!({"op":"image.import","path":path,"x":3,"y":4,"frame":0})];
        let draft = Draft::prepare_shared(
            &shared,
            "agent",
            &commands,
            &original.id,
            0,
            "Reviewed import",
            None,
            &|| Ok(()),
        )
        .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let created = draft
            .prepared
            .created_layers()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 1);
        let key = draft.prepared.preview().key();
        let reviewed = draft
            .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
            .unwrap();
        // Acceptance must neither decode the replacement nor replay this path.
        std::fs::write(&path, b"replacement is no longer an image").unwrap();
        draft
            .accept_shared(
                &shared,
                "human",
                &original.id,
                0,
                &EditControl::default(),
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason));
        let native = {
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [1, 0]);
            assert_eq!(engine.native_document().unwrap().key(), key);
            engine.native_document().unwrap().clone()
        };
        let source = native
            .source(native.layer_slot(&created[0]).unwrap())
            .unwrap();
        let region = source
            .prepare_region(
                crate::raster::backed::Region::new([0, 0, 2, 1]).unwrap(),
                &mut || Ok(()),
            )
            .unwrap();
        let expected = if depth == 16 {
            words
        } else {
            words.map(|word| u16::from(crate::raster::project16(word)) * 257)
        };
        assert_eq!(region.pixel16(0, 0).unwrap(), expected[..4]);
        assert_eq!(region.pixel16(1, 0).unwrap(), expected[4..]);
        assert!(!reviewed
            .frame()
            .copy_rgba8(&mut || Ok(()))
            .unwrap()
            .is_empty());
    }
}

#[test]
fn refused_acceptance_returns_the_same_candidate_and_sources_without_publishing() {
    for depth in [8, 16] {
        for refusal in [
            "actor",
            "document",
            "revision",
            "project",
            "task",
            "expired",
            "reservation",
            "runtime",
            "closed",
            "cancel",
            "check",
            "admission",
        ] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let (shared, workspace, original) = project(&runtime, depth);
            let task = if ["task", "expired"].contains(&refusal) {
                Some(
                    shared
                        .lock()
                        .unwrap()
                        .reserve(
                            "agent",
                            "Propose",
                            vec![Scope::layer(&original.layers[0].id)],
                        )
                        .unwrap()
                        .id,
                )
            } else {
                None
            };
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
                task.as_deref(),
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason()));
            let key = draft.prepared.preview().key();
            let held = runtime.disk().used().unwrap();
            let control = EditControl::default();
            let mut admission = None;
            let project_id = shared.lock().unwrap().project_id.clone();
            match refusal {
                "project" => shared.lock().unwrap().project_id = "changed-project".into(),
                "task" => shared.lock().unwrap().take_over_tasks(),
                "expired" => shared.lock().unwrap().leases[0].expires = 0,
                "reservation" => {
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
                "runtime" => workspace.lock().unwrap().source_runtime = None,
                "closed" => shared.lock().unwrap().closed = true,
                "cancel" => {
                    assert!(control.cancel());
                }
                "admission" => {
                    let pool = runtime.resident();
                    admission = Some(
                        pool.reserve_bytes(pool.limit() - pool.used(), pool.limit())
                            .unwrap(),
                    );
                }
                _ => (),
            }
            let refused = draft
                .accept_shared(
                    &shared,
                    if refusal == "actor" { "agent" } else { "human" },
                    if refusal == "document" {
                        "old-document"
                    } else {
                        &original.id
                    },
                    if refusal == "revision" { 1 } else { 0 },
                    &control,
                    &|| {
                        if refusal == "check" {
                            Err("Stop acceptance".into())
                        } else {
                            Ok(())
                        }
                    },
                )
                .err()
                .unwrap_or_else(|| panic!("{refusal} must refuse"));
            drop(admission);
            assert!(control.is_finished());
            assert_eq!(runtime.disk().used().unwrap(), held, "{refusal}");
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
            assert_eq!(refused.draft.prepared.preview().key(), key);
            assert_eq!(
                shared
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire),
                0
            );
            shared.lock().unwrap().project_id = project_id;
            shared.lock().unwrap().closed = false;
            workspace.lock().unwrap().source_runtime = Some(runtime.clone());
            let draft = refused.into_draft();
            if !["task", "expired"].contains(&refusal) {
                assert!(
                    draft
                        .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
                        .is_ok(),
                    "{refusal}"
                );
            }
            draft.discard().unwrap();
            assert_eq!(runtime.disk().used().unwrap(), before, "{refusal}");
        }
    }
}

#[test]
fn final_runtime_fence_refusal_keeps_the_frozen_candidate_for_review_and_retry() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, _workspace, original) = project(&runtime, depth);
        let commands = [
            json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
        ];
        let draft = Draft::prepare_shared(
            &shared,
            "agent",
            &commands,
            &original.id,
            0,
            "Frozen review",
            None,
            &|| Ok(()),
        )
        .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let key = draft.prepared.preview().key();
        let frozen = draft
            .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
            .unwrap();
        let before = runtime.disk().used().unwrap();
        let checks = std::cell::Cell::new(0);
        let control = EditControl::default();
        let refused = draft
            .accept_shared(&shared, "human", &original.id, 0, &control, &|| {
                assert!(shared.try_lock().is_ok());
                checks.set(checks.get() + 1);
                if checks.get() == 3 {
                    // Does not change the document, task or source generation. The
                    // actual Engine publication must refuse its stale AI serial.
                    shared.lock().unwrap().mark_ai(
                        "other-agent",
                        "Later activity",
                        "paint",
                        vec![],
                    );
                }
                Ok(())
            })
            .err()
            .expect("Actual final publication must refuse");
        assert!(checks.get() >= 3);
        assert!(
            refused.reason.contains("Runtime effects differ"),
            "{}",
            refused.reason
        );
        assert!(control.is_finished());
        assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
        assert_eq!(
            shared.lock().unwrap().ai_change.as_ref().unwrap().actor,
            "other-agent"
        );
        assert_eq!(refused.draft.prepared.preview().key(), key);
        assert_eq!(runtime.disk().used().unwrap(), before);
        let draft = refused.into_draft();
        let retry_view = draft
            .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
            .unwrap();
        assert_eq!(
            &*retry_view.frame().copy_rgba8(&mut || Ok(())).unwrap(),
            &*frozen.frame().copy_rgba8(&mut || Ok(())).unwrap()
        );
        draft
            .accept_shared(
                &shared,
                "human",
                &original.id,
                0,
                &EditControl::default(),
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason));
        let engine = shared.lock().unwrap();
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert_eq!(engine.history_lengths(), [1, 0]);
        assert_eq!(engine.history_entries().next().unwrap().actor(), "agent");
        assert_eq!(engine.ai_change.as_ref().unwrap().actor, "agent");
    }
}

#[test]
fn takeover_and_cancellation_during_preparation_never_reacquire_or_commit_the_task() {
    for depth in [8, 16] {
        for action in ["takeover", "cancel", "runtime"] {
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
            let key = draft.prepared.preview().key();
            let disk = runtime.disk().used().unwrap();
            let control = EditControl::default();
            let checks = std::cell::Cell::new(0);
            let refused = draft
                .accept_shared(&shared, "human", &original.id, 0, &control, &|| {
                    assert!(shared.try_lock().is_ok());
                    checks.set(checks.get() + 1);
                    if checks.get() == 3 {
                        match action {
                            "takeover" => shared.lock().unwrap().take_over_tasks(),
                            "cancel" => {
                                assert!(control.cancel());
                            }
                            "runtime" => workspace.lock().unwrap().source_runtime = None,
                            _ => unreachable!(),
                        }
                    }
                    Ok(())
                })
                .err()
                .expect("Interrupted acceptance must refuse");
            assert!(checks.get() >= 3);
            assert!(control.is_finished());
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
            assert_eq!(refused.draft.prepared.preview().key(), key);
            assert_eq!(runtime.disk().used().unwrap(), disk);
            if action == "takeover" {
                assert!(shared.lock().unwrap().leases.is_empty());
            }
            refused.discard().unwrap();
        }
    }
}

#[test]
fn a_late_human_reservation_refuses_publication_and_preserves_the_candidate_for_retry() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, _workspace, original) = project(&runtime, depth);
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
            None,
            &|| Ok(()),
        )
        .unwrap_or_else(|refused| panic!("{}", refused.reason()));
        let key = draft.prepared.preview().key();
        let disk = runtime.disk().used().unwrap();
        let checks = std::cell::Cell::new(0);
        let reservation = std::cell::RefCell::new(None);
        let control = EditControl::default();
        let refused = draft
            .accept_shared(&shared, "human", &original.id, 0, &control, &|| {
                assert!(shared.try_lock().is_ok());
                checks.set(checks.get() + 1);
                if checks.get() == 3 {
                    *reservation.borrow_mut() = Some(
                        shared
                            .lock()
                            .unwrap()
                            .reserve(
                                "human",
                                "Keep human work",
                                vec![Scope::layer(&original.layers[0].id)],
                            )
                            .unwrap()
                            .id,
                    );
                }
                Ok(())
            })
            .err()
            .expect("Native publication must respect the new reservation");
        assert!(checks.get() >= 3);
        assert!(
            refused.reason.contains("Reserved by human"),
            "{}",
            refused.reason
        );
        assert!(control.is_finished());
        assert_eq!(refused.draft.prepared.preview().key(), key);
        assert_eq!(runtime.disk().used().unwrap(), disk);
        {
            let engine = shared.lock().unwrap();
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert!(engine.ai_change.is_none());
            assert!(engine
                .leases
                .iter()
                .any(|lease| lease.owner == "human"
                    && Some(&lease.id) == reservation.borrow().as_ref()));
            assert_eq!(engine.file_operations.load(Ordering::Acquire), 0);
        }
        let draft = refused.into_draft();
        assert!(draft
            .render_shared(&shared, None, 19, None, false, None, &|| Ok(()))
            .is_ok());
        shared
            .lock()
            .unwrap()
            .finish_task(reservation.borrow().as_ref().unwrap(), "ended");
        draft
            .accept_shared(
                &shared,
                "human",
                &original.id,
                0,
                &EditControl::default(),
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason));
        let engine = shared.lock().unwrap();
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert_eq!(engine.history_lengths(), [1, 0]);
        assert_eq!(engine.history_entries().next().unwrap().actor(), "agent");
    }
}
