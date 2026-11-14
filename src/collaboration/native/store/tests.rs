use super::*;
use crate::{
    collaboration::native::capture::tests::project, engine::native::worker::tests::Fixture,
};
use serde_json::json;

fn commands(original: &engine::Document) -> Vec<Value> {
    vec![
        json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[4,3,10,9],"color":[31,73,149,177]}),
    ]
}
fn create(shared: &Shared, original: &engine::Document, task: Option<&str>) -> String {
    create_stored(
        shared,
        "agent",
        &commands(original),
        &original.id,
        0,
        "Stored review",
        task,
    )
    .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned()
}
fn store(workspace: &workspace::Registry) -> Arc<Mutex<Store>> {
    workspace.lock().unwrap().native_proposals.clone().unwrap()
}

#[test]
fn stored_native_candidates_share_engine_state_review_and_atomic_human_acceptance() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, workspace, original) = project(&runtime, depth);
        let before = runtime.disk().used().unwrap();
        let id = create(&shared, &original, None);
        let store = store(&workspace);
        {
            let mut engine = shared.lock().unwrap();
            assert_eq!(engine.proposals_state()[0]["id"], id);
            assert!(engine.proposals[0].draft.is_none());
            assert!(engine.proposals[0].native.is_some());
            assert!(engine.proposals[0].native_charge.is_some());
            assert!(engine.proposals[0].commands.is_empty());
            assert_eq!(engine.history_lengths(), [0, 0]);
            assert_eq!(engine.native_document().unwrap().metadata().revision(), 0);
        }
        assert!(runtime.disk().used().unwrap() > before);
        let frame = review_stored(&shared, &id, None, 19, None, false, None, &|| {
            assert!(shared.try_lock().is_ok());
            assert!(workspace.try_lock().is_ok());
            assert!(store.try_lock().is_ok());
            Ok(())
        })
        .unwrap();
        let key = frame.frame().key().source;
        let mut oracle = Engine::from_document(original.clone());
        oracle
            .edit(
                "agent",
                &commands(&original),
                Some(0),
                None,
                "Stored review",
            )
            .unwrap();
        let expected = oracle
            .resident_state()
            .unwrap()
            .document
            .preview(None, 19, None, false)
            .unwrap()
            .2;
        assert_eq!(
            &*frame.frame().copy_rgba8(&mut || Ok(())).unwrap(),
            &expected
        );
        let reply = accept_stored(
            &shared,
            "human",
            &id,
            &original.id,
            0,
            &EditControl::default(),
        )
        .unwrap();
        assert_eq!(reply.data()["revision"], 1);
        assert!(store.lock().unwrap().slots.is_empty());
        let mut engine = shared.lock().unwrap();
        assert_eq!(engine.proposals_state()[0]["status"], "accepted");
        assert_eq!(engine.native_document().unwrap().key(), key);
        assert_eq!(engine.history_lengths(), [1, 0]);
        assert_eq!(engine.history_entries().next().unwrap().actor(), "agent");
        assert!(engine.proposals[0].native.is_none());
        drop(engine);
        assert!(accept_stored(
            &shared,
            "human",
            &id,
            &original.id,
            1,
            &EditControl::default()
        )
        .is_err());
        assert_eq!(
            &*frame.frame().copy_rgba8(&mut || Ok(())).unwrap(),
            &expected
        );
    }
}

#[test]
fn engine_terminal_routes_only_invalidate_and_retirement_runs_after_unlocking() {
    for depth in [8, 16] {
        for terminal in [
            "reject", "expire", "task", "takeover", "edit", "clear", "close",
        ] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let (shared, workspace, original) = project(&runtime, depth);
            let task = ["task", "takeover"].contains(&terminal).then(|| {
                shared
                    .lock()
                    .unwrap()
                    .reserve(
                        "agent",
                        "Propose",
                        vec![engine::Scope::layer(&original.layers[0].id)],
                    )
                    .unwrap()
                    .id
            });
            let before = runtime.disk().used().unwrap();
            let id = create(&shared, &original, task.as_deref());
            let store = store(&workspace);
            let (signal, weak_session) = {
                let state = store.lock().unwrap();
                let slot = &state.slots[0];
                let Payload::Held(owner) = &slot.payload else {
                    panic!("Stored source expected");
                };
                let held = owner.lock().unwrap();
                (
                    slot.signal.clone(),
                    Arc::downgrade(&held.as_ref().unwrap().session),
                )
            };
            let held = runtime.disk().used().unwrap();
            let mut human_added = 0;
            if terminal == "edit" {
                crate::server::edit_shared(&shared,"human",&[json!({"op":"paint.fill","layer":original.layers[0].id,"rect":[12,2,16,6],"color":[101,53,17,199]})],Some(0),None,"Human work",None,Some(&original.id)).unwrap();
                human_added = runtime.disk().used().unwrap() - held;
                shared.lock().unwrap().expire();
            } else if terminal == "close" {
                let project = shared.lock().unwrap().project_id.clone();
                workspace::close_in(&workspace, &project, &original.id, 0, true, "human").unwrap();
            } else {
                let mut engine = shared.lock().unwrap();
                let _workspace_guard = workspace.lock().unwrap();
                match terminal {
                    "reject" => {
                        engine.reject_proposal("human", &id).unwrap();
                    }
                    "expire" => {
                        engine.proposals[0].expires = 0;
                        engine.expire();
                    }
                    "task" => {
                        engine.finish_task(task.as_ref().unwrap(), "ended");
                        engine.expire();
                    }
                    "takeover" => engine.take_over_tasks(),
                    "clear" => engine.proposals.clear(),
                    _ => unreachable!(),
                }
                assert_eq!(runtime.disk().used().unwrap(), held, "{terminal}");
                assert!(weak_session.upgrade().is_some());
            }
            assert!(!signal.live.load(Ordering::Acquire), "{terminal}");
            assert_eq!(runtime.disk().used().unwrap(), held + human_added);
            let current = {
                let e = shared.lock().unwrap();
                (
                    e.history_lengths(),
                    engine::view::current(&e).id().to_owned(),
                    engine::view::current(&e).revision(),
                )
            };
            maintain(&store).unwrap();
            assert!(weak_session.upgrade().is_none(), "{terminal}");
            assert_eq!(
                runtime.disk().used().unwrap(),
                before + human_added,
                "{terminal}"
            );
            assert!(store.lock().unwrap().slots.is_empty());
            let e = shared.lock().unwrap();
            assert_eq!(
                (
                    e.history_lengths(),
                    engine::view::current(&e).id().to_owned(),
                    engine::view::current(&e).revision()
                ),
                current
            );
        }
    }
}

#[test]
fn readers_and_retiring_sources_keep_their_slots_until_actual_cleanup_succeeds() {
    for depth in [8, 16] {
        let fixture = Fixture::new();
        let (runtime, _connection) = fixture.runtime();
        let (shared, workspace, original) = project(&runtime, depth);
        let ids = (0..MAX_PENDING)
            .map(|_| create(&shared, &original, None))
            .collect::<Vec<_>>();
        let store = store(&workspace);
        assert!(create_stored(
            &shared,
            "agent",
            &commands(&original),
            &original.id,
            0,
            "Full",
            None
        )
        .unwrap_err()
        .contains("Four"));
        let (_, approval, owner) = observed(&shared, &ids[0]).unwrap();
        let (reader, weak_session) = {
            let held = owner.lock().unwrap();
            let draft = held.as_ref().unwrap();
            (
                draft.prepared.preview().clone(),
                Arc::downgrade(&draft.session),
            )
        };
        let held = runtime.disk().used().unwrap();
        shared
            .lock()
            .unwrap()
            .reject_proposal("human", &ids[0])
            .unwrap();
        assert!(approval.active().is_err());
        assert!(maintain(&store).is_err());
        assert_eq!(store.lock().unwrap().slots.len(), MAX_PENDING);
        assert!(create_stored(
            &shared,
            "agent",
            &commands(&original),
            &original.id,
            0,
            "Still full",
            None
        )
        .is_err());
        drop(owner);
        assert!(maintain(&store).is_err());
        assert_eq!(runtime.disk().used().unwrap(), held);
        assert!(weak_session.upgrade().is_some());
        assert!(store
            .lock()
            .unwrap()
            .slots
            .iter()
            .any(|slot| matches!(slot.payload, Payload::Retiring(_))));
        drop(reader);
        maintain(&store).unwrap();
        assert!(weak_session.upgrade().is_none());
        assert_eq!(store.lock().unwrap().slots.len(), MAX_PENDING - 1);
        for _ in 0..20 {
            let id = create(&shared, &original, None);
            shared
                .lock()
                .unwrap()
                .reject_proposal("human", &id)
                .unwrap();
            maintain(&store).unwrap();
            assert!(shared.lock().unwrap().proposals.len() <= MAX_RECORDS);
            assert_eq!(store.lock().unwrap().slots.len(), MAX_PENDING - 1);
        }
    }
}

#[test]
fn the_final_stored_record_fence_rechecks_rejection_expiry_and_replacement() {
    for depth in [8, 16] {
        for action in ["reject", "expire", "replace"] {
            let fixture = Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let (shared, workspace, original) = project(&runtime, depth);
            let id = create(&shared, &original, None);
            let (store, approval, owner) = observed(&shared, &id).unwrap();
            let mut held = owner.lock().unwrap();
            let draft = held.take().unwrap();
            let base = draft.prepared.base_key();
            let checks = std::cell::Cell::new(0);
            let refused = draft
                .accept_guarded(
                    &shared,
                    "human",
                    &original.id,
                    0,
                    &EditControl::default(),
                    &approval,
                    &|| {
                        assert!(shared.try_lock().is_ok());
                        assert!(workspace.try_lock().is_ok());
                        checks.set(checks.get() + 1);
                        if checks.get() == 3 {
                            match action {
                                "reject" => {
                                    shared
                                        .lock()
                                        .unwrap()
                                        .reject_proposal("human", &id)
                                        .unwrap();
                                }
                                "expire" => {
                                    shared.lock().unwrap().proposals[0].expires = 0;
                                }
                                "replace" => {
                                    let next = create(&shared, &original, None);
                                    let retired = {
                                        let mut e = shared.lock().unwrap();
                                        let index =
                                            e.proposals.iter().position(|p| p.id == id).unwrap();
                                        let retired = e.proposals.remove(index);
                                        e.proposals.iter_mut().find(|p| p.id == next).unwrap().id =
                                            id.clone();
                                        retired
                                    };
                                    drop(retired);
                                }
                                _ => unreachable!(),
                            }
                        }
                        Ok(())
                    },
                )
                .err()
                .expect("Stopped or replaced approval must not commit");
            assert!(checks.get() >= 3);
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
            assert_eq!(
                shared.lock().unwrap().native_document().unwrap().key(),
                base
            );
            *held = Some(refused.into_draft());
            drop(held);
            drop(owner);
            maintain(&store).unwrap();
            if action == "replace" {
                assert_eq!(shared.lock().unwrap().proposals[0].status, "pending");
                review_stored(&shared, &id, None, 19, None, false, None, &|| Ok(())).unwrap();
            } else {
                assert!(store.lock().unwrap().slots.is_empty());
            }
        }
    }
}

#[test]
fn preparing_slots_and_timer_expiry_do_not_depend_on_engine_observation() {
    let fixture = Fixture::new();
    let (runtime, _connection) = fixture.runtime();
    let (shared, workspace, original) = project(&runtime, 16);
    let (store, seed) = configured(&shared).unwrap();
    let ticket = reserve(store.clone(), &seed).unwrap();
    ticket.signal.live.store(false, Ordering::Release);
    assert!(maintain(&store).is_err());
    assert_eq!(store.lock().unwrap().slots.len(), 1);
    drop(ticket);
    maintain(&store).unwrap();
    assert!(store.lock().unwrap().slots.is_empty());
    let id = create(&shared, &original, None);
    let (_, approval, owner) = observed(&shared, &id).unwrap();
    approval.signal.expires.store(0, Ordering::Release);
    assert!(review_stored(&shared, &id, None, 19, None, false, None, &|| Ok(())).is_err());
    assert!(approval.validate(&mut shared.lock().unwrap()).is_err());
    shared
        .lock()
        .unwrap()
        .reject_proposal("human", &id)
        .unwrap();
    drop(owner);
    maintain_workspace(&workspace);
    assert!(store.lock().unwrap().slots.is_empty());
}

#[test]
fn the_instance_worker_releases_pending_proposal_sessions_before_writer_drain() {
    for depth in [8, 16] {
        for close in [false, true] {
            let fixture = Fixture::new();
            let (runtime, connection) = fixture.runtime();
            let workspace = connection
                .instance_lock
                .as_ref()
                .unwrap()
                .source_admission()
                .workspace()
                .unwrap();
            let shared = workspace::handles_in(&workspace).remove(0);
            let native = NativeDocument::new_blank(
                19,
                13,
                depth,
                &runtime.resident(),
                &runtime.disk(),
                &|| Ok(()),
            )
            .unwrap();
            let authority = engine::authority::DocumentAuthority::from_native(native, 8).unwrap();
            let retired = {
                let mut e = shared.lock().unwrap();
                std::mem::replace(&mut e.authority, authority)
            };
            drop(retired);
            workspace.lock().unwrap().source_runtime = Some(runtime.clone());
            let (document, layer) = {
                let e = shared.lock().unwrap();
                let view = engine::view::current(&e);
                (view.id().to_owned(), view.layer(0).unwrap().id().to_owned())
            };
            let command =
                json!({"op":"paint.fill","layer":layer,"rect":[4,3,10,9],"color":[31,73,149,177]});
            create_stored(
                &shared,
                "agent",
                &[command],
                &document,
                0,
                "Pending at shutdown",
                None,
            )
            .unwrap();
            let store = store(&workspace);
            assert_eq!(store.lock().unwrap().slots.len(), 1);
            assert_eq!(
                workspace
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire),
                0
            );
            let active = std::cell::Cell::new(false);
            let id = shared.lock().unwrap().proposals[0].id.clone();
            review_stored(&shared, &id, None, 19, None, false, None, &|| {
                if workspace
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire)
                    > 0
                {
                    active.set(true);
                }
                Ok(())
            })
            .unwrap();
            assert!(active.get());
            assert_eq!(
                workspace
                    .lock()
                    .unwrap()
                    .file_operations
                    .load(Ordering::Acquire),
                0
            );
            if close {
                let project = shared.lock().unwrap().project_id.clone();
                workspace::close_in(&workspace, &project, &document, 0, true, "human").unwrap();
            }
            let (finished, done) = std::sync::mpsc::channel();
            let stopping = std::thread::spawn(move || {
                drop(connection);
                finished.send(()).unwrap();
            });
            done.recv_timeout(std::time::Duration::from_secs(5))
                .expect("Pending proposal sessions must not deadlock instance drain");
            stopping.join().unwrap();
            assert!(store.lock().unwrap().slots.is_empty());
            assert!(store.lock().unwrap().closed);
            assert!(workspace.lock().unwrap().native_proposals.is_none());
            assert!(workspace.lock().unwrap().native_proposals_closing);
            if close {
                assert!(shared.lock().unwrap().proposals.is_empty());
            } else {
                assert_eq!(shared.lock().unwrap().proposals[0].status, "interrupted");
            }
            assert_eq!(shared.lock().unwrap().history_lengths(), [0, 0]);
            assert!(configured(&shared).is_err());
        }
    }
}

#[test]
fn parking_requires_unique_source_ownership_and_keeps_the_actual_instance_admission() {
    let fixture = Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let workspace = connection
        .instance_lock
        .as_ref()
        .unwrap()
        .source_admission()
        .workspace()
        .unwrap();
    let activity = workspace.lock().unwrap().file_operations.clone();
    let mut session = Arc::new(runtime.begin_foreground().unwrap());
    assert_eq!(activity.load(Ordering::Acquire), 1);
    assert!(runtime.try_checkpoint().unwrap().is_none());
    let other = session.clone();
    assert!(SourceSession::park_activity(&mut session).is_err());
    assert_eq!(activity.load(Ordering::Acquire), 1);
    drop(other);
    let child = session.retain_for_child().unwrap();
    assert!(SourceSession::park_activity(&mut session).is_err());
    assert_eq!(activity.load(Ordering::Acquire), 1);
    drop(child);
    SourceSession::park_activity(&mut session).unwrap();
    assert_eq!(activity.load(Ordering::Acquire), 0);
    assert!(runtime.try_checkpoint().unwrap().is_some());
    let working = session.activity();
    let foreground = session.foreground_activity().unwrap();
    assert_eq!(activity.load(Ordering::Acquire), 1);
    assert!(runtime.try_checkpoint().unwrap().is_none());
    drop(foreground);
    drop(working);
    assert_eq!(activity.load(Ordering::Acquire), 0);
    let (finished, done) = std::sync::mpsc::channel();
    let stopping = std::thread::spawn(move || {
        drop(connection);
        finished.send(()).unwrap();
    });
    assert!(matches!(
        done.recv_timeout(std::time::Duration::from_millis(25)),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout)
    ));
    drop(session);
    done.recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    stopping.join().unwrap();
}
