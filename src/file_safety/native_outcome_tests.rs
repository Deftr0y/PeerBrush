//! Actual completed-save/profile/instance components for isolated CI execution;
//! the direct Control fence below is not the production Engine transition.
use super::*;
use crate::{file_safety::NativeLease, psd::save36::with_session_candidate};

#[test]
fn actual_native_new_destination_requires_durable_creation_and_keeps_all_debt_on_abort() {
    for depth in [8, 16] {
        with_session_candidate(depth, |session, candidate, runtime, root, _| {
            let path = root.join("new-source.psb");
            let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
            let inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()))
                .unwrap();
            drop(observed);
            let mut writer =
                NativeNewDestination::create(session, candidate, &inventory, &intent, &|| Ok(()))
                    .unwrap();
            // The exact creation owner must be durably registered before replay.
            assert!(writer.creation.is_some());
            let temporary = root.join(intent.temporary_name());
            let temporary_id = NativeExternalFile::observe_held(&writer.temporary)
                .unwrap()
                .identity();
            assert_eq!(fs::metadata(&temporary).unwrap().len(), 0);
            writer.replay(candidate, &|| Ok(())).unwrap();
            writer.ready_to_arm(&|| Ok(())).unwrap();
            let receipt = writer.abort(&|| Ok(())).unwrap();
            receipt
                .validate(
                    intent.sha256(),
                    receipt.core.plan.target_binding(),
                    receipt.core.plan.growth(),
                )
                .unwrap();
            assert!(receipt.retired().is_empty());
            assert!(receipt
                .facts()
                .iter()
                .any(|f| f.identity() == temporary_id && f.bytes() == candidate.bytes()));
            assert!(!path.exists());
            assert_eq!(fs::metadata(temporary).unwrap().len(), candidate.bytes());
            assert!(runtime.disk().used().unwrap() >= candidate.bytes());
            intent.abort(&receipt, &|| Ok(())).unwrap();
            let next_observation = inventory.observe_external(&|| Ok(())).unwrap();
            assert!(session
                .prepare_publication(candidate, &next_observation, &|| Ok(()))
                .is_err());
            drop(next_observation);
            drop(receipt);
            drop(inventory);
            drop(lease);
        });
    }
}

#[test]
fn actual_native_new_destination_no_clobber_publication_returns_exact_live_identity() {
    for depth in [8, 16] {
        with_session_candidate(depth, |session, candidate, runtime, root, _| {
            let path = root.join("new-source.psb");
            let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
            let inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()))
                .unwrap();
            drop(observed);
            let mut writer =
                NativeNewDestination::create(session, candidate, &inventory, &intent, &|| Ok(()))
                    .unwrap();
            let temporary = root.join(intent.temporary_name());
            let id = super::super::identity(&writer.temporary).unwrap();
            writer.replay(candidate, &|| Ok(())).unwrap();
            writer.ready_to_arm(&|| Ok(())).unwrap();
            let identity = candidate.scene().identity();
            let control = crate::saving::Control::default();
            control
                .begin(identity.document_id.clone(), identity.revision)
                .unwrap();
            let armed = intent.prearm(candidate, &control, &|| Ok(())).unwrap();
            let committed = armed.commit(&control).unwrap();
            let receipt = writer.publish(&committed, &|| Ok(())).unwrap();
            let version = lease
                .verified_version(&path, candidate.bytes(), candidate.sha256(), || Ok(()))
                .unwrap();
            assert_eq!(receipt.version(), version);
            assert_eq!(version.identity, id);
            assert!(!temporary.exists());
            assert!(receipt.retired().is_empty());
            assert!(receipt
                .facts()
                .iter()
                .any(|f| f.identity() == id && f.bytes() == candidate.bytes()));
            receipt
                .validate(
                    committed.intent_sha256(),
                    committed.target_binding(),
                    committed.growth(),
                )
                .unwrap();
            assert!(!control.cancel());
            // The journal consumes this permit after its own closed record API;
            // the receipt deliberately does not borrow the permit or Session.
            committed.finish(&receipt, &|| Ok(())).unwrap();
            drop(receipt);
            drop(inventory);
            drop(lease);
        });
    }
}

#[test]
fn actual_native_foreign_target_and_temporary_replacements_are_never_overwritten() {
    for replace_temporary in [false, true] {
        with_session_candidate(16, |session, candidate, runtime, root, _| {
            let path = root.join("new-source.psb");
            let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
            let inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()))
                .unwrap();
            drop(observed);
            let mut writer =
                NativeNewDestination::create(session, candidate, &inventory, &intent, &|| Ok(()))
                    .unwrap();
            let temporary = root.join(intent.temporary_name());
            let displaced = root.join("held-original-temporary");
            let foreign = if replace_temporary {
                fs::rename(&temporary, &displaced).unwrap();
                temporary.clone()
            } else {
                path.clone()
            };
            fs::write(&foreign, b"foreign replacement must survive").unwrap();
            assert!(writer.replay(candidate, &|| Ok(())).is_err());
            assert_eq!(
                fs::read(&foreign).unwrap(),
                b"foreign replacement must survive"
            );
            assert!(writer.abort(&|| Ok(())).is_err());
            if replace_temporary {
                assert!(displaced.exists());
            }
            drop(intent);
            drop(inventory);
            drop(lease);
        });
    }
}

#[test]
fn actual_native_replay_cancellation_is_latched_and_keeps_prefix_as_charged_debt() {
    with_session_candidate(16, |session, candidate, runtime, root, _| {
        let path = root.join("new-source.psb");
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()))
            .unwrap();
        drop(observed);
        let mut writer =
            NativeNewDestination::create(session, candidate, &inventory, &intent, &|| Ok(()))
                .unwrap();
        let temporary = root.join(intent.temporary_name());
        let error = writer
            .replay(candidate, &|| {
                if fs::metadata(&temporary).unwrap().len() > 0 {
                    Err("exact native replay cancellation".into())
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
        assert_eq!(error, "exact native replay cancellation");
        assert!(writer.replay(candidate, &|| Ok(())).is_err());
        assert!(writer.ready_to_arm(&|| Ok(())).is_err());
        let receipt = writer.abort(&|| Ok(())).unwrap();
        assert!(receipt.retired().is_empty());
        assert!(fs::metadata(&temporary).unwrap().len() > 0);
        assert!(!path.exists());
        let debt = runtime.disk().used().unwrap();
        assert!(intent.abort(&receipt, &|| Ok(())).is_err());
        assert!(runtime.disk().used().unwrap() >= debt);
        drop(receipt);
        drop(inventory);
        drop(lease);
    });
}

#[test]
fn actual_native_committed_new_target_finishes_after_closing_without_new_source_admission() {
    crate::psd::save36::with_session_candidate_join(
        16,
        |session, candidate, runtime, root, connection| {
            let path = root.join("new-source.psb");
            let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
            let inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()))
                .unwrap();
            drop(observed);
            let mut writer =
                NativeNewDestination::create(session, candidate, &inventory, &intent, &|| Ok(()))
                    .unwrap();
            writer.replay(candidate, &|| Ok(())).unwrap();
            writer.ready_to_arm(&|| Ok(())).unwrap();
            let identity = candidate.scene().identity();
            let control = crate::saving::Control::default();
            control
                .begin(identity.document_id.clone(), identity.revision)
                .unwrap();
            let committed = intent
                .prearm(candidate, &control, &|| Ok(()))
                .unwrap()
                .commit(&control)
                .unwrap();
            let connection = connection.take().unwrap();
            let (done, finished) = std::sync::mpsc::channel();
            let closer = std::thread::spawn(move || {
                drop(connection);
                done.send(()).unwrap();
            });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while session.check().is_ok() && std::time::Instant::now() < deadline {
                std::thread::yield_now();
            }
            assert!(session.check().is_err());
            assert!(runtime.begin().is_err());
            assert!(matches!(
                finished.try_recv(),
                Err(std::sync::mpsc::TryRecvError::Empty)
            ));
            let receipt = writer.publish(&committed, &|| Ok(())).unwrap();
            assert_eq!(
                lease
                    .verified_version(&path, candidate.bytes(), candidate.sha256(), || Ok(()))
                    .unwrap(),
                receipt.version()
            );
            committed.finish(&receipt, &|| Ok(())).unwrap();
            drop(receipt);
            drop(inventory);
            drop(lease);
            ((), Some(closer))
        },
    );
}

#[test]
fn actual_native_late_foreign_replacement_prevents_receipt_settlement_without_cleanup() {
    with_session_candidate(8, |session, candidate, runtime, root, _| {
        let path = root.join("new-source.psb");
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()))
            .unwrap();
        drop(observed);
        let mut writer =
            NativeNewDestination::create(session, candidate, &inventory, &intent, &|| Ok(()))
                .unwrap();
        writer.replay(candidate, &|| Ok(())).unwrap();
        writer.ready_to_arm(&|| Ok(())).unwrap();
        let identity = candidate.scene().identity();
        let control = crate::saving::Control::default();
        control
            .begin(identity.document_id.clone(), identity.revision)
            .unwrap();
        let committed = intent
            .prearm(candidate, &control, &|| Ok(()))
            .unwrap()
            .commit(&control)
            .unwrap();
        let receipt = writer.publish(&committed, &|| Ok(())).unwrap();
        let displaced = root.join("retained-published-owner");
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, b"late foreign edit survives").unwrap();
        assert!(receipt
            .validate(
                committed.intent_sha256(),
                committed.target_binding(),
                committed.growth()
            )
            .is_err());
        let debt = runtime.disk().used().unwrap();
        assert!(committed.finish(&receipt, &|| Ok(())).is_err());
        assert!(runtime.disk().used().unwrap() >= debt);
        assert_eq!(fs::read(&path).unwrap(), b"late foreign edit survives");
        assert_eq!(fs::metadata(displaced).unwrap().len(), candidate.bytes());
        drop(receipt);
        drop(inventory);
        drop(lease);
    });
}

#[test]
fn actual_native_existing_target_refuses_new_target_driver_before_external_staging() {
    with_session_candidate(16, |session, candidate, runtime, root, _| {
        let path = root.join("existing-source.psb");
        fs::write(&path, b"original version stays exactly").unwrap();
        let lease = NativeLease::acquire(&path, || Ok(())).unwrap();
        let inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()))
            .unwrap();
        drop(observed);
        let temporary = root.join(intent.temporary_name());
        let backup = root
            .join(".peerbrush-versions")
            .join(intent.backup_name().unwrap());
        assert!(
            NativeNewDestination::create(session, candidate, &inventory, &intent, &|| Ok(()))
                .is_err()
        );
        assert_eq!(fs::read(&path).unwrap(), b"original version stays exactly");
        assert!(!temporary.exists());
        assert!(!backup.exists());
        inventory.validate(&|| Ok(())).unwrap();
        drop(intent);
        drop(inventory);
        drop(lease);
    });
}
