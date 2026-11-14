use super::*;
use crate::engine::{
    authority::DocumentAuthority,
    native::{worker, CommandDescriptor, Direction, Publication},
    scene::{CapturedScene, LegacyTileCache},
    Document, Engine,
};
use serde_json::json;

fn gate(actor: &str) -> Publication<'_> {
    Publication {
        actor,
        leases: &[],
        now: 0,
        closed: false,
        write_available: true,
    }
}
fn capture(
    authority: &DocumentAuthority,
    commands: &[serde_json::Value],
) -> crate::engine::authority::NativeCapture {
    let doc = authority.native().unwrap();
    let descriptor =
        CommandDescriptor::from_normalized(doc, doc.metadata().revision(), commands, &|| Ok(()))
            .unwrap();
    authority
        .capture_native(descriptor, "agent", &[], 0, false, true)
        .unwrap()
}

#[test]
fn held_review_preserves_native_sources_until_discard_or_one_undoable_acceptance() {
    for depth in [8, 16] {
        for terminal in ["discard", "accept", "discard-acceptance"] {
            let fixture = worker::tests::Fixture::new();
            let (runtime, connection) = fixture.runtime();
            let pool = runtime.resident();
            let cache = LegacyTileCache::new(&pool, 32).unwrap();
            let mut original = Document::new_depth(19, 13, depth).unwrap();
            original.layers[0]
                .pixels
                .set16(2, 3, [12347, 23459, 34571, 61379]);
            let id = original.layers[0].id.clone();
            let mut staged =
                CapturedScene::from_parts(original.clone(), "proposal".into(), vec![], None, None)
                    .stage(&runtime, &cache, &|| Ok(()))
                    .unwrap();
            let native =
                NativeDocument::from_pending(staged.scene.into_provisional_pending().unwrap())
                    .unwrap();
            let session = Arc::new(staged.session.take().unwrap());
            let mut authority = DocumentAuthority::from_native(native, 8).unwrap();
            let disk_before = runtime.disk().used().unwrap();
            let commands =
                [json!({"op":"paint.fill","layer":id,"rect":[4,3,10,9],"color":[31,73,149,177]})];
            let prepared =
                worker::prepare(capture(&authority, &commands), &session, &|| Ok(())).unwrap();
            let scene = Arc::new(
                authority
                    .capture_scene(None, "proposal".into(), vec![], None, None)
                    .unwrap()
                    .into_native()
                    .unwrap(),
            );
            let weak_scene = Arc::downgrade(&scene);
            let draft = Draft::new(
                scene,
                prepared,
                session.clone(),
                "agent",
                "Held proposal",
                None,
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason));
            let mut oracle = Engine::new();
            oracle
                .resident_state_mut()
                .expect("resident fixture")
                .document = original.clone();
            oracle
                .edit("agent", &commands, None, None, "Held proposal")
                .unwrap();
            let expected = oracle
                .resident_state()
                .expect("resident fixture")
                .document
                .preview(None, 48, None, false)
                .unwrap()
                .2;
            let baseline_key = authority.native().unwrap().key();
            let mut last_frame = None;
            for _ in 0..3 {
                let rendered = draft
                    .render(
                        authority.native().unwrap(),
                        None,
                        48,
                        None,
                        false,
                        None,
                        &|| Ok(()),
                    )
                    .unwrap();
                let (_, frame, supplement) = rendered.into_parts();
                drop(supplement);
                assert_eq!(&*frame.copy_rgba8(&mut || Ok(())).unwrap(), &expected);
                last_frame = Some(frame);
                assert_eq!(authority.history_lengths(), [0, 0]);
                assert_eq!(authority.native().unwrap().key(), baseline_key);
            }
            // A refused acceptance returns the same candidate for review/retry.
            let draft = draft
                .prepare_accept("agent", authority.native().unwrap(), &|| Ok(()))
                .err()
                .unwrap()
                .into_draft();
            let held = pool
                .reserve_bytes(pool.limit() - pool.used(), pool.limit())
                .unwrap();
            let refused = draft
                .prepare_accept("human", authority.native().unwrap(), &|| Ok(()))
                .err()
                .unwrap();
            drop(held);
            let draft = refused.into_draft();
            assert!(draft
                .render(
                    authority.native().unwrap(),
                    None,
                    48,
                    None,
                    false,
                    None,
                    &|| Ok(())
                )
                .is_ok());
            if terminal == "discard" {
                draft.discard().unwrap();
                assert_eq!(runtime.disk().used().unwrap(), disk_before);
            } else {
                let acceptance = draft
                    .prepare_accept("human", authority.native().unwrap(), &|| Ok(()))
                    .unwrap_or_else(|refused| panic!("{}", refused.reason));
                if terminal == "discard-acceptance" {
                    acceptance.discard().unwrap();
                    assert_eq!(runtime.disk().used().unwrap(), disk_before);
                } else {
                    let (pending, held_session) = acceptance.into_parts();
                    authority
                        .commit_native(gate("agent"), pending)
                        .unwrap()
                        .retire(&*held_session, &|| held_session.cleanup_check())
                        .unwrap();
                    assert_eq!(authority.history_lengths(), [1, 0]);
                    let entry = authority.history_entries().next().unwrap();
                    assert_eq!(entry.actor(), "agent");
                    assert_eq!(entry.label(), "Held proposal");
                    let undo = authority
                        .native_history()
                        .unwrap()
                        .prepare_move(
                            authority.native().unwrap(),
                            Direction::Undo,
                            "human",
                            &|| Ok(()),
                        )
                        .unwrap();
                    drop(authority.move_native(gate("human"), undo).unwrap());
                    assert_eq!(authority.history_lengths(), [0, 1]);
                    let doc = authority.native().unwrap();
                    let slot = doc.layer_slot(&id).unwrap();
                    let pixels = doc
                        .source(slot)
                        .unwrap()
                        .prepare_region(
                            crate::raster::backed::Region::new([2, 3, 3, 4]).unwrap(),
                            &mut || Ok(()),
                        )
                        .unwrap();
                    assert_eq!(
                        pixels.pixel16(2, 3).unwrap(),
                        original.layers[0].pixels.get16(2, 3)
                    );
                }
            }
            assert!(weak_scene.upgrade().is_none());
            assert_eq!(
                &*last_frame.unwrap().copy_rgba8(&mut || Ok(())).unwrap(),
                &expected
            );
            drop(connection);
        }
    }
}

#[test]
fn failed_draft_construction_keeps_the_session_for_explicit_retirement() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    let session = Arc::new(runtime.begin().unwrap());
    let document =
        NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
            .unwrap();
    let authority = DocumentAuthority::from_native(document, 4).unwrap();
    let layer = authority.view().layer(0).unwrap().id().to_owned();
    let before = runtime.disk().used().unwrap();
    let prepared = worker::prepare(
        capture(
            &authority,
            &[json!({"op":"paint.fill","layer":layer,"rect":[0,0,4,2],"color":[31,73,149,177]})],
        ),
        &session,
        &|| Ok(()),
    )
    .unwrap();
    let scene = Arc::new(
        authority
            .capture_scene(None, "proposal".into(), vec![], None, None)
            .unwrap()
            .into_native()
            .unwrap(),
    );
    let weak_session = Arc::downgrade(&session);
    let refused = Draft::new(
        scene,
        prepared,
        session,
        "another-agent",
        "Refused",
        None,
        &|| Ok(()),
    )
    .err()
    .unwrap();
    assert!(weak_session.upgrade().is_some());
    refused.discard().unwrap();
    assert!(weak_session.upgrade().is_none());
    assert_eq!(runtime.disk().used().unwrap(), before);
    assert_eq!(authority.history_lengths(), [0, 0]);
    drop(connection);
}

#[test]
fn stale_and_cancelled_reviews_preserve_owned_sources_until_explicit_discard() {
    let fixture = worker::tests::Fixture::new();
    let (runtime, connection) = fixture.runtime();
    for cancelled in [false, true] {
        let session = Arc::new(runtime.begin().unwrap());
        let doc =
            NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let authority = DocumentAuthority::from_native(doc.clone(), 4).unwrap();
        let id = authority.view().layer(0).unwrap().id().to_owned();
        let before = runtime.disk().used().unwrap();
        let captured = capture(
            &authority,
            &[json!({"op":"paint.fill","layer":id,"rect":[0,0,4,2],"color":[31,73,149,177]})],
        );
        let cancellation = captured.cancellation();
        let prepared = worker::prepare(captured, &session, &|| Ok(())).unwrap();
        let scene = Arc::new(
            authority
                .capture_scene(None, "proposal".into(), vec![], None, None)
                .unwrap()
                .into_native()
                .unwrap(),
        );
        let draft = Draft::new(
            scene,
            prepared,
            session,
            "agent",
            "Review",
            None,
            &|| Ok(()),
        )
        .unwrap_or_else(|refused| panic!("{}", refused.reason));
        let other =
            NativeDocument::new_blank(4, 2, 16, &runtime.resident(), &runtime.disk(), &|| Ok(()))
                .unwrap();
        let current = if cancelled {
            cancellation.cancel();
            &doc
        } else {
            &other
        };
        assert!(draft
            .render(current, None, 48, None, false, None, &|| Ok(()))
            .is_err());
        let refused = draft
            .prepare_accept("human", current, &|| Ok(()))
            .err()
            .unwrap();
        assert_eq!(authority.history_lengths(), [0, 0]);
        refused.discard().unwrap();
        assert_eq!(runtime.disk().used().unwrap(), before);
    }
    drop(connection);
}

#[test]
fn proposal_retirement_returns_its_sources_and_session_for_an_explicit_retry() {
    for depth in [8, 16] {
        for fault in ["reader", "cancel", "acceptance", "debt"] {
            let fixture = worker::tests::Fixture::new();
            let (runtime, _connection) = fixture.runtime();
            let session = Arc::new(runtime.begin().unwrap());
            let document = NativeDocument::new_blank(
                19,
                13,
                depth,
                &runtime.resident(),
                &runtime.disk(),
                &|| Ok(()),
            )
            .unwrap();
            let authority = DocumentAuthority::from_native(document.clone(), 4).unwrap();
            let metadata = serde_json::to_value(document.metadata()).unwrap();
            let layer = authority.view().layer(0).unwrap().id().to_owned();
            let commands = [
                json!({"op":"paint.fill","layer":layer,"rect":[0,0,6,4],"color":[17,79,151,191]}),
                json!({"op":"paint.fill","layer":layer,"rect":[8,7,15,11],"color":[113,71,29,233]}),
            ];
            let disk_before = runtime.disk().used().unwrap();
            let prepared =
                worker::prepare(capture(&authority, &commands), &session, &|| Ok(())).unwrap();
            let reader = (fault == "reader").then(|| prepared.preview().clone());
            let scene = Arc::new(
                authority
                    .capture_scene(None, "proposal".into(), vec![], None, None)
                    .unwrap()
                    .into_native()
                    .unwrap(),
            );
            let weak_scene = Arc::downgrade(&scene);
            let weak_session = Arc::downgrade(&session);
            let draft = Draft::new(
                scene,
                prepared,
                session.clone(),
                "agent",
                "Discard with retry",
                None,
                &|| Ok(()),
            )
            .unwrap_or_else(|refused| panic!("{}", refused.reason));
            let frame = draft
                .render(&document, None, 48, None, false, None, &|| Ok(()))
                .unwrap();
            let held_disk = runtime.disk().used().unwrap();
            assert!(held_disk > disk_before);
            let retirement = if fault == "acceptance" {
                draft
                    .prepare_accept("human", &document, &|| Ok(()))
                    .unwrap_or_else(|refused| panic!("{}", refused.reason))
                    .prepare_retirement()
                    .unwrap_or_else(|(_, reason)| panic!("{reason}"))
            } else {
                draft
                    .prepare_retirement()
                    .unwrap_or_else(|refused| panic!("{}", refused.reason))
            };
            assert!(weak_scene.upgrade().is_none());
            drop(session);
            let refused = retirement
                .retire_with(&|| {
                    if fault == "debt" {
                        if runtime.disk().used().unwrap() < held_disk {
                            Err("Stop after source unlink".into())
                        } else {
                            Ok(())
                        }
                    } else if fault == "reader" {
                        Ok(())
                    } else {
                        Err("Pause proposal retirement".into())
                    }
                })
                .err()
                .expect("The retirement owner must survive refusal");
            if fault == "debt" {
                assert_eq!(refused.reason, "Stop after source unlink");
                let retirement = refused.into_retirement();
                assert!(!retirement.retryable());
                let debt = runtime.disk().used().unwrap();
                assert!(debt >= disk_before && debt < held_disk);
                let refused = retirement
                    .retire_with(&|| panic!("Uncertain retirement cannot retry I/O"))
                    .err()
                    .unwrap();
                assert_eq!(refused.reason, "Stop after source unlink");
                assert!(!refused.into_retirement().retryable());
                assert_eq!(runtime.disk().used().unwrap(), debt);
                assert_eq!(authority.history_lengths(), [0, 0]);
                assert_eq!(serde_json::to_value(document.metadata()).unwrap(), metadata);
                continue;
            } else if fault != "reader" {
                assert_eq!(refused.reason, "Pause proposal retirement");
            }
            let retirement = refused.into_retirement();
            assert!(retirement.retryable());
            assert!(weak_session.upgrade().is_some());
            assert_eq!(runtime.disk().used().unwrap(), held_disk);
            assert_eq!(authority.history_lengths(), [0, 0]);
            assert_eq!(serde_json::to_value(document.metadata()).unwrap(), metadata);
            drop(reader);
            retirement
                .retire()
                .unwrap_or_else(|refused| panic!("{}", refused.reason));
            assert_eq!(runtime.disk().used().unwrap(), disk_before);
            assert!(weak_session.upgrade().is_none());
            assert_eq!(
                frame.frame().copy_rgba8(&mut || Ok(())).unwrap().len(),
                19 * 13 * 4
            );
        }
    }
}
