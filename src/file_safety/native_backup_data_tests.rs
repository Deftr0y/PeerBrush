//! Real source-session copies; these do not authorize existing-file replacement.
use super::*;
use crate::{
    file_safety::NativeLease, large_document::VERIFIED_BATCH_BYTES as TRANSFER,
    psd::save36::with_session_candidate,
};
use std::cell::Cell;

fn original() -> Vec<u8> {
    (0..(TRANSFER * 2 + 173))
        .map(|at| (at * 29 + 17) as u8)
        .collect()
}

#[test]
fn registered_backup_retains_exact_bytes_and_transfers_once_into_frozen_inventory() {
    for depth in [8, 16] {
        with_session_candidate(depth, |session, candidate, runtime, root, _| {
            let path = root.join("existing-source.psb");
            let old = original();
            fs::write(&path, &old).unwrap();
            let lease = NativeLease::acquire_admitted(
                session.admit_destination(&path, &|| Ok(())).unwrap(),
                || Ok(()),
            )
            .unwrap();
            let mut inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let before = inventory.external_bytes().unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
                .unwrap();
            drop(observed);
            let backup =
                NativeBackupData::prepare(session, candidate, &mut inventory, &intent, &|| Ok(()))
                    .unwrap();
            let backup_path = backup
                .plan
                .backup_directory()
                .join(backup.plan.backup_name().unwrap());
            assert_eq!(fs::read(&path).unwrap(), old);
            assert_eq!(fs::read(&backup_path).unwrap(), old);
            assert_ne!(backup.fact.identity(), lease.expected().unwrap().identity);
            backup
                ._creation
                .check_plan(
                    &backup.plan,
                    session,
                    ExternalCreationRole::Backup,
                    backup.fact,
                )
                .unwrap();
            backup.recheck(&|| Ok(())).unwrap();
            inventory.validate(&|| Ok(())).unwrap();
            assert_eq!(
                inventory.external_bytes().unwrap(),
                before + old.len() as u64
            );
            assert!(inventory
                .retain_created_backup(&backup, &|| Ok(()))
                .is_err());
            assert_eq!(
                inventory.external_bytes().unwrap(),
                before + old.len() as u64
            );
            assert_eq!(
                inventory
                    .writer_facts(&|| Ok(()))
                    .unwrap()
                    .iter()
                    .filter(|f| f.identity() == backup.fact.identity())
                    .count(),
                1
            );
            // Losing the local copy owner does not remove its file or refund debt.
            let debt = runtime.disk().used().unwrap();
            drop(backup);
            assert_eq!(fs::read(&backup_path).unwrap(), old);
            assert!(runtime.disk().used().unwrap() >= debt);
            inventory.validate(&|| Ok(())).unwrap();
        });
    }
}

#[test]
fn cancellation_keeps_empty_or_partial_backup_and_original_without_refunding_admission() {
    for threshold in [0, TRANSFER as u64] {
        with_session_candidate(16, |session, candidate, runtime, root, _| {
            let path = root.join("existing-source.psb");
            let old = original();
            fs::write(&path, &old).unwrap();
            let lease = NativeLease::acquire_admitted(
                session.admit_destination(&path, &|| Ok(())).unwrap(),
                || Ok(()),
            )
            .unwrap();
            let mut inventory = lease
                .freeze_inventory(&runtime.resident(), &|| Ok(()))
                .unwrap();
            let before = inventory.external_bytes().unwrap();
            let observed = inventory.observe_external(&|| Ok(())).unwrap();
            let intent = session
                .prepare_publication(candidate, &observed, &|| Ok(()), &mut |_| Ok(()))
                .unwrap();
            drop(observed);
            let backup_path = root
                .join(".peerbrush-versions")
                .join(intent.backup_name().unwrap());
            let canceled = Cell::new(false);
            let debt = runtime.disk().used().unwrap();
            let result =
                NativeBackupData::prepare(session, candidate, &mut inventory, &intent, &|| {
                    if fs::metadata(&backup_path).is_ok_and(|m| m.len() >= threshold) {
                        canceled.set(true);
                        Err("Requested backup cancellation".into())
                    } else {
                        Ok(())
                    }
                });
            assert!(result.is_err());
            assert!(canceled.get());
            assert_eq!(fs::read(&path).unwrap(), old);
            let prefix = fs::read(&backup_path).unwrap();
            assert_eq!(prefix.len() as u64, threshold);
            assert_eq!(prefix, old[..prefix.len()]);
            assert!(runtime.disk().used().unwrap() >= debt);
            assert_eq!(inventory.external_bytes().unwrap(), before);
            // The original frozen set cannot silently absorb an uncertain prefix.
            assert!(inventory.validate(&|| Ok(())).is_err());
        });
    }
}

#[test]
fn foreign_backup_name_replacement_survives_failed_copy_without_cleanup() {
    with_session_candidate(8, |session, candidate, runtime, root, _| {
        let path = root.join("existing-source.psb");
        let old = original();
        fs::write(&path, &old).unwrap();
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
        let retained = root.join(".peerbrush-versions/retained-prefix");
        let replaced = Cell::new(false);
        let debt = runtime.disk().used().unwrap();
        assert!(
            NativeBackupData::prepare(session, candidate, &mut inventory, &intent, &|| {
                if !replaced.get()
                    && fs::metadata(&backup_path).is_ok_and(|m| m.len() >= TRANSFER as u64)
                {
                    fs::rename(&backup_path, &retained).unwrap();
                    fs::write(&backup_path, b"foreign backup survives").unwrap();
                    replaced.set(true);
                }
                Ok(())
            })
            .is_err()
        );
        assert!(replaced.get());
        assert_eq!(fs::read(&path).unwrap(), old);
        assert_eq!(fs::read(&backup_path).unwrap(), b"foreign backup survives");
        assert_eq!(fs::read(retained).unwrap(), old[..TRANSFER]);
        assert!(runtime.disk().used().unwrap() >= debt);
    });
}

#[test]
fn same_stamp_source_change_in_copied_prefix_fails_final_full_source_hash() {
    with_session_candidate(16, |session, candidate, runtime, root, _| {
        let path = root.join("existing-source.psb");
        let old = original();
        fs::write(&path, &old).unwrap();
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
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
        let changed = Cell::new(false);
        let debt = runtime.disk().used().unwrap();
        assert!(
            NativeBackupData::prepare(session, candidate, &mut inventory, &intent, &|| {
                if !changed.get()
                    && fs::metadata(&backup_path).is_ok_and(|m| m.len() >= TRANSFER as u64)
                {
                    let mut source = fs::OpenOptions::new().write(true).open(&path).unwrap();
                    source.write_all(&[old[0] ^ 0xff]).unwrap();
                    source.sync_all().unwrap();
                    source
                        .set_times(fs::FileTimes::new().set_modified(modified))
                        .unwrap();
                    drop(source);
                    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
                    changed.set(true);
                }
                Ok(())
            })
            .is_err()
        );
        assert!(changed.get());
        let mut changed_bytes = old.clone();
        changed_bytes[0] ^= 0xff;
        assert_eq!(fs::read(&path).unwrap(), changed_bytes);
        assert_eq!(fs::read(&backup_path).unwrap(), old);
        assert!(runtime.disk().used().unwrap() >= debt);
        assert!(inventory.validate(&|| Ok(())).is_err());
    });
}
