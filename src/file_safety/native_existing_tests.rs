//! Actual candidate/session/backup publication. Direct Control finalization here
//! does not substitute for the production Engine source/save fence.
use super::*;
use crate::{file_safety::NativeLease, psd::save36::with_session_candidate};

#[cfg(unix)]
#[test]
fn extended_attribute_preflight_refuses_before_journaling_and_preserves_its_value() {
    use std::os::fd::AsRawFd;
    with_session_candidate(16, |session, _candidate, runtime, root, _| {
        let path = root.join("existing.psb");
        fs::write(&path, b"original data").unwrap();
        let source = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        #[cfg(target_os = "linux")]
        let name = b"user.peerbrush-test\0";
        #[cfg(target_os = "macos")]
        let name = b"com.peerbrush-test\0";
        let value = b"preserve metadata";
        #[cfg(target_os = "linux")]
        let set = unsafe {
            libc::fsetxattr(
                source.as_raw_fd(),
                name.as_ptr().cast(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        };
        #[cfg(target_os = "macos")]
        let set = unsafe {
            libc::fsetxattr(
                source.as_raw_fd(),
                name.as_ptr().cast(),
                value.as_ptr().cast(),
                value.len(),
                0,
                0,
            )
        };
        assert_eq!(set, 0);
        let lease = NativeLease::acquire_admitted(
            session.admit_destination(&path, &|| Ok(())).unwrap(),
            || Ok(()),
        )
        .unwrap();
        let debt = runtime.disk().admitted().unwrap();
        assert!(
            NativeExistingDestination::preflight(&lease, &runtime.resident(), &|| Ok(())).is_err()
        );
        assert_eq!(runtime.disk().admitted().unwrap(), debt);
        assert_eq!(fs::read(&path).unwrap(), b"original data");
        let mut restored = [0u8; 17];
        #[cfg(target_os = "linux")]
        let count = unsafe {
            libc::fgetxattr(
                source.as_raw_fd(),
                name.as_ptr().cast(),
                restored.as_mut_ptr().cast(),
                restored.len(),
            )
        };
        #[cfg(target_os = "macos")]
        let count = unsafe {
            libc::fgetxattr(
                source.as_raw_fd(),
                name.as_ptr().cast(),
                restored.as_mut_ptr().cast(),
                restored.len(),
                0,
                0,
            )
        };
        assert_eq!(count, value.len() as isize);
        assert_eq!(&restored, value);
    });
}

#[test]
fn existing_publication_keeps_the_original_physical_owner_and_closes_exact_debt() {
    for depth in [8, 16] {
        with_session_candidate(depth, |session, candidate, runtime, root, _| {
            let path = root.join("existing.psb");
            let old = b"original document stays intact";
            fs::write(&path, old).unwrap();
            let lease = NativeLease::acquire_admitted(
                session.admit_destination(&path, &|| Ok(())).unwrap(),
                || Ok(()),
            )
            .unwrap();
            let original = lease.expected().unwrap();
            let mut inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
                .unwrap();
            drop(observed);
            let backup = root
                .join(".peerbrush-versions")
                .join(intent.backup_name().unwrap());
            let temporary = root.join(intent.temporary_name());
            let mut writer = NativeExistingDestination::create(
                session,
                candidate,
                &mut inventory,
                &intent,
                &|| Ok(()),
            )
            .unwrap();
            let retained = root
                .join(".peerbrush-versions")
                .join(writer.staged.plan.retained_original_name().unwrap());
            writer.replay(candidate, &|| Ok(()), &mut |_| Ok(())).unwrap();
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
            let version = receipt.version();
            assert_ne!(version.identity, original.identity);
            assert_eq!(
                version,
                lease
                    .verified_version(&path, candidate.bytes(), candidate.sha256(), || Ok(()))
                    .unwrap()
            );
            let named = crate::file_safety::open_native_identity(&retained).unwrap();
            assert_eq!(
                crate::file_safety::identity(&named).unwrap(),
                original.identity
            );
            assert_eq!(
                named
                    .metadata()
                    .unwrap()
                    .modified()
                    .unwrap()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                original.modified_nanos()
            );
            drop(named);
            assert_eq!(fs::read(&retained).unwrap(), old);
            assert_eq!(fs::read(&backup).unwrap(), old);
            assert!(!temporary.exists());
            assert_eq!(receipt.facts().len(), 4);
            assert!(receipt.retired().is_empty());
            receipt
                .validate(
                    committed.intent_sha256(),
                    committed.target_binding(),
                    committed.growth(),
                )
                .unwrap();
            committed
                .finish(&receipt, &|| session.cleanup_check())
                .unwrap();
            let debt = runtime.disk().admitted().unwrap();
            drop(receipt);
            assert_eq!(runtime.disk().admitted().unwrap(), debt);
            assert_eq!(
                lease
                    .verified_version(&path, candidate.bytes(), candidate.sha256(), || Ok(()))
                    .unwrap(),
                version
            );
            assert_eq!(fs::read(&retained).unwrap(), old);
        });
    }
}

#[test]
fn late_foreign_destination_keeps_original_backup_candidate_and_full_pending_debt() {
    use std::cell::Cell;
    with_session_candidate(16, |session, candidate, runtime, root, _| {
        let path = root.join("existing.psb");
        fs::write(&path, b"original stays intact").unwrap();
        let lease = NativeLease::acquire_admitted(
            session.admit_destination(&path, &|| Ok(())).unwrap(),
            || Ok(()),
        )
        .unwrap();
        let mut inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
            .unwrap();
        drop(observed);
        let temporary = root.join(intent.temporary_name());
        let backup = root
            .join(".peerbrush-versions")
            .join(intent.backup_name().unwrap());
        let mut writer =
            NativeExistingDestination::create(session, candidate, &mut inventory, &intent, &|| {
                Ok(())
            })
            .unwrap();
        let retained = root
            .join(".peerbrush-versions")
            .join(writer.staged.plan.retained_original_name().unwrap());
        writer.replay(candidate, &|| Ok(()), &mut |_| Ok(())).unwrap();
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
        let debt = runtime.disk().admitted().unwrap();
        let inserted = Cell::new(false);
        assert!(writer
            .publish(&committed, &|| {
                if !inserted.get() && retained.exists() && !path.exists() {
                    fs::write(&path, b"late human document survives").unwrap();
                    inserted.set(true);
                }
                Ok(())
            })
            .is_err());
        assert!(inserted.get());
        assert_eq!(fs::read(&path).unwrap(), b"late human document survives");
        assert_eq!(fs::read(&retained).unwrap(), b"original stays intact");
        assert_eq!(fs::read(&backup).unwrap(), b"original stays intact");
        assert_eq!(fs::metadata(&temporary).unwrap().len(), candidate.bytes());
        assert!(runtime.disk().admitted().unwrap() >= debt);
    });
}

#[test]
fn late_published_target_replacement_refuses_settlement_and_keeps_every_version() {
    with_session_candidate(8, |session, candidate, runtime, root, _| {
        let path = root.join("existing.psb");
        fs::write(&path, b"original stays intact").unwrap();
        let lease = NativeLease::acquire_admitted(
            session.admit_destination(&path, &|| Ok(())).unwrap(),
            || Ok(()),
        )
        .unwrap();
        let mut inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
            .unwrap();
        drop(observed);
        let backup = root
            .join(".peerbrush-versions")
            .join(intent.backup_name().unwrap());
        let mut writer =
            NativeExistingDestination::create(session, candidate, &mut inventory, &intent, &|| {
                Ok(())
            })
            .unwrap();
        let retained = root
            .join(".peerbrush-versions")
            .join(writer.staged.plan.retained_original_name().unwrap());
        writer.replay(candidate, &|| Ok(()), &mut |_| Ok(())).unwrap();
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
        let displaced = root.join("retained-published-candidate.psb");
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, b"new human document survives").unwrap();
        let debt = runtime.disk().admitted().unwrap();
        assert!(committed
            .finish(&receipt, &|| session.cleanup_check())
            .is_err());
        assert!(runtime.disk().admitted().unwrap() >= debt);
        assert_eq!(fs::read(&path).unwrap(), b"new human document survives");
        assert_eq!(fs::metadata(&displaced).unwrap().len(), candidate.bytes());
        assert_eq!(fs::read(&retained).unwrap(), b"original stays intact");
        assert_eq!(fs::read(&backup).unwrap(), b"original stays intact");
    });
}

#[test]
fn existing_staging_registers_both_creations_and_aborts_with_the_original_unchanged() {
    for depth in [8, 16] {
        with_session_candidate(depth, |session, candidate, runtime, root, _| {
            let path = root.join("existing.psb");
            let old = b"original document and native samples";
            fs::write(&path, old).unwrap();
            let lease = NativeLease::acquire_admitted(
                session.admit_destination(&path, &|| Ok(())).unwrap(),
                || Ok(()),
            )
            .unwrap();
            let mut inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
                .unwrap();
            drop(observed);
            let backup_path = root
                .join(".peerbrush-versions")
                .join(intent.backup_name().unwrap());
            let temporary = root.join(intent.temporary_name());
            let mut writer = NativeExistingDestination::create(
                session,
                candidate,
                &mut inventory,
                &intent,
                &|| Ok(()),
            )
            .unwrap();
            assert_eq!(fs::read(&path).unwrap(), old);
            assert_eq!(fs::read(&backup_path).unwrap(), old);
            assert_eq!(fs::metadata(&temporary).unwrap().len(), 0);
            writer
                .staged
                .creation
                .as_ref()
                .unwrap()
                .check_plan(
                    &writer.staged.plan,
                    session,
                    ExternalCreationRole::Temporary,
                    NativeExternalFile::observe_held(&writer.staged.temporary).unwrap(),
                )
                .unwrap();
            writer.replay(candidate, &|| Ok(()), &mut |_| Ok(())).unwrap();
            writer.ready_to_arm(&|| Ok(())).unwrap();
            let receipt = writer.abort(&|| Ok(())).unwrap();
            receipt
                .validate(
                    intent.sha256(),
                    receipt.staged.core.plan.target_binding(),
                    receipt.staged.core.plan.growth(),
                )
                .unwrap();
            assert_eq!(receipt.facts().len(), 4); // Original, backup, complete candidate, quota owner.
            assert!(receipt.retired().is_empty());
            assert!(receipt
                .facts()
                .iter()
                .any(|f| f.identity() == lease.expected().unwrap().identity));
            intent.abort(&receipt, &|| Ok(())).unwrap();
            assert_eq!(fs::read(&path).unwrap(), old);
            assert_eq!(fs::read(&backup_path).unwrap(), old);
            assert_eq!(fs::metadata(&temporary).unwrap().len(), candidate.bytes());
            let debt = runtime.disk().admitted().unwrap();
            let facts = receipt.facts().to_vec();
            drop(receipt);
            assert_eq!(runtime.disk().admitted().unwrap(), debt);
            for path in [&path, &backup_path, &temporary] {
                let named = crate::file_safety::open_native_identity(path).unwrap();
                assert!(facts.contains(&NativeExternalFile::observe_held(&named).unwrap()));
            }
            inventory.validate(&|| Ok(())).unwrap();
        });
    }
}

#[test]
fn cancellation_before_replay_retains_the_registered_empty_temporary_and_all_debt() {
    with_session_candidate(16, |session, candidate, runtime, root, _| {
        let path = root.join("existing.psb");
        fs::write(&path, b"keep original").unwrap();
        let lease = NativeLease::acquire_admitted(
            session.admit_destination(&path, &|| Ok(())).unwrap(),
            || Ok(()),
        )
        .unwrap();
        let mut inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
            .unwrap();
        drop(observed);
        let temporary = root.join(intent.temporary_name());
        let backup = root
            .join(".peerbrush-versions")
            .join(intent.backup_name().unwrap());
        let mut writer =
            NativeExistingDestination::create(session, candidate, &mut inventory, &intent, &|| {
                Ok(())
            })
            .unwrap();
        let debt = runtime.disk().admitted().unwrap();
        assert!(writer
            .replay(candidate, &|| Err("cancel before replay".into()), &mut |_| Ok(()))
            .is_err());
        assert!(writer.ready_to_arm(&|| Ok(())).is_err());
        let receipt = writer.abort(&|| session.cleanup_check()).unwrap();
        // An incomplete candidate cannot settle the admitted growth.
        assert!(intent.abort(&receipt, &|| session.cleanup_check()).is_err());
        assert!(runtime.disk().admitted().unwrap() >= debt);
        assert_eq!(fs::metadata(&temporary).unwrap().len(), 0);
        assert_eq!(fs::read(&path).unwrap(), b"keep original");
        assert_eq!(fs::read(&backup).unwrap(), b"keep original");
    });
}

#[test]
fn foreign_original_replacement_refuses_replay_and_abort_without_removing_either_version() {
    with_session_candidate(8, |session, candidate, runtime, root, _| {
        let path = root.join("existing.psb");
        fs::write(&path, b"old original").unwrap();
        let lease = NativeLease::acquire_admitted(
            session.admit_destination(&path, &|| Ok(())).unwrap(),
            || Ok(()),
        )
        .unwrap();
        let mut inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
            .unwrap();
        drop(observed);
        let backup = root
            .join(".peerbrush-versions")
            .join(intent.backup_name().unwrap());
        let temporary = root.join(intent.temporary_name());
        let mut writer =
            NativeExistingDestination::create(session, candidate, &mut inventory, &intent, &|| {
                Ok(())
            })
            .unwrap();
        let debt = runtime.disk().admitted().unwrap();
        let displaced = root.join("retained-human-original.psb");
        fs::rename(&path, &displaced).unwrap();
        fs::write(&path, b"foreign document survives").unwrap();
        assert!(writer.replay(candidate, &|| Ok(()), &mut |_| Ok(())).is_err());
        assert!(writer.abort(&|| session.cleanup_check()).is_err());
        assert!(runtime.disk().admitted().unwrap() >= debt);
        assert_eq!(fs::read(&path).unwrap(), b"foreign document survives");
        assert_eq!(fs::read(&displaced).unwrap(), b"old original");
        assert_eq!(fs::read(&backup).unwrap(), b"old original");
        assert!(temporary.exists());
    });
}

#[test]
fn same_stamp_backup_corruption_cannot_mint_a_settleable_abort_receipt() {
    use std::io::{Seek, SeekFrom};
    with_session_candidate(16, |session, candidate, runtime, root, _| {
        let path = root.join("existing.psb");
        fs::write(&path, b"old original").unwrap();
        let lease = NativeLease::acquire_admitted(
            session.admit_destination(&path, &|| Ok(())).unwrap(),
            || Ok(()),
        )
        .unwrap();
        let mut inventory = lease
            .freeze_inventory(&runtime.resident(), &|| Ok(()))
            .unwrap();
        let observed = inventory.observe_external(&|| Ok(())).unwrap();
        let intent = session
            .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
            .unwrap();
        drop(observed);
        let writer =
            NativeExistingDestination::create(session, candidate, &mut inventory, &intent, &|| {
                Ok(())
            })
            .unwrap();
        let (_, file, _) = writer.backup.inventory_binding();
        let modified = file.metadata().unwrap().modified().unwrap();
        let mut file = file;
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(b"X").unwrap();
        file.sync_all().unwrap();
        file.set_times(fs::FileTimes::new().set_modified(modified))
            .unwrap();
        assert!(writer.abort(&|| session.cleanup_check()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old original");
    });
}
