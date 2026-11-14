use super::*;
use crate::large_document::{root::PersistentRootContext, TileSpec};
use crate::workspace::source_runtime::tests::{limits, Fixture};

fn source(session: &SourceSession, depth: u16, commit: bool) -> ([u8; 16], PathBuf, Vec<u8>) {
    let store = session.store_for_lineage([0x73; 16]).unwrap();
    let spec = TileSpec::new(256, 256, depth).unwrap();
    let reference = store
        .put(spec, &vec![47; spec.bytes()], &mut || Ok(()))
        .unwrap();
    if commit {
        drop(
            session
                .seal_store(&store, &[reference], b"parked source")
                .unwrap(),
        );
    }
    let id = store.identity();
    let path = session
        .runtime
        .inventory
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .locations[&id]
        .path
        .clone();
    let original = store.file_bytes_for_test().unwrap();
    (id, path, original)
}

#[test]
#[cfg(windows)]
fn complete_parked_sources_release_only_their_actual_file_and_directory_admission() {
    for depth in [8, 16] {
        for commit in [false, true] {
            let f = Fixture::new();
            let (_engine, connection) = f.connection();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let runtime = SourceRuntime::new(context, limits()).unwrap();
            let session = runtime.begin().unwrap();
            let disk = runtime.disk().used().unwrap();
            let resident = runtime.resident().used();
            let (id, path, original) = source(&session, depth, commit);
            let charged = runtime.disk().used().unwrap();
            drop(session);
            let collected = runtime
                .begin_existing()
                .unwrap()
                .collect_sources(&|| Ok(()))
                .unwrap();
            assert_eq!(collected.sources, 1);
            assert_eq!(collected.bytes, original.len() as u64);
            assert_eq!(collected.bytes, charged - disk);
            assert_eq!(runtime.disk().used().unwrap(), disk);
            assert_eq!(runtime.resident().used(), resident);
            assert!(!path.exists());
            let state = runtime.inventory.lock().unwrap();
            let inventory = state.as_ref().unwrap();
            assert!(!inventory.locations.contains_key(&id));
            assert!(!inventory.lineages.values().any(|source| *source == id));
            assert_eq!(inventory.directories, 0);
            drop(state);
            let session = runtime.begin_existing().unwrap();
            let next = session.store_for_lineage([0x73; 16]).unwrap();
            assert_ne!(next.identity(), id);
        }
    }
}

#[test]
#[cfg(windows)]
fn surface_owners_readers_and_retained_children_prevent_collection() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    let session = runtime.begin().unwrap();
    let (id, path, original) = source(&session, 16, true);
    let store = session.store_for_lineage([0x73; 16]).unwrap();
    let selected = store.checkpoint_snapshot(&mut || Ok(())).unwrap().unwrap();
    let mut refs = std::collections::BTreeMap::new();
    selected
        .visit_blocks(&mut || Ok(()), |reference| {
            refs.insert((0, 0), reference);
            Ok(())
        })
        .unwrap();
    let surface = crate::raster::Surface::from_plane(
        crate::raster::backed::Plane::from_blocks(256, 256, 16, store.clone(), refs).unwrap(),
    )
    .unwrap();
    let mut history = Some(surface.clone());
    let mut clipboard = Some(surface.clone());
    let mut preview = Some(surface.clone());
    drop((surface, selected, store));
    let child = session.retain_for_child().unwrap();
    assert!(runtime
        .begin_existing()
        .unwrap()
        .collect_sources(&|| Ok(()))
        .is_err());
    drop(session);
    assert!(runtime
        .begin_existing()
        .unwrap()
        .collect_sources(&|| Ok(()))
        .is_err());
    drop(child);
    let charged = runtime.disk().used().unwrap();
    for owner in [&mut history, &mut clipboard, &mut preview] {
        assert_eq!(
            runtime
                .begin_existing()
                .unwrap()
                .collect_sources(&|| Ok(()))
                .unwrap()
                .sources,
            0
        );
        let session = runtime.begin_existing().unwrap();
        let store = session.store_for_lineage([0x73; 16]).unwrap();
        assert_eq!(store.identity(), id);
        assert_eq!(store.file_bytes_for_test().unwrap(), original);
        assert_eq!(runtime.disk().used().unwrap(), charged);
        drop((store, session));
        drop(owner.take());
    }
    assert_eq!(
        runtime
            .begin_existing()
            .unwrap()
            .collect_sources(&|| Ok(()))
            .unwrap()
            .sources,
        1
    );
    assert!(!path.exists());
}

#[test]
#[cfg(windows)]
fn preattempt_refusals_restore_custody_bytes_and_admission_for_retry() {
    for refusal in ["cancel", "quota", "foreign", "unknown"] {
        let f = Fixture::new();
        let (_engine, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        let session = runtime.begin().unwrap();
        let (_, path, original) = source(&session, 16, true);
        drop(session);
        let charged = runtime.disk().used().unwrap();
        let foreign = if refusal == "foreign" {
            path.join("human.txt")
        } else {
            path.parent()
                .unwrap()
                .join(format!("pending-{}", uuid::Uuid::new_v4()))
        };
        if matches!(refusal, "foreign" | "unknown") {
            fs::write(&foreign, b"preserve").unwrap();
        }
        let ballast = (refusal == "quota").then(|| {
            runtime
                .resident()
                .reserve_bytes(
                    runtime.resident().limit() - runtime.resident().used(),
                    runtime.resident().limit(),
                )
                .unwrap()
        });
        let calls = Cell::new(0);
        let refused = runtime.begin_existing().unwrap().collect_sources(&|| {
            calls.set(calls.get() + 1);
            if refusal == "cancel" && calls.get() == 4 {
                Err("cancel after claim".into())
            } else {
                Ok(())
            }
        });
        assert!(refused.is_err());
        drop(ballast);
        assert!(!runtime.tainted.load(Ordering::Acquire));
        assert_eq!(runtime.disk().used().unwrap(), charged);
        let session = runtime.begin_existing().unwrap();
        let store = session.store_for_lineage([0x73; 16]).unwrap();
        assert_eq!(store.file_bytes_for_test().unwrap(), original);
        drop((store, session));
        if matches!(refusal, "foreign" | "unknown") {
            assert_eq!(fs::read(&foreign).unwrap(), b"preserve");
            fs::remove_file(&foreign).unwrap();
        }
        assert_eq!(
            runtime
                .begin_existing()
                .unwrap()
                .collect_sources(&|| Ok(()))
                .unwrap()
                .sources,
            1
        );
    }
}

#[test]
#[cfg(windows)]
fn uncertain_unlink_directory_or_settlement_retains_debt_and_blocks_fresh_admission() {
    for cold in [false, true] {
        for fault in [
            CollectionFault::File,
            CollectionFault::Directory,
            CollectionFault::Settlement,
            CollectionFault::PanicFile,
        ] {
            let f = Fixture::new();
            let (_engine, connection) = f.connection();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let mut runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
            let session = runtime.begin().unwrap();
            source(&session, 16, true);
            drop(session);
            if cold {
                drop(runtime);
                runtime = SourceRuntime::new(context, limits()).unwrap();
                drop(runtime.begin_existing().unwrap());
            }
            let charged = runtime.disk().used().unwrap();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                runtime
                    .begin_existing()
                    .unwrap()
                    .collect_sources_with_fault(&|| Ok(()), fault)
            }));
            if fault == CollectionFault::PanicFile {
                assert!(result.is_err());
            } else {
                assert!(result.unwrap().is_err());
            }
            assert!(runtime.tainted.load(Ordering::Acquire));
            assert_eq!(runtime.disk().used().unwrap(), charged);
            assert!(runtime.begin_existing().is_err());
        }
    }
}

#[test]
fn startup_retirement_requires_complete_physical_inspection_and_never_admits_a_store() {
    for depth in [8, 16] {
        for kind in ["empty", "tiles", "commits", "tail"] {
            let f = Fixture::new();
            let (_engine, connection) = f.connection();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
            let session = runtime.begin().unwrap();
            let store = session.store_for_lineage([0x73; 16]).unwrap();
            let id = store.identity();
            let spec = TileSpec::new(256, 256, depth).unwrap();
            if kind != "empty" {
                let first = store
                    .put(spec, &vec![47; spec.bytes()], &mut || Ok(()))
                    .unwrap();
                if matches!(kind, "commits" | "tail") {
                    drop(
                        session
                            .seal_store(&store, &[first], b"first prefix")
                            .unwrap(),
                    );
                    let second = store
                        .put(spec, &vec![91; spec.bytes()], &mut || Ok(()))
                        .unwrap();
                    if kind == "commits" {
                        drop(
                            session
                                .seal_store(&store, &[second], b"second prefix")
                                .unwrap(),
                        );
                    }
                }
            }
            let path = runtime
                .inventory
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .locations[&id]
                .path
                .clone();
            let original = store.file_bytes_for_test().unwrap();
            drop((store, session, runtime));
            let runtime = SourceRuntime::new(context, limits()).unwrap();
            let session = runtime.begin_existing().unwrap();
            let charged = runtime.disk().used().unwrap();
            {
                let state = runtime.inventory.lock().unwrap();
                let location = &state.as_ref().unwrap().locations[&id];
                assert!(location.weak.is_none());
                assert!(location.selected.is_none());
            }
            let result = session.collect_sources(&|| Ok(()));
            #[cfg(windows)]
            {
                let collected = result.unwrap();
                assert_eq!(collected.sources, 1);
                assert_eq!(collected.bytes, original.len() as u64);
                assert_eq!(runtime.disk().used().unwrap(), charged - collected.bytes);
                assert!(!path.exists());
                assert!(!runtime
                    .inventory
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .locations
                    .contains_key(&id));
            }
            #[cfg(not(windows))]
            {
                assert!(result.is_err());
                assert_eq!(fs::read(path.join("blocks.pbt")).unwrap(), original);
                assert_eq!(runtime.disk().used().unwrap(), charged);
            }
        }
    }
}

#[test]
#[cfg(windows)]
fn refused_startup_inspection_restores_the_exact_observer_without_editable_authority() {
    for refusal in ["cancel", "quota", "corrupt", "partial", "unknown_record"] {
        let f = Fixture::new();
        let (_engine, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let (id, path, original) = source(&session, 16, true);
        drop((session, runtime));
        let runtime = SourceRuntime::new(context, limits()).unwrap();
        drop(runtime.begin_existing().unwrap());
        let mut expected = original;
        if refusal == "corrupt" {
            expected[76] ^= 0xff;
        }
        if refusal == "partial" {
            expected.pop();
        }
        if refusal == "unknown_record" {
            expected[24..28].copy_from_slice(b"WHAT");
        }
        if matches!(refusal, "corrupt" | "partial" | "unknown_record") {
            // External changes precede the held observation in a fresh inventory.
            drop(runtime);
            fs::write(path.join("blocks.pbt"), &expected).unwrap();
            let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
            let runtime = SourceRuntime::new(context, limits()).unwrap();
            drop(runtime.begin_existing().unwrap());
            assert_startup_refusal(&runtime, id, &path, &expected, refusal);
        } else {
            assert_startup_refusal(&runtime, id, &path, &expected, refusal);
        }
    }
}
#[cfg(windows)]
fn assert_startup_refusal(
    runtime: &Arc<SourceRuntime>,
    id: [u8; 16],
    path: &Path,
    expected: &[u8],
    refusal: &str,
) {
    let (identity, observed_bytes, modified, at) = {
        let state = runtime.inventory.lock().unwrap();
        let inventory = state.as_ref().unwrap();
        let at = inventory
            .files
            .iter()
            .position(|held| held.path == path.join("blocks.pbt"))
            .unwrap();
        let held = &inventory.files[at];
        (held.identity, held.bytes, held.modified, at)
    };
    let charged = runtime.disk().used().unwrap();
    let resident = runtime.resident().used();
    let ballast = (refusal == "quota").then(|| {
        runtime
            .resident()
            .reserve_bytes(
                runtime.resident().limit() - resident - 32768,
                runtime.resident().limit(),
            )
            .unwrap()
    });
    let calls = Cell::new(0);
    assert!(runtime
        .begin_existing()
        .unwrap()
        .collect_sources(&|| {
            calls.set(calls.get() + 1);
            if refusal == "cancel" && calls.get() == 5 {
                Err("cancel startup scan".into())
            } else {
                Ok(())
            }
        })
        .is_err());
    drop(ballast);
    assert!(!runtime.tainted.load(Ordering::Acquire));
    assert_eq!(runtime.disk().used().unwrap(), charged);
    assert_eq!(runtime.resident().used(), resident);
    assert_eq!(fs::read(path.join("blocks.pbt")).unwrap(), expected);
    let state = runtime.inventory.lock().unwrap();
    let inventory = state.as_ref().unwrap();
    let held = &inventory.files[at];
    assert_eq!(held.path, path.join("blocks.pbt"));
    assert_eq!(
        (held.identity, held.bytes, held.modified),
        (identity, observed_bytes, modified)
    );
    let location = &inventory.locations[&id];
    assert!(location.weak.is_none());
    assert!(location.selected.is_none());
}

#[test]
#[cfg(windows)]
fn startup_collection_preserves_a_source_owner_that_outlives_its_old_runtime() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
    let session = runtime.begin().unwrap();
    let (_, path, original) = source(&session, 16, true);
    let live = session.store_for_lineage([0x73; 16]).unwrap();
    drop((session, runtime));
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    // An old logical source owns both its data handle and OS lock. Depending on
    // the filesystem, initial inventory or subsequent inspection refuses it.
    if let Ok(session) = runtime.begin_existing() {
        assert!(session.collect_sources(&|| Ok(())).is_err());
    }
    assert_eq!(live.file_bytes_for_test().unwrap(), original);
    assert!(path.join("blocks.pbt").exists());
}

#[test]
#[cfg(windows)]
fn foreground_admission_interrupts_warm_and_startup_scans_without_dropping_the_request() {
    for cold in [false, true] {
        let f = Fixture::new();
        let (_engine, connection) = f.connection();
        let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
        let mut runtime = SourceRuntime::new(context.clone(), limits()).unwrap();
        let session = runtime.begin().unwrap();
        let (_, path, original) = source(&session, 16, true);
        drop(session);
        if cold {
            drop(runtime);
            runtime = SourceRuntime::new(context, limits()).unwrap();
            drop(runtime.begin_existing().unwrap());
        }
        let charged = runtime.disk().used().unwrap();
        let calls = Cell::new(0);
        let worker = RefCell::new(None);
        let (send, receive) = std::sync::mpsc::channel();
        let result = runtime.begin_existing().unwrap().collect_sources(&|| {
            calls.set(calls.get() + 1);
            if calls.get() == 5 {
                let foreground_runtime = runtime.clone();
                let send = send.clone();
                *worker.borrow_mut() = Some(std::thread::spawn(move || {
                    send.send(
                        foreground_runtime
                            .begin_existing()
                            .and_then(|session| session.cleanup_check()),
                    )
                    .unwrap();
                }));
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
                while runtime.maintenance.check().is_ok() && std::time::Instant::now() < deadline {
                    std::thread::yield_now();
                }
                assert!(runtime.maintenance.check().is_err());
            }
            Ok(())
        });
        assert!(result.is_err());
        receive
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .unwrap();
        worker.borrow_mut().take().unwrap().join().unwrap();
        assert!(!runtime.tainted.load(Ordering::Acquire));
        assert_eq!(runtime.disk().used().unwrap(), charged);
        let session = runtime.begin_existing().unwrap();
        assert_eq!(
            session
                .owned_file_bytes_for_test(&path.join("blocks.pbt"))
                .unwrap(),
            original
        );
    }
}

#[test]
#[cfg(windows)]
fn foreground_yield_after_unlink_finishes_that_retirement_without_tainting_the_service() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    let session = runtime.begin().unwrap();
    let (id, path, original) = source(&session, 16, true);
    drop(session);
    let before = runtime.disk().used().unwrap();
    assert!(runtime
        .begin_existing()
        .unwrap()
        .collect_sources_with_fault(&|| Ok(()), CollectionFault::YieldFile)
        .is_err());
    assert!(!runtime.tainted.load(Ordering::Acquire));
    assert_eq!(
        runtime.disk().used().unwrap(),
        before - original.len() as u64
    );
    assert!(!path.exists());
    assert!(!runtime
        .inventory
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .locations
        .contains_key(&id));
    runtime.begin_existing().unwrap().cleanup_check().unwrap();
}

#[test]
#[cfg(windows)]
fn source_sessions_woken_together_complete_admission_instead_of_losing_inventory_contention() {
    let f = Fixture::new();
    let (_engine, connection) = f.connection();
    let context = PersistentRootContext::from_explicit_state(&connection).unwrap();
    let runtime = SourceRuntime::new(context, limits()).unwrap();
    let mut session = runtime.begin().unwrap();
    let maintenance = runtime.maintenance.start().unwrap();
    let exclusive = session.access.take().unwrap().exclusive().unwrap();
    let (send, receive) = std::sync::mpsc::channel();
    let barrier = Arc::new(std::sync::Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let runtime = runtime.clone();
            let send = send.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                assert!(send.send(runtime.begin_existing()).is_ok());
            })
        })
        .collect();
    barrier.wait();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while runtime.maintenance.check().is_ok() && std::time::Instant::now() < deadline {
        std::thread::yield_now();
    }
    assert!(runtime.maintenance.check().is_err());
    drop(exclusive);
    drop(maintenance);
    let first = receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
    let second = receive
        .recv_timeout(std::time::Duration::from_secs(2))
        .unwrap()
        .unwrap();
    for thread in threads {
        thread.join().unwrap();
    }
    drop((first, second, session));
    runtime.begin_existing().unwrap().cleanup_check().unwrap();
}
